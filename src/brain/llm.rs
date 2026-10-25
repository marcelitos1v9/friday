//! Clientes de LLM com suporte a ferramentas (tool calling).
//!
//! - Groq usa a API compatível com OpenAI (`/chat/completions`).
//! - Ollama usa a API nativa (`/api/chat`), que permite controlar `keep_alive` e assim
//!   tirar o modelo da VRAM durante os jogos.
//!
//! As chamadas são bloqueantes (ureq) e feitas só na thread do cérebro: sem runtime async
//! e sem conexão aberta enquanto ninguém fala com a Sexta-Feira.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::config::{BrainConfig, Provider};

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    System(String),
    User(String),
    Assistant { content: String, tool_calls: Vec<ToolCall> },
    Tool { call_id: String, name: String, content: String },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reply {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
}

pub trait Llm: Send {
    /// `tools` é a lista no formato OpenAI (`[{type: "function", function: {...}}]`).
    fn chat(&mut self, msgs: &[Msg], tools: &Value, game_mode: bool) -> Result<Reply>;

    /// Libera recursos locais (ex.: descarregar o modelo do Ollama). Padrão: nada.
    fn unload(&mut self) {}
}

pub fn from_config(cfg: &BrainConfig) -> Box<dyn Llm> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(cfg.timeout_s)))
        .http_status_as_error(false)
        .build()
        .into();
    match cfg.provider {
        Provider::Groq => {
            let key = if cfg.groq_api_key.trim().is_empty() {
                std::env::var("GROQ_API_KEY").unwrap_or_default()
            } else {
                cfg.groq_api_key.trim().to_string()
            };
            Box::new(OpenAiCompat { agent, base_url: cfg.groq_url.clone(), model: cfg.groq_model.clone(), key })
        }
        Provider::Ollama => Box::new(Ollama {
            agent,
            base_url: cfg.ollama_url.clone(),
            model: cfg.ollama_model.clone(),
            keep_alive: cfg.ollama_keep_alive.clone(),
            unload_in_game: cfg.ollama_unload_in_game,
        }),
    }
}

fn post(agent: &ureq::Agent, url: &str, key: Option<&str>, body: &Value) -> Result<Value> {
    let mut req = agent.post(url);
    if let Some(k) = key {
        req = req.header("Authorization", &format!("Bearer {k}"));
    }
    let mut resp = req.send_json(body).with_context(|| format!("sem conexão com {url}"))?;
    let status = resp.status();
    let text = resp.body_mut().read_to_string().context("resposta ilegível")?;
    if !status.is_success() {
        let detail: String = text.chars().take(300).collect();
        bail!("{url} respondeu {status}: {detail}");
    }
    serde_json::from_str(&text).context("resposta não é JSON")
}

// ---------- OpenAI / Groq ----------

pub struct OpenAiCompat {
    agent: ureq::Agent,
    base_url: String,
    model: String,
    key: String,
}

pub fn openai_messages(msgs: &[Msg]) -> Value {
    Value::Array(
        msgs.iter()
            .map(|m| match m {
                Msg::System(c) => json!({"role": "system", "content": c}),
                Msg::User(c) => json!({"role": "user", "content": c}),
                Msg::Assistant { content, tool_calls } if tool_calls.is_empty() => {
                    json!({"role": "assistant", "content": content})
                }
                Msg::Assistant { content, tool_calls } => json!({
                    "role": "assistant",
                    "content": content,
                    "tool_calls": tool_calls.iter().map(|c| json!({
                        "id": c.id,
                        "type": "function",
                        "function": {"name": c.name, "arguments": c.args.to_string()},
                    })).collect::<Vec<_>>(),
                }),
                Msg::Tool { call_id, content, .. } => json!({"role": "tool", "tool_call_id": call_id, "content": content}),
            })
            .collect(),
    )
}

pub fn parse_openai(resp: &Value) -> Result<Reply> {
    let msg = resp.pointer("/choices/0/message").ok_or_else(|| anyhow!("resposta sem mensagem"))?;
    let content = msg.get("content").and_then(Value::as_str).unwrap_or("").to_string();
    let mut tool_calls = Vec::new();
    for (i, c) in msg.get("tool_calls").and_then(Value::as_array).into_iter().flatten().enumerate() {
        let name = c.pointer("/function/name").and_then(Value::as_str).unwrap_or("").to_string();
        let raw = c.pointer("/function/arguments").cloned().unwrap_or(Value::Null);
        // A API manda os argumentos como string JSON; alguns servidores mandam objeto.
        let args = match raw {
            Value::String(s) if s.trim().is_empty() => json!({}),
            Value::String(s) => serde_json::from_str(&s).unwrap_or(json!({})),
            Value::Null => json!({}),
            v => v,
        };
        let id = c.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("call_{i}"));
        tool_calls.push(ToolCall { id, name, args });
    }
    Ok(Reply { content, tool_calls })
}

impl Llm for OpenAiCompat {
    fn chat(&mut self, msgs: &[Msg], tools: &Value, _game_mode: bool) -> Result<Reply> {
        if self.key.is_empty() {
            bail!("falta a chave da Groq: defina brain.groq_api_key no config.toml ou a variável GROQ_API_KEY");
        }
        let body = json!({
            "model": self.model,
            "messages": openai_messages(msgs),
            "tools": tools,
            "tool_choice": "auto",
            "temperature": 0.4,
            "max_tokens": 600,
        });
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        parse_openai(&post(&self.agent, &url, Some(&self.key), &body)?)
    }
}

// ---------- Ollama ----------

pub struct Ollama {
    agent: ureq::Agent,
    base_url: String,
    model: String,
    keep_alive: String,
    unload_in_game: bool,
}

pub fn ollama_messages(msgs: &[Msg]) -> Value {
    Value::Array(
        msgs.iter()
            .map(|m| match m {
                Msg::System(c) => json!({"role": "system", "content": c}),
                Msg::User(c) => json!({"role": "user", "content": c}),
                Msg::Assistant { content, tool_calls } if tool_calls.is_empty() => {
                    json!({"role": "assistant", "content": content})
                }
                Msg::Assistant { content, tool_calls } => json!({
                    "role": "assistant",
                    "content": content,
                    "tool_calls": tool_calls.iter().map(|c| json!({
                        "function": {"name": c.name, "arguments": c.args},
                    })).collect::<Vec<_>>(),
                }),
                Msg::Tool { name, content, .. } => json!({"role": "tool", "tool_name": name, "content": content}),
            })
            .collect(),
    )
}

pub fn parse_ollama(resp: &Value) -> Result<Reply> {
    let msg = resp.get("message").ok_or_else(|| anyhow!("resposta do Ollama sem mensagem"))?;
    let content = msg.get("content").and_then(Value::as_str).unwrap_or("").to_string();
    let tool_calls = msg
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, c)| ToolCall {
            id: format!("call_{i}"),
            name: c.pointer("/function/name").and_then(Value::as_str).unwrap_or("").to_string(),
            args: match c.pointer("/function/arguments") {
                Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(json!({})),
                Some(v) if v.is_object() => v.clone(),
                _ => json!({}),
            },
        })
        .collect();
    Ok(Reply { content, tool_calls })
}

impl Ollama {
    fn keep_alive(&self, game_mode: bool) -> Value {
        if game_mode && self.unload_in_game {
            json!(0)
        } else {
            json!(self.keep_alive)
        }
    }
}

impl Llm for Ollama {
    fn chat(&mut self, msgs: &[Msg], tools: &Value, game_mode: bool) -> Result<Reply> {
        let body = json!({
            "model": self.model,
            "messages": ollama_messages(msgs),
            "tools": tools,
            "stream": false,
            "keep_alive": self.keep_alive(game_mode),
            "options": {"temperature": 0.4, "num_predict": 600},
        });
        let url = format!("{}/api/chat", self.base_url.trim_end_matches('/'));
        let resp = post(&self.agent, &url, None, &body)
            .map_err(|e| anyhow!("{e:#}. O Ollama está aberto e o modelo {} foi baixado?", self.model))?;
        parse_ollama(&resp)
    }

    fn unload(&mut self) {
        if !self.unload_in_game {
            return;
        }
        let url = format!("{}/api/generate", self.base_url.trim_end_matches('/'));
        match post(&self.agent, &url, None, &json!({"model": self.model, "keep_alive": 0})) {
            Ok(_) => log::info!("modelo {} descarregado da memória para o jogo", self.model),
            Err(e) => log::debug!("não consegui descarregar o modelo: {e:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation() -> Vec<Msg> {
        vec![
            Msg::System("sys".into()),
            Msg::User("volume 30".into()),
            Msg::Assistant {
                content: String::new(),
                tool_calls: vec![ToolCall { id: "c1".into(), name: "volume".into(), args: json!({"acao": "definir", "valor": 30}) }],
            },
            Msg::Tool { call_id: "c1".into(), name: "volume".into(), content: "ok".into() },
        ]
    }

    #[test]
    fn formato_openai() {
        let v = openai_messages(&conversation());
        assert_eq!(v[2]["tool_calls"][0]["function"]["arguments"], "{\"acao\":\"definir\",\"valor\":30}");
        assert_eq!(v[3]["tool_call_id"], "c1");
        let resp = json!({"choices": [{"message": {"content": null, "tool_calls": [
            {"id": "x", "type": "function", "function": {"name": "abrir_app", "arguments": "{\"nome\":\"chrome\"}"}}
        ]}}]});
        let r = parse_openai(&resp).unwrap();
        assert_eq!(r.tool_calls[0].name, "abrir_app");
        assert_eq!(r.tool_calls[0].args["nome"], "chrome");
    }

    /// Servidor HTTP de uma resposta só, para testar a chamada de verdade.
    fn mock_server(body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                let n = sock.read(&mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                        .unwrap_or(0);
                    if req.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            sock.write_all(resp.as_bytes()).unwrap();
            String::from_utf8_lossy(&req).to_string()
        });
        (url, h)
    }

    #[test]
    fn chamada_http_groq() {
        let (url, h) = mock_server(r#"{"choices":[{"message":{"role":"assistant","content":"Oi, chefe."}}]}"#);
        let cfg = BrainConfig { groq_url: url, groq_api_key: "abc".into(), ..Default::default() };
        let r = from_config(&cfg).chat(&[Msg::User("oi".into())], &json!([]), false).unwrap();
        assert_eq!(r.content, "Oi, chefe.");
        let req = h.join().unwrap();
        assert!(req.starts_with("POST /chat/completions"));
        assert!(req.to_lowercase().contains("authorization: bearer abc"));
    }

    #[test]
    fn chamada_http_ollama_no_modo_jogo() {
        let (url, h) = mock_server(r#"{"message":{"role":"assistant","content":"Ok."},"done":true}"#);
        let cfg = BrainConfig { provider: Provider::Ollama, ollama_url: url, ..Default::default() };
        let r = from_config(&cfg).chat(&[Msg::User("oi".into())], &json!([]), true).unwrap();
        assert_eq!(r.content, "Ok.");
        let req = h.join().unwrap();
        assert!(req.starts_with("POST /api/chat"));
        let body: Value = serde_json::from_str(&req[req.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(body["keep_alive"], 0);
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn formato_ollama() {
        let v = ollama_messages(&conversation());
        assert_eq!(v[2]["tool_calls"][0]["function"]["arguments"]["valor"], 30);
        assert_eq!(v[3]["tool_name"], "volume");
        let resp = json!({"message": {"role": "assistant", "content": "", "tool_calls": [
            {"function": {"name": "midia", "arguments": {"acao": "proxima"}}}
        ]}});
        let r = parse_ollama(&resp).unwrap();
        assert_eq!(r.tool_calls[0].args["acao"], "proxima");
    }
}

//! Ferramentas que o LLM pode chamar e a execução das ações.

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use super::quick::Action;
use super::{reminders, Pending, Worker};
use crate::actions::{self, MediaKey, VolumeAction};
use crate::diagnostics::tool as diag_tool;

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {"type": "object", "properties": properties, "required": required},
        }
    })
}

/// Lista de ferramentas no formato OpenAI (o Ollama aceita o mesmo formato).
pub fn specs(with_diagnostics: bool) -> Value {
    let mut list = base_specs();
    if with_diagnostics {
        // A ferramenta do diagnóstico vem no formato da API do Claude; converte para o da OpenAI.
        let d = diag_tool::definition();
        let spec = json!({
            "type": "function",
            "function": {"name": d["name"], "description": d["description"], "parameters": d["input_schema"]},
        });
        if let Some(arr) = list.as_array_mut() {
            arr.push(spec);
        }
    }
    list
}

fn base_specs() -> Value {
    json!([
        tool("abrir_app", "Abre um programa, jogo ou site instalado no PC pelo nome.",
            json!({"nome": {"type": "string", "description": "Nome do app ou jogo, ex.: 'discord', 'counter strike 2'"}}), &["nome"]),
        tool("listar_apps", "Lista apps e jogos instalados que contêm o filtro.",
            json!({"filtro": {"type": "string"}}), &[]),
        tool("volume", "Controla o volume geral do Windows.",
            json!({
                "acao": {"type": "string", "enum": ["definir", "aumentar", "diminuir", "mudo", "som"]},
                "valor": {"type": "integer", "description": "0 a 100 para definir; passo para aumentar/diminuir"}
            }), &["acao"]),
        tool("midia", "Controla a música ou vídeo que está tocando (Spotify, YouTube etc.).",
            json!({"acao": {"type": "string", "enum": ["tocar_pausar", "proxima", "anterior", "parar"]}}), &["acao"]),
        tool("criar_lembrete", "Cria um lembrete. Informe em_minutos OU horario.",
            json!({
                "texto": {"type": "string"},
                "em_minutos": {"type": "number"},
                "horario": {"type": "string", "description": "'HH:MM' (hoje ou amanhã), 'amanhã HH:MM' ou 'AAAA-MM-DD HH:MM'"}
            }), &["texto"]),
        tool("listar_lembretes", "Lista os lembretes pendentes com seus ids.", json!({}), &[]),
        tool("cancelar_lembrete", "Cancela um lembrete pelo id.", json!({"id": {"type": "integer"}}), &["id"]),
        tool("executar_rotina", "Executa uma rotina configurada pelo usuário (ex.: 'trabalho').",
            json!({"nome": {"type": "string"}}), &["nome"]),
        tool("programas_travados", "Lista programas que o Windows marca como 'Não está respondendo'.", json!({}), &[]),
        tool("fechar_programa", "Pede para fechar um programa. O usuário sempre confirma antes. Use travado=true para fechar o que travou.",
            json!({"nome": {"type": "string", "description": "Nome do programa, ex.: 'chrome'"}, "travado": {"type": "boolean"}}), &[]),
        tool("status_pc", "Uso atual de CPU, RAM, GPU, temperaturas, alertas e programas mais pesados.", json!({}), &[]),
        tool("lembrar_fato", "Guarda um fato duradouro sobre o usuário ou o PC dele para usar no futuro.",
            json!({"fato": {"type": "string"}}), &["fato"]),
        tool("esquecer_fato", "Apaga um fato guardado pelo id.", json!({"id": {"type": "integer"}}), &["id"]),
    ])
}

/// Marca no resultado de uma ferramenta que a ação só acontece depois do "sim" do usuário.
pub const CONFIRM_PREFIX: &str = "AGUARDANDO CONFIRMAÇÃO DO USUÁRIO. Pergunte: ";

fn str_arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("").trim()
}

fn int_arg(args: &Value, key: &str) -> Option<i64> {
    let v = args.get(key)?;
    v.as_i64().or_else(|| v.as_f64().map(|f| f.round() as i64)).or_else(|| v.as_str()?.trim().parse().ok())
}

const SITES: &[(&str, &str)] = &[
    ("youtube", "https://www.youtube.com"),
    ("netflix", "https://www.netflix.com"),
    ("twitch", "https://www.twitch.tv"),
    ("gmail", "https://mail.google.com"),
    ("whatsapp", "https://web.whatsapp.com"),
    ("github", "https://github.com"),
    ("chatgpt", "https://chatgpt.com"),
    ("google", "https://www.google.com"),
];

impl Worker {
    pub(super) fn run_tool(&mut self, name: &str, args: &Value) -> String {
        let r = self.try_tool(name, args);
        match r {
            Ok(s) => s,
            Err(e) => format!("ERRO: {e:#}"),
        }
    }

    fn try_tool(&mut self, name: &str, args: &Value) -> Result<String> {
        match name {
            "abrir_app" => self.run_action(Action::Open(str_arg(args, "nome").into()), true),
            "listar_apps" => {
                let list = self.apps.list(str_arg(args, "filtro"), 40);
                Ok(if list.is_empty() { "Nenhum app encontrado.".into() } else { list.join(", ") })
            }
            "volume" => {
                let v = int_arg(args, "valor").map(|v| v.clamp(0, 100) as u8);
                let a = match str_arg(args, "acao") {
                    "definir" => VolumeAction::Set(v.ok_or_else(|| anyhow!("faltou o valor"))?),
                    "aumentar" => VolumeAction::Up(v.unwrap_or(10)),
                    "diminuir" => VolumeAction::Down(v.unwrap_or(10)),
                    "mudo" => VolumeAction::Mute,
                    "som" => VolumeAction::Unmute,
                    other => bail!("ação de volume desconhecida: {other}"),
                };
                self.run_action(Action::Volume(a), true)
            }
            "midia" => {
                let k = match str_arg(args, "acao") {
                    "tocar_pausar" => MediaKey::PlayPause,
                    "proxima" => MediaKey::Next,
                    "anterior" => MediaKey::Previous,
                    "parar" => MediaKey::Stop,
                    other => bail!("ação de mídia desconhecida: {other}"),
                };
                self.run_action(Action::Media(k), true)
            }
            "criar_lembrete" => {
                let minutes = args.get("em_minutos").and_then(Value::as_f64);
                let at = args.get("horario").and_then(Value::as_str);
                self.add_reminder(str_arg(args, "texto"), minutes, at)
            }
            "listar_lembretes" => {
                let list = self.mem.lock().unwrap().pending_reminders()?;
                if list.is_empty() {
                    return Ok("Nenhum lembrete pendente.".into());
                }
                let now = chrono::Local::now();
                Ok(list
                    .iter()
                    .map(|r| format!("[{}] {} — {}", r.id, r.text, reminders::describe(r.due, now)))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            "cancelar_lembrete" => {
                let id = int_arg(args, "id").ok_or_else(|| anyhow!("faltou o id"))?;
                let ok = self.mem.lock().unwrap().cancel_reminder(id)?;
                let _ = self.wake_reminders.send(());
                Ok(if ok { "Lembrete cancelado.".into() } else { "Não achei esse lembrete.".into() })
            }
            "executar_rotina" => self.run_action(Action::Routine(str_arg(args, "nome").into()), true),
            "programas_travados" => {
                let hung = actions::hung_programs();
                Ok(if hung.is_empty() {
                    "Nenhum programa travado.".into()
                } else {
                    hung.iter().map(|h| h.name.clone()).collect::<Vec<_>>().join(", ")
                })
            }
            "fechar_programa" => {
                let hung = args.get("travado").and_then(Value::as_bool).unwrap_or(false);
                let action = if hung { Action::CloseHung } else { Action::Close(str_arg(args, "nome").into()) };
                self.run_action(action, true)
            }
            "status_pc" => Ok(self.status_text()),
            diag_tool::NAME => match &self.diagnostics {
                Some(diag) => Ok(diag_tool::call(diag, args).to_string()),
                None => bail!("diagnóstico indisponível"),
            },
            "lembrar_fato" => {
                let f = str_arg(args, "fato");
                if f.is_empty() {
                    bail!("fato vazio");
                }
                let id = self.mem.lock().unwrap().add_fact(f)?;
                Ok(format!("Guardado (id {id})."))
            }
            "esquecer_fato" => {
                let id = int_arg(args, "id").ok_or_else(|| anyhow!("faltou o id"))?;
                Ok(if self.mem.lock().unwrap().delete_fact(id)? { "Apagado.".into() } else { "Não achei.".into() })
            }
            other => bail!("ferramenta desconhecida: {other}"),
        }
    }

    pub(super) fn add_reminder(&mut self, text: &str, minutes: Option<f64>, at: Option<&str>) -> Result<String> {
        let now = chrono::Local::now();
        let due = reminders::resolve(minutes, at, now)?;
        let text = if text.is_empty() { "lembrete" } else { text };
        self.mem.lock().unwrap().add_reminder(due.timestamp(), text)?;
        let _ = self.wake_reminders.send(());
        Ok(format!("Combinado, te lembro {} de {text}.", reminders::describe(due.timestamp(), now)))
    }

    /// Executa uma ação. `confirm` = ações destrutivas pedem confirmação antes
    /// (dentro de uma rotina o próprio usuário já configurou, então não pedem).
    pub(super) fn run_action(&mut self, action: Action, confirm: bool) -> Result<String> {
        match action {
            Action::Open(name) => {
                if name.is_empty() {
                    bail!("faltou o nome do app");
                }
                if let Some(app) = self.apps.find(&name) {
                    actions::launch(&app.target)?;
                    return Ok(format!("Abrindo {}.", app.name));
                }
                let key = crate::actions::apps::normalize(&name);
                if let Some((site, url)) = SITES.iter().find(|(s, _)| key == *s || key.starts_with(&format!("{s} "))) {
                    actions::launch(url)?;
                    return Ok(format!("Abrindo o {site} no navegador."));
                }
                bail!("não achei \"{name}\" instalado. Dá para criar um apelido em brain.apps no config.toml")
            }
            Action::Volume(a) => {
                let (level, muted) = actions::volume(a)?;
                Ok(if muted { "Som no mudo.".into() } else { format!("Volume em {level}%.") })
            }
            Action::Media(k) => {
                actions::media(k)?;
                Ok(match k {
                    MediaKey::PlayPause => "Feito.",
                    MediaKey::Next => "Próxima.",
                    MediaKey::Previous => "Voltando.",
                    MediaKey::Stop => "Parado.",
                }
                .into())
            }
            Action::Wait(s) => {
                std::thread::sleep(std::time::Duration::from_secs(s.min(30)));
                Ok(String::new())
            }
            Action::Routine(name) => self.run_routine(&name),
            Action::Close(name) => {
                let found = actions::find_processes(&name);
                match found.len() {
                    0 => bail!("não achei nenhum programa chamado \"{name}\" aberto"),
                    1 => {}
                    _ => {
                        let names: Vec<_> = found.iter().map(|f| f.name.as_str()).collect();
                        bail!("achei mais de um: {}. Qual deles?", names.join(", "))
                    }
                }
                let target = found.into_iter().next().unwrap();
                if confirm {
                    let q = format!("Confirma fechar o {}? Se tiver algo sem salvar, pode perder.", target.name);
                    self.pending = Some(Pending::new(vec![target], false));
                    return Ok(format!("{CONFIRM_PREFIX}{q}"));
                }
                let n = actions::close(&target, false)?;
                Ok(format!("Fechei o {} ({n}).", target.name))
            }
            Action::CloseHung => {
                let hung = actions::hung_programs();
                if hung.is_empty() {
                    return Ok("Nenhum programa está travado agora.".into());
                }
                let names: Vec<_> = hung.iter().map(|h| h.name.clone()).collect();
                if confirm {
                    self.pending = Some(Pending::new(hung, true));
                    return Ok(format!("{CONFIRM_PREFIX}{} não está respondendo. Encerro à força?", names.join(", ")));
                }
                for h in &hung {
                    actions::close(h, true)?;
                }
                Ok(format!("Encerrei {}.", names.join(", ")))
            }
        }
    }

    fn run_routine(&mut self, name: &str) -> Result<String> {
        let key = crate::actions::apps::normalize(name);
        let Some((rname, steps)) = self
            .routines
            .iter()
            .find(|(n, _)| crate::actions::apps::normalize(n) == key)
            .map(|(n, s)| (n.clone(), s.clone()))
        else {
            let names: Vec<_> = self.routines.keys().cloned().collect();
            bail!("não existe a rotina \"{name}\". Rotinas: {}", names.join(", "));
        };
        let mut failures = Vec::new();
        for step in &steps {
            let parsed = super::quick::parse(step, &[]);
            let result = match parsed {
                Some(super::quick::Intent::Act(Action::Routine(_))) => Err(anyhow!("rotina dentro de rotina")),
                Some(super::quick::Intent::Act(a)) => self.run_action(a, false),
                _ => Err(anyhow!("passo não entendido")),
            };
            if let Err(e) = result {
                log::warn!("rotina {rname}, passo \"{step}\": {e:#}");
                failures.push(format!("\"{step}\" ({e})"));
            }
        }
        Ok(if failures.is_empty() {
            format!("Modo {rname} pronto.")
        } else {
            format!("Modo {rname} executado, mas falhou: {}.", failures.join("; "))
        })
    }

    /// Executa o que estava aguardando confirmação.
    pub(super) fn confirm_pending(&mut self, yes: bool) -> Option<String> {
        let p = self.pending.take()?;
        if p.expired() {
            return Some("Esse pedido já expirou. Pode pedir de novo?".into());
        }
        if !yes {
            return Some("Ok, não fechei nada.".into());
        }
        let mut done = Vec::new();
        let mut errors = Vec::new();
        for t in &p.targets {
            match actions::close(t, p.force) {
                Ok(_) => done.push(t.name.clone()),
                Err(e) => errors.push(format!("{e:#}")),
            }
        }
        let mut msg = String::new();
        if !done.is_empty() {
            msg = format!("Fechei {}.", done.join(", "));
        }
        if !errors.is_empty() {
            msg = format!("{msg} {}", errors.join("; ")).trim().to_string();
        }
        Some(msg)
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    fn diagnostics_tool_is_listed_only_when_available() {
        let names = |v: Value| -> Vec<String> {
            v.as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap().to_string()).collect()
        };
        assert!(!names(specs(false)).contains(&diag_tool::NAME.to_string()));
        let with = specs(true);
        let spec = with.as_array().unwrap().iter().find(|t| t["function"]["name"] == diag_tool::NAME).unwrap();
        assert_eq!(spec["type"], "function");
        assert_eq!(spec["function"]["parameters"]["type"], "object");
    }
}

//! Servidor de voz local (Python): Whisper, Chatterbox e Kokoro.
//!
//! Os modelos neurais de voz só existem em Python/PyTorch, então ficam num processo separado
//! que a Sexta-Feira liga sob demanda e desliga quando fica ocioso. Assim o app em Rust continua
//! leve e a memória de vídeo só é usada enquanto ela fala ou escuta.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use serde_json::json;

use crate::config::VoiceConfig;

pub struct VoiceServer {
    cfg: VoiceConfig,
    child: Option<Child>,
    agent: ureq::Agent,
    last_used: Instant,
}

impl VoiceServer {
    pub fn new(cfg: VoiceConfig) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(180)))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            cfg,
            child: None,
            agent,
            last_used: Instant::now(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.cfg.server_port)
    }

    fn dir(&self) -> PathBuf {
        if !self.cfg.server_dir.trim().is_empty() {
            return PathBuf::from(&self.cfg.server_dir);
        }
        let beside_exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("voice-server")));
        match beside_exe {
            Some(d) if d.exists() => d,
            _ => PathBuf::from("voice-server"),
        }
    }

    /// Usa o Python do ambiente criado pelo install.ps1, se existir.
    fn python(&self, dir: &std::path::Path) -> PathBuf {
        if self.cfg.python == "python" {
            for venv in [
                dir.join(".venv/Scripts/python.exe"),
                dir.join(".venv/bin/python"),
            ] {
                if venv.exists() {
                    return venv;
                }
            }
        }
        PathBuf::from(&self.cfg.python)
    }

    pub fn is_running(&mut self) -> bool {
        match &mut self.child {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    fn healthy(&self) -> bool {
        self.agent
            .get(self.url("/health"))
            .call()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Garante que o servidor está no ar, iniciando se preciso.
    pub fn ensure(&mut self) -> anyhow::Result<()> {
        self.last_used = Instant::now();
        if self.is_running() || self.healthy() {
            return Ok(());
        }
        let dir = self.dir();
        let script = dir.join("server.py");
        if !script.exists() {
            return Err(anyhow!(
                "servidor de voz não encontrado em {}. Veja o README para instalar.",
                dir.display()
            ));
        }
        log::info!("iniciando servidor de voz em {}", dir.display());
        let mut cmd = Command::new(self.python(&dir));
        cmd.arg(&script)
            .arg("--port")
            .arg(self.cfg.server_port.to_string())
            .arg("--stt-model")
            .arg(&self.cfg.local_stt_model)
            .current_dir(&dir)
            .stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
            cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
        }
        let child = cmd
            .spawn()
            .with_context(|| format!("não consegui rodar '{}'", self.cfg.python))?;
        self.child = Some(child);

        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if self.healthy() {
                return Ok(());
            }
            if !self.is_running() {
                return Err(anyhow!(
                    "o servidor de voz fechou ao iniciar; rode install.ps1 na pasta voice-server"
                ));
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        Err(anyhow!("o servidor de voz não respondeu em 30 s"))
    }

    /// Transcreve áudio WAV com o Whisper local.
    pub fn stt(&mut self, wav: Vec<u8>) -> anyhow::Result<String> {
        self.ensure()?;
        let mut resp = self
            .agent
            .post(self.url("/stt"))
            .header("Content-Type", "audio/wav")
            .send(&wav[..])?;
        let body: serde_json::Value = resp.body_mut().read_json()?;
        if !resp.status().is_success() {
            return Err(anyhow!("erro no Whisper local: {}", body["error"]));
        }
        Ok(body["text"].as_str().unwrap_or_default().trim().to_string())
    }

    /// Sintetiza fala e devolve o WAV.
    pub fn tts(&mut self, text: &str, engine: &str) -> anyhow::Result<Vec<u8>> {
        self.ensure()?;
        let mut resp = self.agent.post(self.url("/tts")).send_json(json!({
            "text": text,
            "engine": engine,
            "lang": "pt",
            "kokoro_voice": self.cfg.kokoro_voice,
            "voice_sample": self.cfg.voice_sample,
        }))?;
        let bytes = resp
            .body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_to_vec()?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "erro na voz ({engine}): {}",
                String::from_utf8_lossy(&bytes)
            ));
        }
        Ok(bytes)
    }

    /// Libera os modelos pesados (GPU), mantendo o processo vivo para o Kokoro.
    pub fn unload_heavy(&mut self) {
        if self.is_running() {
            let _ = self
                .agent
                .post(self.url("/unload"))
                .send_json(json!({ "engines": ["chatterbox", "whisper"] }));
        }
    }

    /// Desliga o servidor se ficou ocioso por muito tempo. Retorna true se desligou.
    pub fn stop_if_idle(&mut self) -> bool {
        let idle = Duration::from_secs(self.cfg.idle_unload_secs);
        if self.is_running() && self.last_used.elapsed() >= idle {
            log::info!("servidor de voz ocioso; desligando para liberar memória");
            self.stop();
            return true;
        }
        false
    }

    pub fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for VoiceServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Transcreve com o Whisper da Groq (nuvem). Não usa nada da GPU ou CPU do PC.
pub fn groq_stt(agent: &ureq::Agent, key: &str, model: &str, wav: &[u8]) -> anyhow::Result<String> {
    let boundary = format!("----sextafeira{}", std::process::id());
    let mut body = Vec::with_capacity(wav.len() + 512);
    for (name, value) in [
        ("model", model),
        ("language", "pt"),
        ("response_format", "json"),
        ("temperature", "0"),
    ] {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"fala.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let mut resp = agent
        .post("https://api.groq.com/openai/v1/audio/transcriptions")
        .header("Authorization", &format!("Bearer {key}"))
        .header(
            "Content-Type",
            &format!("multipart/form-data; boundary={boundary}"),
        )
        .send(&body[..])?;
    let status = resp.status();
    let json: serde_json::Value = resp.body_mut().read_json()?;
    if !status.is_success() {
        return Err(anyhow!(
            "Groq respondeu {status}: {}",
            json["error"]["message"]
        ));
    }
    Ok(json["text"].as_str().unwrap_or_default().trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sobe o servidor de voz em modo de teste (FRIDAY_FAKE=1, sem modelos) e testa o protocolo.
    /// Defina FRIDAY_TEST_PYTHON com um Python que tenha numpy para rodar.
    #[test]
    fn talks_to_voice_server() {
        let Ok(python) = std::env::var("FRIDAY_TEST_PYTHON") else {
            eprintln!("FRIDAY_TEST_PYTHON não definido; pulando");
            return;
        };
        std::env::set_var("FRIDAY_FAKE", "1");
        let cfg = VoiceConfig {
            python,
            server_dir: concat!(env!("CARGO_MANIFEST_DIR"), "/voice-server").into(),
            server_port: 18_765,
            ..Default::default()
        };
        let mut server = VoiceServer::new(cfg);
        let wav = server.tts("Olá, Marcelo.", "kokoro").expect("tts");
        let (samples, rate) = crate::voice::wav::decode(&wav).unwrap();
        assert_eq!((samples.len(), rate), (12_000, 24_000));

        let input = crate::voice::wav::encode(&vec![0.1; 16_000], 16_000).unwrap();
        assert_eq!(server.stt(input).unwrap(), "teste de transcrição");
        assert!(
            server.tts("", "kokoro").is_err(),
            "texto vazio deveria falhar"
        );

        server.unload_heavy();
        server.stop();
        assert!(!server.is_running());
    }
}

//! Configuração do usuário, lida de `config.toml` na pasta de configuração do sistema
//! (no Windows: `%APPDATA%\sexta-feira\config.toml`). Se o arquivo não existir, é criado
//! com os valores padrão.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub monitor: MonitorConfig,
    pub game_mode: GameModeConfig,
    pub widget: WidgetConfig,
    pub voice: VoiceConfig,
    pub diagnostics: DiagnosticsConfig,
    pub brain: BrainConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorConfig {
    /// Intervalo de coleta normal, em milissegundos.
    pub interval_ms: u64,
    /// Intervalo de coleta durante o modo jogo, em milissegundos.
    pub game_interval_ms: u64,
    /// Temperatura (°C) a partir da qual a CPU ou a GPU gera alerta.
    pub temp_alert_c: f32,
    /// Uso de RAM (%) a partir do qual gera alerta.
    pub ram_alert_pct: f32,
    /// Espaço livre mínimo em disco (%) antes de gerar alerta.
    pub disk_free_alert_pct: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GameModeConfig {
    /// Ativa o modo jogo automaticamente.
    pub auto: bool,
    /// Considera qualquer janela em tela cheia no primeiro plano como jogo (Windows).
    pub detect_fullscreen: bool,
    /// Executáveis que sempre ativam o modo jogo (sem diferenciar maiúsculas).
    pub executables: Vec<String>,
    /// Esconde o widget enquanto o modo jogo estiver ativo.
    pub hide_widget: bool,
}

/// Qual LLM responde: Groq (nuvem, não pesa no PC) ou Ollama (local).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Groq,
    Ollama,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BrainConfig {
    pub provider: Provider,
    /// Chave da Groq. Vazia = usa a variável de ambiente `GROQ_API_KEY`.
    pub groq_api_key: String,
    pub groq_model: String,
    pub groq_url: String,
    pub ollama_url: String,
    pub ollama_model: String,
    /// Quanto tempo o Ollama mantém o modelo carregado depois de responder (ex.: "5m").
    pub ollama_keep_alive: String,
    /// No modo jogo, descarrega o modelo do Ollama da VRAM logo depois de cada resposta.
    pub ollama_unload_in_game: bool,
    /// Quantas mensagens anteriores vão como contexto para o LLM.
    pub history_messages: usize,
    /// Tempo máximo de espera pela resposta do LLM, em segundos.
    pub timeout_s: u64,
    /// Apelidos de apps: nome falado -> caminho, atalho ou URL (ex.: "steam://rungameid/730").
    pub apps: BTreeMap<String, String>,
    /// Rotinas: nome -> passos. Cada passo é uma linha, ex.: "abrir code", "volume 30",
    /// "fechar spotify", "midia pausar", "esperar 2".
    pub routines: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WidgetConfig {
    pub enabled: bool,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagnosticsConfig {
    /// De quantas em quantas horas roda o diagnóstico profundo sozinho (0 = só quando pedir).
    /// Nunca roda sozinho durante o modo jogo.
    pub auto_interval_h: u64,
    /// Espera depois de abrir a Sexta-Feira antes do primeiro diagnóstico automático, em segundos.
    pub startup_delay_s: u64,
    /// Quantos dias do Log de Eventos analisar.
    pub event_days: u32,
    /// Drivers de rede, áudio, chipset etc. mais velhos que isso (em anos) entram no relatório.
    pub old_driver_years: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            monitor: MonitorConfig::default(),
            game_mode: GameModeConfig::default(),
            widget: WidgetConfig::default(),
            voice: VoiceConfig::default(),
            diagnostics: DiagnosticsConfig::default(),
            brain: BrainConfig::default(),
        }
    }
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            interval_ms: 2_000,
            game_interval_ms: 15_000,
            temp_alert_c: 90.0,
            ram_alert_pct: 90.0,
            disk_free_alert_pct: 10.0,
        }
    }
}

impl Default for GameModeConfig {
    fn default() -> Self {
        Self {
            auto: true,
            detect_fullscreen: true,
            executables: vec![
                "cs2.exe".into(),
                "valorant-win64-shipping.exe".into(),
                "fortniteclient-win64-shipping.exe".into(),
                "leagueoflegends.exe".into(),
                "r5apex.exe".into(),
                "gta5.exe".into(),
                "eldenring.exe".into(),
                "cyberpunk2077.exe".into(),
            ],
            hide_widget: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceConfig {
    pub enabled: bool,
    /// Fala para texto: "groq" (Whisper na nuvem, custo zero no PC) ou "local" (servidor de voz).
    pub stt: String,
    /// Chave da Groq para o Whisper. Vazia = variável de ambiente GROQ_API_KEY.
    pub groq_api_key: String,
    pub groq_stt_model: String,
    /// Motor de voz fora de jogo: "chatterbox" (clonagem, mais pesado), "kokoro" (leve) ou "off".
    pub tts_engine: String,
    /// Motor de voz durante o modo jogo.
    pub tts_engine_game: String,
    /// Áudio de referência (WAV de 5 a 15 s) para o Chatterbox clonar. Use uma voz com direito de uso.
    pub voice_sample: String,
    /// Voz do Kokoro (pf_dora = feminina pt-BR).
    pub kokoro_voice: String,
    /// Modelo do Whisper local (tiny, base, small, medium, large-v3-turbo).
    pub local_stt_model: String,
    /// Detecta a palavra "Sexta-Feira" (precisa treinar uma vez pelo app). Desligada no modo jogo.
    pub wake_word: bool,
    /// Rigor da palavra de ativação (0 a 1; maior = menos falsos positivos).
    pub wake_threshold: f32,
    /// Executável do Python usado pelo servidor de voz.
    pub python: String,
    /// Pasta do servidor de voz. Vazio = pasta "voice-server" ao lado do executável.
    pub server_dir: String,
    pub server_port: u16,
    /// Desliga o servidor de voz depois de tantos segundos sem uso.
    pub idle_unload_secs: u64,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            stt: "groq".into(),
            groq_api_key: String::new(),
            groq_stt_model: "whisper-large-v3-turbo".into(),
            tts_engine: "chatterbox".into(),
            tts_engine_game: "kokoro".into(),
            voice_sample: String::new(),
            kokoro_voice: "pf_dora".into(),
            local_stt_model: "small".into(),
            wake_word: true,
            wake_threshold: 0.5,
            python: "python".into(),
            server_dir: String::new(),
            server_port: 8765,
            idle_unload_secs: 300,
        }
    }
}

impl Default for BrainConfig {
    fn default() -> Self {
        let mut routines = BTreeMap::new();
        routines.insert(
            "trabalho".into(),
            vec!["abrir visual studio code".into(), "abrir chrome".into(), "volume 30".into()],
        );
        Self {
            provider: Provider::Groq,
            groq_api_key: String::new(),
            groq_model: "openai/gpt-oss-120b".into(),
            groq_url: "https://api.groq.com/openai/v1".into(),
            ollama_url: "http://localhost:11434".into(),
            ollama_model: "qwen2.5:7b".into(),
            ollama_keep_alive: "5m".into(),
            ollama_unload_in_game: true,
            history_messages: 12,
            timeout_s: 60,
            apps: BTreeMap::new(),
            routines,
        }
    }
}

impl VoiceConfig {
    pub fn groq_key(&self) -> Option<String> {
        let key = if self.groq_api_key.trim().is_empty() {
            std::env::var("GROQ_API_KEY").unwrap_or_default()
        } else {
            self.groq_api_key.clone()
        };
        let key = key.trim().to_string();
        (!key.is_empty()).then_some(key)
    }
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self { auto_interval_h: 6, startup_delay_s: 180, event_days: 7, old_driver_years: 3 }
    }
}

impl Default for WidgetConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            x: 24,
            y: 24,
        }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "sexta-feira")
            .map(|d| d.config_dir().join("config.toml"))
    }

    /// Pasta de dados (palavra de ativação treinada, etc.). É criada se não existir.
    pub fn data_dir() -> PathBuf {
        let dir = directories::ProjectDirs::from("", "", "sexta-feira")
            .map(|d| d.data_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// Carrega a configuração, criando o arquivo padrão se ele ainda não existir.
    pub fn load_or_create() -> anyhow::Result<Self> {
        let Some(path) = Self::path() else {
            return Ok(Self::default());
        };
        if !path.exists() {
            let cfg = Self::default();
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, toml::to_string_pretty(&cfg)?)?;
            log::info!("configuração padrão criada em {}", path.display());
            return Ok(cfg);
        }
        let text = std::fs::read_to_string(&path)?;
        toml::from_str(&text).with_context(|| format!("config inválida em {}", path.display()))
    }

    pub fn save(&self) -> anyhow::Result<()> {
        if let Some(path) = Self::path() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, toml::to_string_pretty(self)?)?;
        }
        Ok(())
    }
}

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

impl Default for Config {
    fn default() -> Self {
        Self {
            monitor: MonitorConfig::default(),
            game_mode: GameModeConfig::default(),
            widget: WidgetConfig::default(),
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

impl Default for WidgetConfig {
    fn default() -> Self {
        Self { enabled: true, x: 24, y: 24 }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "sexta-feira").map(|d| d.config_dir().join("config.toml"))
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

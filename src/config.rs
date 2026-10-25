//! Configuração do usuário, lida de `config.toml` na pasta de configuração do sistema
//! (no Windows: `%APPDATA%\sexta-feira\config.toml`). Se o arquivo não existir, é criado
//! com os valores padrão.

use std::path::PathBuf;

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub monitor: MonitorConfig,
    pub game_mode: GameModeConfig,
    pub widget: WidgetConfig,
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

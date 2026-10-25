//! Coleta dos dados brutos. Cada parte pode falhar sozinha (sem permissão, serviço
//! desligado...) sem derrubar as outras; a falha vira uma nota no relatório.

use sysinfo::{Components, Disks};

use super::SensorReading;
use crate::config::DiagnosticsConfig;

#[cfg(windows)]
mod windows;

/// Um evento de erro ou crítico do Log de Eventos.
#[derive(Debug, Clone, Default)]
pub struct EventRecord {
    pub provider: String,
    pub event_id: u32,
    /// 1 = crítico, 2 = erro.
    pub level: u8,
    pub time_unix: i64,
    pub message: String,
}

/// Saúde de um disco físico.
#[derive(Debug, Clone, Default)]
pub struct DiskHealth {
    pub name: String,
    /// "SSD", "HDD" ou vazio se desconhecido.
    pub media: String,
    /// 0 = saudável, 1 = atenção, 2 = com defeito (MSFT_PhysicalDisk.HealthStatus).
    pub health: Option<u16>,
    /// O SMART do próprio disco prevê falha.
    pub predict_failure: Option<bool>,
    pub temp_c: Option<f32>,
    pub temp_max_c: Option<f32>,
    /// Desgaste do SSD em % (0 = novo, 100 = vida útil estimada esgotada).
    pub wear_pct: Option<u32>,
    pub read_errors_uncorrected: Option<u64>,
    pub write_errors_uncorrected: Option<u64>,
    pub power_on_hours: Option<u64>,
}

/// Espaço de uma partição montada.
#[derive(Debug, Clone, Default)]
pub struct VolumeSpace {
    pub mount: String,
    pub total_gb: u64,
    pub free_gb: u64,
}

#[derive(Debug, Clone, Default)]
pub struct DriverInfo {
    pub device: String,
    /// Classe do Windows: DISPLAY, NET, MEDIA, SYSTEM...
    pub class: String,
    pub provider: String,
    pub version: String,
    /// Data do driver como (ano, mês, dia).
    pub date: Option<(i32, u32, u32)>,
}

/// Dispositivo com o "ponto de exclamação amarelo" no Gerenciador de Dispositivos.
#[derive(Debug, Clone, Default)]
pub struct ProblemDevice {
    pub name: String,
    pub class: String,
    pub error_code: u32,
}

#[derive(Debug)]
pub struct RawData {
    pub events: Result<Vec<EventRecord>, String>,
    pub disks: Result<Vec<DiskHealth>, String>,
    pub volumes: Vec<VolumeSpace>,
    pub drivers: Result<Vec<DriverInfo>, String>,
    pub problem_devices: Result<Vec<ProblemDevice>, String>,
    pub sensors: Vec<SensorReading>,
    /// Indica se veio alguma leitura de temperatura da CPU.
    pub cpu_sensor_found: bool,
    pub uptime_s: u64,
    pub notes: Vec<String>,
}

pub fn collect(cfg: &DiagnosticsConfig) -> RawData {
    let mut raw = RawData {
        events: Ok(Vec::new()),
        disks: Ok(Vec::new()),
        drivers: Ok(Vec::new()),
        problem_devices: Ok(Vec::new()),
        volumes: volumes(),
        sensors: Vec::new(),
        cpu_sensor_found: false,
        uptime_s: sysinfo::System::uptime(),
        notes: Vec::new(),
    };
    common_sensors(&mut raw);
    #[cfg(windows)]
    windows::collect(cfg, &mut raw);
    #[cfg(not(windows))]
    {
        let _ = cfg;
        let msg = "disponível só no Windows".to_string();
        raw.events = Err(msg.clone());
        raw.disks = Err(msg.clone());
        raw.drivers = Err(msg.clone());
        raw.problem_devices = Err(msg);
    }
    raw
}

/// Abaixa a prioridade de CPU e de disco da thread atual para não competir com jogos.
pub fn enter_background_mode() {
    #[cfg(windows)]
    windows::enter_background_mode();
}

fn volumes() -> Vec<VolumeSpace> {
    let disks = Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|d| d.total_space() >= 1_073_741_824)
        .map(|d| VolumeSpace {
            mount: d.mount_point().display().to_string(),
            total_gb: d.total_space() / 1_073_741_824,
            free_gb: d.available_space() / 1_073_741_824,
        })
        .collect()
}

const CPU_LABELS: [&str; 5] = ["cpu", "package", "tctl", "tdie", "core"];

/// Sensores que funcionam em qualquer sistema: os do sysinfo e as GPUs NVIDIA.
fn common_sensors(raw: &mut RawData) {
    let components = Components::new_with_refreshed_list();
    for c in components.list() {
        let Some(t) = c.temperature().filter(|t| t.is_finite() && *t > 0.0) else { continue };
        let label = c.label().to_string();
        let lower = label.to_lowercase();
        if CPU_LABELS.iter().any(|k| lower.contains(k)) {
            raw.cpu_sensor_found = true;
        }
        raw.sensors.push(SensorReading {
            source: "Sistema".into(),
            label,
            temp_c: t,
            limit_c: c.critical().filter(|c| c.is_finite() && *c > 0.0),
        });
    }

    use nvml_wrapper::enum_wrappers::device::{TemperatureSensor, TemperatureThreshold};
    let Ok(nvml) = nvml_wrapper::Nvml::init() else { return };
    let count = nvml.device_count().unwrap_or(0);
    for i in 0..count {
        let Ok(dev) = nvml.device_by_index(i) else { continue };
        let Ok(t) = dev.temperature(TemperatureSensor::Gpu) else { continue };
        raw.sensors.push(SensorReading {
            source: "GPU".into(),
            label: dev.name().unwrap_or_else(|_| format!("GPU NVIDIA {i}")),
            temp_c: t as f32,
            limit_c: dev.temperature_threshold(TemperatureThreshold::Slowdown).ok().map(|t| t as f32),
        });
    }
}

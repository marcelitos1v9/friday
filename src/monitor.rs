//! Coleta de métricas do sistema com custo mínimo.
//!
//! Cada tipo de dado tem a sua própria frequência: CPU e RAM a cada ciclo, processos a cada
//! poucos ciclos, discos e sensores raramente. No modo jogo o ciclo fica bem mais lento.

use std::time::{Duration, Instant};

use nvml_wrapper::{enum_wrappers::device::TemperatureSensor, Nvml};
use sysinfo::{Components, Disks, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

use crate::config::MonitorConfig;

#[derive(Debug, Clone, Default)]
pub struct GpuSnapshot {
    pub name: String,
    pub usage_pct: f32,
    pub temp_c: f32,
    pub vram_used_mb: u64,
    pub vram_total_mb: u64,
}

#[derive(Debug, Clone, Default)]
pub struct ProcessInfo {
    pub name: String,
    pub cpu_pct: f32,
    pub mem_mb: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Critical,
}

#[derive(Debug, Clone)]
pub struct Alert {
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub cpu_pct: f32,
    pub cpu_temp_c: Option<f32>,
    pub ram_used_mb: u64,
    pub ram_total_mb: u64,
    pub gpu: Option<GpuSnapshot>,
    pub top_processes: Vec<ProcessInfo>,
    pub alerts: Vec<Alert>,
    /// Memória usada pela própria Sexta-Feira, para provar que ela é leve.
    pub self_mem_mb: u64,
}

impl Snapshot {
    pub fn ram_pct(&self) -> f32 {
        if self.ram_total_mb == 0 {
            0.0
        } else {
            self.ram_used_mb as f32 * 100.0 / self.ram_total_mb as f32
        }
    }
}

pub struct Monitor {
    sys: System,
    components: Components,
    disks: Disks,
    nvml: Option<Nvml>,
    cfg: MonitorConfig,
    tick: u64,
    last_disk_refresh: Option<Instant>,
    disk_alerts: Vec<Alert>,
    top_processes: Vec<ProcessInfo>,
    self_pid: Option<sysinfo::Pid>,
}

const PROCESS_EVERY_TICKS: u64 = 3;
const SENSOR_EVERY_TICKS: u64 = 5;
const DISK_INTERVAL: Duration = Duration::from_secs(120);
const TOP_N: usize = 5;

impl Monitor {
    pub fn new(cfg: MonitorConfig) -> Self {
        let sys = System::new_with_specifics(RefreshKind::nothing());
        let nvml = match Nvml::init() {
            Ok(n) => Some(n),
            Err(e) => {
                log::info!("NVML indisponível (sem GPU NVIDIA ou driver): {e}");
                None
            }
        };
        Self {
            sys,
            components: Components::new_with_refreshed_list(),
            disks: Disks::new(),
            nvml,
            cfg,
            tick: 0,
            last_disk_refresh: None,
            disk_alerts: Vec::new(),
            top_processes: Vec::new(),
            self_pid: sysinfo::get_current_pid().ok(),
        }
    }

    pub fn interval(&self, game_mode: bool) -> Duration {
        Duration::from_millis(if game_mode { self.cfg.game_interval_ms } else { self.cfg.interval_ms })
    }

    /// Faz uma coleta. Em modo jogo pula tudo o que não é essencial.
    pub fn sample(&mut self, game_mode: bool) -> Snapshot {
        self.tick += 1;
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();

        let mut snap = Snapshot {
            cpu_pct: self.sys.global_cpu_usage(),
            ram_used_mb: self.sys.used_memory() / 1_048_576,
            ram_total_mb: self.sys.total_memory() / 1_048_576,
            ..Default::default()
        };

        if !game_mode && self.tick % PROCESS_EVERY_TICKS == 1 {
            self.refresh_processes();
        }
        snap.top_processes = self.top_processes.clone();
        if let Some(pid) = self.self_pid {
            self.sys.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[pid]),
                false,
                ProcessRefreshKind::nothing().with_memory(),
            );
            if let Some(p) = self.sys.process(pid) {
                snap.self_mem_mb = p.memory() / 1_048_576;
            }
        }

        if game_mode || self.tick % SENSOR_EVERY_TICKS == 1 {
            self.components.refresh(false);
        }
        snap.cpu_temp_c = self.cpu_temp();
        snap.gpu = self.gpu();

        let disk_due = self.last_disk_refresh.map_or(true, |t| t.elapsed() >= DISK_INTERVAL);
        if disk_due {
            self.refresh_disks();
        }

        snap.alerts = self.alerts(&snap);
        snap
    }

    fn refresh_processes(&mut self) {
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        let cpus = self.sys.cpus().len().max(1) as f32;
        // Agrupa por nome: o Chrome com 30 processos aparece como uma linha só.
        let mut by_name: std::collections::HashMap<String, ProcessInfo> = std::collections::HashMap::new();
        for p in self.sys.processes().values().filter(|p| p.thread_kind().is_none()) {
            let name = p.name().to_string_lossy().into_owned();
            let entry = by_name.entry(name.clone()).or_insert_with(|| ProcessInfo { name, ..Default::default() });
            entry.cpu_pct += p.cpu_usage() / cpus;
            entry.mem_mb += p.memory() / 1_048_576;
        }
        let mut procs: Vec<ProcessInfo> = by_name.into_values().collect();
        procs.sort_by(|a, b| b.mem_mb.cmp(&a.mem_mb));
        procs.truncate(TOP_N);
        self.top_processes = procs;
    }

    fn cpu_temp(&self) -> Option<f32> {
        // Procura sensores típicos de CPU; se não houver, usa o mais quente.
        let mut cpu = None;
        let mut hottest: Option<f32> = None;
        for c in self.components.list() {
            let Some(t) = c.temperature() else { continue };
            if !t.is_finite() || t <= 0.0 {
                continue;
            }
            let label = c.label().to_lowercase();
            if ["cpu", "package", "tctl", "tdie", "core"].iter().any(|k| label.contains(k)) {
                cpu = Some(cpu.map_or(t, |c: f32| c.max(t)));
            }
            hottest = Some(hottest.map_or(t, |h| h.max(t)));
        }
        cpu.or(hottest)
    }

    fn gpu(&self) -> Option<GpuSnapshot> {
        let nvml = self.nvml.as_ref()?;
        let dev = nvml.device_by_index(0).ok()?;
        let util = dev.utilization_rates().ok();
        let mem = dev.memory_info().ok();
        Some(GpuSnapshot {
            name: dev.name().unwrap_or_else(|_| "GPU NVIDIA".into()),
            usage_pct: util.map_or(0.0, |u| u.gpu as f32),
            temp_c: dev.temperature(TemperatureSensor::Gpu).map_or(0.0, |t| t as f32),
            vram_used_mb: mem.as_ref().map_or(0, |m| m.used / 1_048_576),
            vram_total_mb: mem.as_ref().map_or(0, |m| m.total / 1_048_576),
        })
    }

    fn refresh_disks(&mut self) {
        self.last_disk_refresh = Some(Instant::now());
        self.disks.refresh(true);
        self.disk_alerts.clear();
        for d in self.disks.list() {
            let total = d.total_space();
            if total < 1_073_741_824 {
                continue; // ignora partições pequenas (recuperação, EFI)
            }
            let free_pct = d.available_space() as f32 * 100.0 / total as f32;
            if free_pct < self.cfg.disk_free_alert_pct {
                self.disk_alerts.push(Alert {
                    severity: if free_pct < 3.0 { Severity::Critical } else { Severity::Warning },
                    message: format!(
                        "Disco {} com só {:.0}% livre ({} GB)",
                        d.mount_point().display(),
                        free_pct,
                        d.available_space() / 1_073_741_824
                    ),
                });
            }
        }
    }

    fn alerts(&self, s: &Snapshot) -> Vec<Alert> {
        let mut out = Vec::new();
        let limit = self.cfg.temp_alert_c;
        if let Some(t) = s.cpu_temp_c.filter(|t| *t >= limit) {
            out.push(Alert { severity: Severity::Critical, message: format!("CPU a {t:.0} °C") });
        }
        if let Some(g) = s.gpu.as_ref().filter(|g| g.temp_c >= limit) {
            out.push(Alert { severity: Severity::Critical, message: format!("GPU a {:.0} °C", g.temp_c) });
        }
        if s.ram_pct() >= self.cfg.ram_alert_pct {
            let culprit = s.top_processes.first().map(|p| format!(" ({} usa {} MB)", p.name, p.mem_mb));
            out.push(Alert {
                severity: Severity::Warning,
                message: format!("RAM em {:.0}%{}", s.ram_pct(), culprit.unwrap_or_default()),
            });
        }
        out.extend(self.disk_alerts.iter().cloned());
        out
    }
}

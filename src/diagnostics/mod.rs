//! Diagnóstico profundo: "o que está errado no meu PC".
//!
//! Lê o Log de Eventos do Windows, a saúde dos discos (SMART e contadores de confiabilidade),
//! drivers e dispositivos com problema e todos os sensores de temperatura que conseguir. A
//! coleta é pesada (WMI, milhares de eventos), então roda numa thread própria, em prioridade de
//! segundo plano, e só quando alguém pede ou de tempos em tempos fora do modo jogo.
//!
//! - [`collect`]: coleta os dados brutos (específico de cada sistema).
//! - [`rules`]: transforma os dados brutos em um [`Report`] com problemas e sugestões.
//! - [`Diagnostics`]: serviço com cache, usado pela interface e pelo cérebro.
//! - [`tool`]: a ferramenta `diagnosticar_pc` que o cérebro (LLM) pode chamar.

// Parte da API (ferramenta do cérebro, utilitários usados só no Windows) ainda não tem
// chamador em todas as plataformas; o cérebro (M2) passa a usar `tool`.
#![allow(dead_code)]

mod collect;
pub mod rules;
pub mod tool;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::config::{DiagnosticsConfig, MonitorConfig};

/// Gravidade de um problema encontrado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Aviso,
    Critico,
}

/// Área do PC a que o problema se refere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Eventos,
    Discos,
    Drivers,
    Temperaturas,
    Sistema,
}

impl Category {
    pub const ALL: [Category; 5] =
        [Category::Eventos, Category::Discos, Category::Drivers, Category::Temperaturas, Category::Sistema];

    pub fn label(self) -> &'static str {
        match self {
            Category::Eventos => "Log de eventos",
            Category::Discos => "Discos",
            Category::Drivers => "Drivers",
            Category::Temperaturas => "Temperaturas",
            Category::Sistema => "Sistema",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| format!("{c:?}").eq_ignore_ascii_case(s))
    }
}

// Os nomes em JSON ficam em português porque é isso que o cérebro lê e repete ao usuário.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    #[serde(rename = "gravidade")]
    pub level: Level,
    #[serde(rename = "categoria")]
    pub category: Category,
    #[serde(rename = "titulo")]
    pub title: String,
    #[serde(rename = "detalhe")]
    pub detail: String,
    #[serde(rename = "sugestao")]
    pub suggestion: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SensorReading {
    /// De onde veio a leitura: "GPU", "Disco", "Sistema", "LibreHardwareMonitor"...
    #[serde(rename = "origem")]
    pub source: String,
    #[serde(rename = "nome")]
    pub label: String,
    #[serde(rename = "temperatura_c")]
    pub temp_c: f32,
    /// Temperatura a partir da qual o próprio hardware considera perigoso, se informada.
    #[serde(rename = "limite_c")]
    pub limit_c: Option<f32>,
}

/// Resultado de uma análise completa.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// Momento da coleta, em segundos desde 1970.
    pub generated_unix: i64,
    pub duration_ms: u64,
    /// Problemas ordenados do mais grave para o mais leve.
    pub findings: Vec<Finding>,
    pub sensors: Vec<SensorReading>,
    /// Coisas que não deu para verificar e por quê (sem permissão, sem sensor...).
    pub notes: Vec<String>,
    #[serde(skip)]
    pub created: Option<Instant>,
}

impl Report {
    pub fn count(&self, level: Level) -> usize {
        self.findings.iter().filter(|f| f.level == level).count()
    }

    /// Frase curta para a interface e para a voz.
    pub fn summary(&self) -> String {
        let crit = self.count(Level::Critico);
        let warn = self.count(Level::Aviso);
        let info = self.count(Level::Info);
        match (crit, warn) {
            (0, 0) if info == 0 => "Nenhum problema encontrado. Seu PC está saudável.".into(),
            (0, 0) => format!("Nada grave. Tenho {} observaç{} para você.", info, if info == 1 { "ão" } else { "ões" }),
            _ => {
                let mut parts = Vec::new();
                if crit > 0 {
                    parts.push(format!("{crit} problema{} crítico{}", plural(crit), plural(crit)));
                }
                if warn > 0 {
                    parts.push(format!("{warn} aviso{}", plural(warn)));
                }
                format!("Encontrei {}.", parts.join(" e "))
            }
        }
    }

    pub fn age(&self) -> Duration {
        self.created.map_or(Duration::MAX, |c| c.elapsed())
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

pub fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Faz a coleta e a análise completas, bloqueando a thread atual.
pub fn run(cfg: &DiagnosticsConfig, monitor: &MonitorConfig) -> Report {
    let start = Instant::now();
    let raw = collect::collect(cfg);
    let mut report = rules::analyze(&raw, cfg, monitor, now_unix());
    report.duration_ms = start.elapsed().as_millis() as u64;
    report.created = Some(Instant::now());
    log::info!("diagnóstico concluído em {} ms: {}", report.duration_ms, report.summary());
    report
}

/// Avisos do serviço para quem estiver ouvindo (a interface).
pub enum Event {
    Started,
    Finished(Arc<Report>),
}

#[derive(Default)]
struct State {
    report: Option<Arc<Report>>,
    running: bool,
    requested: bool,
    quit: bool,
    /// Quantas análises já terminaram; usado para esperar pela próxima.
    generation: u64,
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
}

/// Serviço de diagnóstico. Clonar é barato; todos os clones falam com a mesma thread.
#[derive(Clone)]
pub struct Diagnostics {
    shared: Arc<Shared>,
}

impl Diagnostics {
    /// Inicia a thread de diagnóstico. `game_mode` é atualizado pelo monitor; enquanto estiver
    /// ligado a análise automática fica suspensa (pedidos explícitos ainda são atendidos).
    pub fn start(
        cfg: DiagnosticsConfig,
        monitor: MonitorConfig,
        game_mode: Arc<AtomicBool>,
        on_event: impl Fn(Event) + Send + 'static,
    ) -> (Self, std::thread::JoinHandle<()>) {
        let shared = Arc::new(Shared { state: Mutex::new(State::default()), cv: Condvar::new() });
        let worker_shared = shared.clone();
        let handle = std::thread::Builder::new()
            .name("diagnostico".into())
            .spawn(move || worker(worker_shared, cfg, monitor, game_mode, on_event))
            .expect("falha ao iniciar a thread de diagnóstico");
        (Self { shared }, handle)
    }

    /// Pede uma análise nova sem esperar por ela.
    pub fn request(&self) {
        let mut st = self.shared.state.lock().unwrap();
        st.requested = true;
        self.shared.cv.notify_all();
    }

    /// Último relatório pronto, se houver.
    pub fn latest(&self) -> Option<Arc<Report>> {
        self.shared.state.lock().unwrap().report.clone()
    }

    pub fn is_running(&self) -> bool {
        self.shared.state.lock().unwrap().running
    }

    /// Devolve um relatório com no máximo `max_age` de idade, fazendo uma análise nova se
    /// preciso. Espera no máximo `timeout`; se estourar, devolve o último que houver.
    pub fn get_fresh(&self, max_age: Duration, timeout: Duration) -> Option<Arc<Report>> {
        let mut st = self.shared.state.lock().unwrap();
        if let Some(r) = st.report.as_ref().filter(|r| r.age() <= max_age) {
            return Some(r.clone());
        }
        // Se já há uma análise rodando, a próxima que terminar serve.
        let target = st.generation + 1;
        if !st.running {
            st.requested = true;
            self.shared.cv.notify_all();
        }
        let deadline = Instant::now() + timeout;
        while st.generation < target && !st.quit {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            st = self.shared.cv.wait_timeout(st, left).unwrap().0;
        }
        st.report.clone()
    }

    pub fn shutdown(&self) {
        let mut st = self.shared.state.lock().unwrap();
        st.quit = true;
        self.shared.cv.notify_all();
    }
}

fn worker(
    shared: Arc<Shared>,
    cfg: DiagnosticsConfig,
    monitor: MonitorConfig,
    game_mode: Arc<AtomicBool>,
    on_event: impl Fn(Event),
) {
    collect::enter_background_mode();
    let started = Instant::now();
    let first_auto = Duration::from_secs(cfg.startup_delay_s);
    let auto_every = (cfg.auto_interval_h > 0).then(|| Duration::from_secs(cfg.auto_interval_h * 3600));
    let mut last_run: Option<Instant> = None;

    loop {
        {
            let mut st = shared.state.lock().unwrap();
            loop {
                if st.quit {
                    return;
                }
                if st.requested {
                    break;
                }
                let auto_due = auto_every.is_some_and(|every| match last_run {
                    None => started.elapsed() >= first_auto,
                    Some(t) => t.elapsed() >= every,
                });
                if auto_due && !game_mode.load(Ordering::Relaxed) {
                    break;
                }
                // Acorda de vez em quando para reavaliar o horário e o modo jogo.
                st = shared.cv.wait_timeout(st, Duration::from_secs(30)).unwrap().0;
            }
            st.requested = false;
            st.running = true;
        }
        on_event(Event::Started);
        let report = Arc::new(run(&cfg, &monitor));
        last_run = Some(Instant::now());
        {
            let mut st = shared.state.lock().unwrap();
            st.report = Some(report.clone());
            st.running = false;
            st.generation += 1;
            shared.cv.notify_all();
        }
        on_event(Event::Finished(report));
    }
}

/// Texto relativo em português: "agora", "há 5 min", "há 3 h", "há 2 dias".
pub fn ago(secs: i64) -> String {
    match secs {
        s if s < 60 => "agora".into(),
        s if s < 3600 => format!("há {} min", s / 60),
        s if s < 86_400 => format!("há {} h", s / 3600),
        s if s < 2 * 86_400 => "há 1 dia".into(),
        s => format!("há {} dias", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(levels: &[Level]) -> Report {
        Report {
            generated_unix: 0,
            duration_ms: 0,
            findings: levels
                .iter()
                .map(|&level| Finding {
                    level,
                    category: Category::Sistema,
                    title: String::new(),
                    detail: String::new(),
                    suggestion: String::new(),
                })
                .collect(),
            sensors: vec![],
            notes: vec![],
            created: None,
        }
    }

    #[test]
    fn summary_texts() {
        assert_eq!(report(&[]).summary(), "Nenhum problema encontrado. Seu PC está saudável.");
        assert_eq!(report(&[Level::Info]).summary(), "Nada grave. Tenho 1 observação para você.");
        assert_eq!(
            report(&[Level::Critico, Level::Aviso, Level::Aviso]).summary(),
            "Encontrei 1 problema crítico e 2 avisos."
        );
        assert_eq!(report(&[Level::Critico, Level::Critico]).summary(), "Encontrei 2 problemas críticos.");
    }

    #[test]
    fn category_names() {
        assert_eq!(Category::from_name("DISCOS"), Some(Category::Discos));
        assert_eq!(Category::from_name("nada"), None);
    }

    #[test]
    fn ago_texts() {
        assert_eq!(ago(10), "agora");
        assert_eq!(ago(300), "há 5 min");
        assert_eq!(ago(7200), "há 2 h");
        assert_eq!(ago(90_000), "há 1 dia");
        assert_eq!(ago(3 * 86_400), "há 3 dias");
    }
}

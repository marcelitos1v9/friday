//! Cérebro da Sexta-Feira: recebe texto, decide o que fazer e devolve texto.
//!
//! Interface pública (texto entra, texto sai):
//!
//! ```ignore
//! let brain = Brain::start(cfg.brain.clone(), status.clone(), |ev| match ev {
//!     BrainEvent::Reply(texto) => voz.speak(&texto),
//!     BrainEvent::Reminder(texto) => voz.speak(&texto),
//!     _ => {}
//! });
//! brain.ask("abre o discord");
//! ```
//!
//! Tudo roda numa thread própria que fica parada no canal até chegar um pedido. Comandos
//! simples (volume, mídia, abrir app, lembretes, rotinas) são resolvidos sem LLM; o resto vai
//! para a Groq ou o Ollama com ferramentas. O cliente do LLM só é criado no primeiro uso.

pub mod llm;
mod memory;
mod quick;
mod reminders;
mod tools;

use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{Datelike, Local, Timelike};

use crate::actions::apps::AppIndex;
use crate::actions::ProcessMatch;
use crate::config::{BrainConfig, Provider};
use crate::diagnostics::Diagnostics;
use crate::monitor::{Severity, Snapshot};
use llm::{Llm, Msg};
use memory::Memory;
use quick::Intent;

/// Última leitura do monitor, compartilhada para o cérebro responder sobre o PC.
pub type SharedStatus = Arc<Mutex<Option<Snapshot>>>;

#[derive(Debug, Clone, PartialEq)]
pub enum BrainEvent {
    /// Começou a pensar (chamada ao LLM em andamento).
    Thinking,
    /// Resposta para mostrar e falar.
    Reply(String),
    /// Um lembrete venceu agora.
    Reminder(String),
    /// Falha (sem rede, sem chave etc.). O texto já é amigável para mostrar.
    Error(String),
}

enum Job {
    Ask(String),
    GameMode(bool),
    Quit,
}

#[derive(Clone)]
pub struct Brain {
    tx: Sender<Job>,
}

type EventFn = Arc<dyn Fn(BrainEvent) + Send + Sync>;

impl Brain {
    /// `diagnostics`, quando presente, vira a ferramenta `diagnosticar_pc` do LLM.
    pub fn start(
        cfg: BrainConfig,
        status: SharedStatus,
        diagnostics: Option<Diagnostics>,
        on_event: impl Fn(BrainEvent) + Send + Sync + 'static,
    ) -> Self {
        let on_event: EventFn = Arc::new(on_event);
        let mem = Memory::open_default().unwrap_or_else(|e| {
            log::error!("não consegui abrir a memória: {e:#}; usando memória temporária");
            Memory::in_memory().expect("sqlite em memória")
        });
        let mem = Arc::new(Mutex::new(mem));

        let (wake_tx, wake_rx) = mpsc::channel();
        let ev = on_event.clone();
        reminders::spawn_scheduler(mem.clone(), wake_rx, move |text| ev(BrainEvent::Reminder(text)));

        let (tx, rx) = mpsc::channel();
        let mut worker = Worker::new(cfg, mem, status, wake_tx, None);
        worker.diagnostics = diagnostics;
        std::thread::Builder::new()
            .name("cerebro".into())
            .spawn(move || worker.run(rx, on_event))
            .expect("falha ao iniciar a thread do cérebro");
        Self { tx }
    }

    /// Envia um pedido do usuário (digitado ou transcrito da voz). A resposta chega como
    /// `BrainEvent::Reply`.
    pub fn ask(&self, text: impl Into<String>) {
        let _ = self.tx.send(Job::Ask(text.into()));
    }

    /// Avisa que o modo jogo ligou/desligou (o Ollama descarrega o modelo da VRAM).
    pub fn set_game_mode(&self, on: bool) {
        let _ = self.tx.send(Job::GameMode(on));
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(Job::Quit);
    }
}

const PENDING_TTL: Duration = Duration::from_secs(90);
const MAX_TOOL_ROUNDS: usize = 5;

/// Ação destrutiva esperando o "sim" do usuário.
pub(crate) struct Pending {
    targets: Vec<ProcessMatch>,
    force: bool,
    created: Instant,
}

impl Pending {
    fn new(targets: Vec<ProcessMatch>, force: bool) -> Self {
        Self { targets, force, created: Instant::now() }
    }

    fn expired(&self) -> bool {
        self.created.elapsed() > PENDING_TTL
    }
}

pub(crate) struct Worker {
    cfg: BrainConfig,
    mem: Arc<Mutex<Memory>>,
    apps: AppIndex,
    routines: BTreeMap<String, Vec<String>>,
    pending: Option<Pending>,
    status: SharedStatus,
    wake_reminders: Sender<()>,
    game_mode: bool,
    llm: Option<Box<dyn Llm>>,
    diagnostics: Option<Diagnostics>,
}

impl Worker {
    fn new(
        cfg: BrainConfig,
        mem: Arc<Mutex<Memory>>,
        status: SharedStatus,
        wake_reminders: Sender<()>,
        llm: Option<Box<dyn Llm>>,
    ) -> Self {
        Self {
            apps: AppIndex::new(cfg.apps.clone()),
            routines: cfg.routines.clone(),
            cfg,
            mem,
            pending: None,
            status,
            wake_reminders,
            game_mode: false,
            llm,
            diagnostics: None,
        }
    }

    fn run(mut self, rx: Receiver<Job>, on_event: EventFn) {
        while let Ok(job) = rx.recv() {
            match job {
                Job::Quit => break,
                Job::GameMode(on) => {
                    self.game_mode = on;
                    if on && self.cfg.provider == Provider::Ollama {
                        if let Some(llm) = self.llm.as_mut() {
                            llm.unload();
                        }
                    }
                }
                Job::Ask(text) => {
                    let started = Instant::now();
                    match self.handle(&text, &*on_event) {
                        Ok(reply) => on_event(BrainEvent::Reply(reply)),
                        Err(e) => {
                            log::warn!("cérebro: {e:#}");
                            on_event(BrainEvent::Error(format!("Não consegui responder: {e:#}")));
                        }
                    }
                    log::info!("pedido respondido em {:?}", started.elapsed());
                }
            }
        }
    }

    fn handle(&mut self, text: &str, on_event: &dyn Fn(BrainEvent)) -> Result<String> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(String::new());
        }
        let reply = match self.quick(text) {
            Some(r) => r,
            None => {
                on_event(BrainEvent::Thinking);
                self.ask_llm(text)?
            }
        };
        let mem = self.mem.lock().unwrap();
        mem.add_turn("user", text)?;
        mem.add_turn("assistant", &reply)?;
        Ok(reply)
    }

    /// Tenta resolver sem LLM. `None` = não é um comando simples.
    fn quick(&mut self, text: &str) -> Option<String> {
        let names: Vec<String> = self.routines.keys().cloned().collect();
        let intent = quick::parse(text, &names)?;
        let result = match intent {
            Intent::Confirm(yes) => return self.confirm_pending(yes),
            Intent::Time => Ok(now_text()),
            Intent::Remind { text, minutes, at } => match self.add_reminder(&text, minutes, at.as_deref()) {
                Ok(s) => Ok(s),
                // Horário que não entendi: deixa o LLM perguntar.
                Err(_) => return None,
            },
            Intent::Act(action) => self.run_action(action, true),
        };
        Some(match result {
            Ok(s) => s.strip_prefix(tools::CONFIRM_PREFIX).map(str::to_string).unwrap_or(s),
            Err(e) => capitalize(&format!("{e:#}.")),
        })
    }

    fn system_prompt(&self) -> String {
        let mut p = String::from(
            "Você é a Sexta-Feira (F.R.I.D.A.Y.), a assistente pessoal que vive no PC do usuário, inspirada na IA do \
             Homem de Ferro: calma, competente e com um toque de bom humor. Responda sempre em português do Brasil, em \
             1 a 3 frases curtas e naturais para serem faladas em voz alta: sem markdown, listas ou emojis. \
             Use as ferramentas para agir no PC e nunca diga que fez algo sem ter chamado a ferramenta. Fechar programas \
             sempre exige confirmação: quando a ferramenta pedir, repita a pergunta ao usuário e pare. Se faltar \
             informação (ex.: horário de um lembrete), pergunte. Quando o usuário contar algo duradouro sobre ele ou o PC \
             (jogos favoritos, horários, preferências), guarde com lembrar_fato.\n",
        );
        p.push_str(&format!("\nAgora: {}.", now_text()));
        if self.game_mode {
            p.push_str("\nO usuário está jogando agora (modo jogo): seja ainda mais breve.");
        }
        p.push_str(&format!("\nPC agora: {}", self.status_text()));
        if !self.routines.is_empty() {
            let names: Vec<_> = self.routines.keys().cloned().collect();
            p.push_str(&format!("\nRotinas disponíveis: {}.", names.join(", ")));
        }
        if let Ok(facts) = self.mem.lock().unwrap().facts() {
            if !facts.is_empty() {
                p.push_str("\nO que você sabe sobre o usuário:");
                for f in facts.iter().rev().take(40).rev() {
                    p.push_str(&format!("\n- [{}] {}", f.id, f.text));
                }
            }
        }
        p
    }

    fn status_text(&self) -> String {
        let guard = self.status.lock().unwrap();
        let Some(s) = guard.as_ref() else { return "ainda sem leitura do monitor.".into() };
        let mut out = format!(
            "CPU {:.0}%{}, RAM {:.1} de {:.1} GB ({:.0}%)",
            s.cpu_pct,
            s.cpu_temp_c.map(|t| format!(" a {t:.0} °C")).unwrap_or_default(),
            s.ram_used_mb as f32 / 1024.0,
            s.ram_total_mb as f32 / 1024.0,
            s.ram_pct()
        );
        match &s.gpu {
            Some(g) => out.push_str(&format!(
                ", GPU {} {:.0}% a {:.0} °C com {}/{} MB de VRAM",
                g.name, g.usage_pct, g.temp_c, g.vram_used_mb, g.vram_total_mb
            )),
            None => out.push_str(", sem GPU NVIDIA detectada"),
        }
        out.push('.');
        if !s.top_processes.is_empty() {
            let top: Vec<String> = s.top_processes.iter().take(5).map(|p| format!("{} {} MB", p.name, p.mem_mb)).collect();
            out.push_str(&format!(" Mais pesados: {}.", top.join(", ")));
        }
        for a in &s.alerts {
            let tag = if a.severity == Severity::Critical { "CRÍTICO" } else { "Atenção" };
            out.push_str(&format!(" {tag}: {}.", a.message));
        }
        out
    }

    fn ask_llm(&mut self, text: &str) -> Result<String> {
        // Um pedido novo cancela confirmações que ficaram para trás.
        self.pending = None;
        let mut msgs = vec![Msg::System(self.system_prompt())];
        for t in self.mem.lock().unwrap().recent_turns(self.cfg.history_messages)? {
            msgs.push(if t.role == "user" {
                Msg::User(t.content)
            } else {
                Msg::Assistant { content: t.content, tool_calls: Vec::new() }
            });
        }
        msgs.push(Msg::User(text.to_string()));

        let specs = tools::specs(self.diagnostics.is_some());
        let game_mode = self.game_mode;
        let mut last_tool_result = String::new();
        for _ in 0..MAX_TOOL_ROUNDS {
            let llm = self.llm.get_or_insert_with(|| llm::from_config(&self.cfg));
            let reply = llm.chat(&msgs, &specs, game_mode)?;
            if reply.tool_calls.is_empty() {
                let answer = strip_think(&reply.content);
                return Ok(if answer.is_empty() { fallback(&last_tool_result) } else { answer });
            }
            let calls = reply.tool_calls.clone();
            msgs.push(Msg::Assistant { content: reply.content, tool_calls: reply.tool_calls });
            for c in calls {
                log::info!("ferramenta {} {}", c.name, c.args);
                let result = self.run_tool(&c.name, &c.args);
                last_tool_result = result.clone();
                msgs.push(Msg::Tool { call_id: c.id, name: c.name, content: result });
            }
        }
        Ok(fallback(&last_tool_result))
    }
}

fn fallback(tool_result: &str) -> String {
    match tool_result.strip_prefix(tools::CONFIRM_PREFIX) {
        Some(q) => q.to_string(),
        None if tool_result.is_empty() || tool_result.starts_with("ERRO") => "Feito.".into(),
        None => tool_result.to_string(),
    }
}

/// Remove o raciocínio `<think>...</think>` que alguns modelos locais devolvem.
fn strip_think(s: &str) -> String {
    let mut out = s.to_string();
    while let (Some(a), Some(b)) = (out.find("<think>"), out.find("</think>")) {
        if b < a {
            break;
        }
        out.replace_range(a..b + "</think>".len(), "");
    }
    out.trim().to_string()
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn now_text() -> String {
    const DIAS: [&str; 7] = ["segunda-feira", "terça-feira", "quarta-feira", "quinta-feira", "sexta-feira", "sábado", "domingo"];
    let n = Local::now();
    format!(
        "{}, {:02}/{:02}/{}, {:02}:{:02}",
        DIAS[n.weekday().num_days_from_monday() as usize],
        n.day(),
        n.month(),
        n.year(),
        n.hour(),
        n.minute()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm::{Reply, ToolCall};
    use serde_json::{json, Value};

    /// LLM de mentira que devolve respostas prontas, na ordem.
    struct Scripted(Vec<Reply>, Arc<Mutex<Vec<Vec<Msg>>>>);

    impl Llm for Scripted {
        fn chat(&mut self, msgs: &[Msg], _tools: &Value, _game: bool) -> Result<Reply> {
            self.1.lock().unwrap().push(msgs.to_vec());
            Ok(if self.0.is_empty() { Reply::default() } else { self.0.remove(0) })
        }
    }

    fn worker(replies: Vec<Reply>) -> (Worker, Arc<Mutex<Vec<Vec<Msg>>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mem = Arc::new(Mutex::new(Memory::in_memory().unwrap()));
        let (wake, _rx) = mpsc::channel();
        let llm: Box<dyn Llm> = Box::new(Scripted(replies, seen.clone()));
        (Worker::new(BrainConfig::default(), mem, Arc::new(Mutex::new(None)), wake, Some(llm)), seen)
    }

    fn call(name: &str, args: Value) -> Reply {
        Reply { content: String::new(), tool_calls: vec![ToolCall { id: "1".into(), name: name.into(), args }] }
    }

    #[test]
    fn lembrete_pelo_atalho_nao_chama_llm() {
        let (mut w, seen) = worker(vec![]);
        let r = w.handle("me lembra de beber água em 15 minutos", &|_| {}).unwrap();
        assert!(r.starts_with("Combinado, te lembro hoje às") || r.starts_with("Combinado, te lembro amanhã às"), "{r}");
        assert!(seen.lock().unwrap().is_empty());
        assert_eq!(w.mem.lock().unwrap().pending_reminders().unwrap().len(), 1);
    }

    #[test]
    fn llm_usa_ferramenta_e_responde() {
        let (mut w, seen) = worker(vec![
            call("criar_lembrete", json!({"texto": "ligar pro João", "em_minutos": 30})),
            Reply { content: "<think>ok</think>Pode deixar, te lembro em meia hora.".into(), tool_calls: vec![] },
        ]);
        let r = w.handle("preciso ligar pro João daqui a pouco, uns 30 min", &|_| {}).unwrap();
        assert_eq!(r, "Pode deixar, te lembro em meia hora.");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        // A segunda chamada leva o resultado da ferramenta.
        assert!(matches!(seen[1].last(), Some(Msg::Tool { name, content, .. }) if name == "criar_lembrete" && content.starts_with("Combinado")));
        // E o histórico fica salvo para a próxima conversa.
        assert_eq!(w.mem.lock().unwrap().recent_turns(10).unwrap().len(), 2);
    }

    #[test]
    fn fatos_vao_para_o_prompt() {
        let (mut w, seen) = worker(vec![
            call("lembrar_fato", json!({"fato": "Joga CS2 toda noite"})),
            Reply { content: "Anotado.".into(), tool_calls: vec![] },
            Reply { content: "Você joga CS2.".into(), tool_calls: vec![] },
        ]);
        w.handle("eu jogo cs2 toda noite", &|_| {}).unwrap();
        w.handle("o que eu jogo?", &|_| {}).unwrap();
        let seen = seen.lock().unwrap();
        let Msg::System(sys) = &seen[2][0] else { panic!() };
        assert!(sys.contains("Joga CS2 toda noite"));
    }

    #[test]
    fn sim_sem_pendencia_vai_para_o_llm() {
        let (mut w, seen) = worker(vec![Reply { content: "Sim o quê?".into(), tool_calls: vec![] }]);
        assert_eq!(w.handle("sim", &|_| {}).unwrap(), "Sim o quê?");
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn think_e_removido() {
        assert_eq!(strip_think("<think>\nhmm\n</think>\n\nOi!"), "Oi!");
        assert_eq!(strip_think("Oi!"), "Oi!");
    }
}

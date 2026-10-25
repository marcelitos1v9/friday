//! Voz da Sexta-Feira: ouvir (palavra de ativação ou push-to-talk), transcrever e falar.
//!
//! Interface para o resto do app:
//! - `Voice::start(cfg, on_event)` cria a thread de voz.
//! - `VoiceEvent::Transcript(texto)` entrega o que o usuário falou (o cérebro consome isso).
//! - `voice.speak(texto)` fala uma resposta.
//! - `voice.set_game_mode(true)` desliga a escuta contínua e troca para a voz leve.

mod audio;
mod server;
mod wake;
mod wav;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::VoiceConfig;
use audio::Mic;
use server::VoiceServer;
use wake::WakeWord;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceState {
    Idle,
    Listening,
    Transcribing,
    Speaking,
}

impl VoiceState {
    pub fn label(self) -> &'static str {
        match self {
            VoiceState::Idle => "",
            VoiceState::Listening => "Ouvindo…",
            VoiceState::Transcribing => "Entendendo…",
            VoiceState::Speaking => "Falando…",
        }
    }
}

#[derive(Debug, Clone)]
pub enum VoiceEvent {
    State(VoiceState),
    /// Frase que o usuário falou, já transcrita.
    Transcript(String),
    /// Mensagem para mostrar na interface (treino, avisos).
    Info(String),
    Error(String),
    /// Se a palavra de ativação está treinada e ativa.
    WakeWord(bool),
}

enum Msg {
    Audio(Vec<f32>),
    Speak(String),
    #[allow(dead_code)]
    StopSpeaking,
    PushToTalk,
    GameMode(bool),
    Train,
    PlaybackDone,
    Quit,
}

#[derive(Clone)]
pub struct Voice {
    tx: Sender<Msg>,
    stop: Arc<AtomicBool>,
}

impl Voice {
    pub fn start(cfg: VoiceConfig, on_event: impl Fn(VoiceEvent) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_tx = tx.clone();
        let worker_stop = stop.clone();
        std::thread::Builder::new()
            .name("voz".into())
            .spawn(move || {
                let mut w = Worker::new(cfg, Box::new(on_event), worker_tx, worker_stop);
                w.run(rx);
            })
            .expect("falha ao iniciar a thread de voz");
        Self { tx, stop }
    }

    pub fn speak(&self, text: impl Into<String>) {
        let _ = self.tx.send(Msg::Speak(text.into()));
    }

    /// Interrompe a fala atual e descarta o que estava na fila (usado pelo cérebro na etapa M2).
    #[allow(dead_code)]
    pub fn stop_speaking(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Msg::StopSpeaking);
    }

    /// Começa a ouvir um comando agora (ou termina, se já estiver ouvindo).
    pub fn push_to_talk(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Msg::PushToTalk);
    }

    pub fn set_game_mode(&self, on: bool) {
        let _ = self.tx.send(Msg::GameMode(on));
    }

    /// Grava três vezes "Sexta-Feira" e cria o modelo da palavra de ativação.
    pub fn train_wake_word(&self) {
        let _ = self.tx.send(Msg::Train);
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Msg::Quit);
    }
}

#[derive(PartialEq, Eq)]
enum Purpose {
    Command,
    Training,
}

struct Recording {
    buf: Vec<f32>,
    started: Instant,
    last_voice: Instant,
    heard: bool,
    purpose: Purpose,
}

impl Recording {
    fn new(purpose: Purpose) -> Self {
        let now = Instant::now();
        Self {
            buf: Vec::new(),
            started: now,
            last_voice: now,
            heard: false,
            purpose,
        }
    }
}

enum Outcome {
    Continue,
    Done,
    Nothing,
}

const SILENCE_END: Duration = Duration::from_millis(900);
const MAX_UTTERANCE: Duration = Duration::from_secs(15);
const NO_SPEECH_TIMEOUT: Duration = Duration::from_secs(6);

struct Worker {
    cfg: VoiceConfig,
    emit: Box<dyn Fn(VoiceEvent) + Send>,
    tx: Sender<Msg>,
    stop: Arc<AtomicBool>,
    mic: Option<Mic>,
    wake: Option<WakeWord>,
    server: VoiceServer,
    http: ureq::Agent,
    game: bool,
    recording: Option<Recording>,
    training: Vec<Vec<f32>>,
    speaking: bool,
    queue: VecDeque<String>,
    noise: f32,
}

impl Worker {
    fn new(
        cfg: VoiceConfig,
        emit: Box<dyn Fn(VoiceEvent) + Send>,
        tx: Sender<Msg>,
        stop: Arc<AtomicBool>,
    ) -> Self {
        let http: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .into();
        let mic = if cfg.enabled {
            match Mic::open(tx.clone(), Msg::Audio) {
                Ok(m) => Some(m),
                Err(e) => {
                    emit(VoiceEvent::Error(format!("Microfone indisponível: {e}")));
                    None
                }
            }
        } else {
            None
        };
        let wake = if cfg.wake_word {
            WakeWord::load(cfg.wake_threshold)
        } else {
            None
        };
        emit(VoiceEvent::WakeWord(wake.is_some()));
        if mic.is_some() && cfg.wake_word && wake.is_none() {
            emit(VoiceEvent::Info(
                "Palavra de ativação ainda não treinada. Use \"Treinar voz\" ou Ctrl+Alt+Espaço para falar.".into(),
            ));
        }
        Self {
            server: VoiceServer::new(cfg.clone()),
            cfg,
            emit,
            tx,
            stop,
            mic,
            wake,
            http,
            game: false,
            recording: None,
            training: Vec::new(),
            speaking: false,
            queue: VecDeque::new(),
            noise: 0.005,
        }
    }

    fn run(&mut self, rx: mpsc::Receiver<Msg>) {
        self.update_mic();
        loop {
            let timeout = if self.server.is_running() {
                Duration::from_secs(30)
            } else {
                Duration::from_secs(3600)
            };
            match rx.recv_timeout(timeout) {
                Ok(Msg::Audio(chunk)) => self.on_audio(&chunk),
                Ok(Msg::Speak(text)) => {
                    self.queue.extend(split_sentences(&clean_for_speech(&text)));
                    if !self.speaking {
                        self.speak_next();
                    }
                }
                Ok(Msg::StopSpeaking) => self.queue.clear(),
                Ok(Msg::PlaybackDone) => {
                    self.speaking = false;
                    self.speak_next();
                }
                Ok(Msg::PushToTalk) => self.on_push_to_talk(),
                Ok(Msg::GameMode(on)) => self.on_game_mode(on),
                Ok(Msg::Train) => {
                    self.training.clear();
                    self.recording = Some(Recording::new(Purpose::Training));
                    (self.emit)(VoiceEvent::Info(format!(
                        "Treino: diga \"Sexta-Feira\" (1/{})",
                        wake::TRAINING_SAMPLES
                    )));
                    (self.emit)(VoiceEvent::State(VoiceState::Listening));
                    self.update_mic();
                }
                Ok(Msg::Quit) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    self.server.stop_if_idle();
                }
            }
        }
        self.server.stop();
    }

    /// Liga o microfone só quando alguém precisa ouvir.
    fn update_mic(&mut self) {
        let wake_active = self.wake.is_some() && self.cfg.wake_word && !self.game && !self.speaking;
        let want = self.recording.is_some() || wake_active;
        if let Some(mic) = &mut self.mic {
            if mic.is_running() != want {
                log::debug!("microfone {}", if want { "ligado" } else { "pausado" });
            }
            mic.set_running(want);
        }
    }

    fn on_audio(&mut self, chunk: &[f32]) {
        if self.recording.is_some() {
            match self.record(chunk) {
                Outcome::Continue => {}
                Outcome::Done => self.finish_recording(),
                Outcome::Nothing => {
                    self.recording = None;
                    (self.emit)(VoiceEvent::State(VoiceState::Idle));
                    self.update_mic();
                }
            }
            return;
        }
        if self.speaking || self.game {
            return;
        }
        // Atualiza o ruído de fundo para o detector de silêncio.
        let level = wav::rms(chunk);
        self.noise = self.noise * 0.95 + level.min(0.05) * 0.05;
        if let Some(w) = &mut self.wake {
            if w.feed(chunk, self.noise) {
                self.recording = Some(Recording::new(Purpose::Command));
                (self.emit)(VoiceEvent::State(VoiceState::Listening));
            }
        }
    }

    fn record(&mut self, chunk: &[f32]) -> Outcome {
        let threshold = (self.noise * 3.0).max(0.015);
        let rec = self.recording.as_mut().expect("gravação ativa");
        rec.buf.extend_from_slice(chunk);
        if wav::rms(chunk) > threshold {
            rec.heard = true;
            rec.last_voice = Instant::now();
        }
        if rec.heard && rec.last_voice.elapsed() >= SILENCE_END {
            Outcome::Done
        } else if rec.started.elapsed() >= MAX_UTTERANCE {
            if rec.heard {
                Outcome::Done
            } else {
                Outcome::Nothing
            }
        } else if !rec.heard && rec.started.elapsed() >= NO_SPEECH_TIMEOUT {
            Outcome::Nothing
        } else {
            Outcome::Continue
        }
    }

    fn finish_recording(&mut self) {
        let Some(rec) = self.recording.take() else {
            return;
        };
        let audio = trim_silence(&rec.buf, (self.noise * 3.0).max(0.015));
        match rec.purpose {
            Purpose::Training => self.on_training_sample(audio),
            Purpose::Command => {
                self.update_mic();
                (self.emit)(VoiceEvent::State(VoiceState::Transcribing));
                match self.transcribe(&audio) {
                    Ok(text) if !text.is_empty() => (self.emit)(VoiceEvent::Transcript(text)),
                    Ok(_) => {
                        (self.emit)(VoiceEvent::Info("Não entendi nada. Pode repetir?".into()))
                    }
                    Err(e) => {
                        (self.emit)(VoiceEvent::Error(format!("Falha ao transcrever: {e:#}")))
                    }
                }
                (self.emit)(VoiceEvent::State(VoiceState::Idle));
            }
        }
    }

    fn on_training_sample(&mut self, audio: Vec<f32>) {
        let secs = audio.len() as f32 / wav::RATE as f32;
        if !(0.3..=3.0).contains(&secs) {
            (self.emit)(VoiceEvent::Info(
                "Essa gravação não ficou boa. Diga só \"Sexta-Feira\", de novo.".into(),
            ));
        } else {
            self.training.push(audio);
        }
        if self.training.len() < wake::TRAINING_SAMPLES {
            (self.emit)(VoiceEvent::Info(format!(
                "Treino: diga \"Sexta-Feira\" ({}/{})",
                self.training.len() + 1,
                wake::TRAINING_SAMPLES
            )));
            self.recording = Some(Recording::new(Purpose::Training));
            return;
        }
        match wake::train(&self.training, self.cfg.wake_threshold) {
            Ok(w) => {
                self.wake = Some(w);
                (self.emit)(VoiceEvent::WakeWord(true));
                (self.emit)(VoiceEvent::Info(
                    "Pronto! Agora é só dizer \"Sexta-Feira\".".into(),
                ));
            }
            Err(e) => (self.emit)(VoiceEvent::Error(format!("Não consegui treinar: {e:#}"))),
        }
        self.training.clear();
        (self.emit)(VoiceEvent::State(VoiceState::Idle));
        self.update_mic();
    }

    fn transcribe(&mut self, audio: &[f32]) -> anyhow::Result<String> {
        let wav = wav::encode(audio, wav::RATE)?;
        if self.cfg.stt == "groq" {
            match self.cfg.groq_key() {
                Some(key) => {
                    return server::groq_stt(&self.http, &key, &self.cfg.groq_stt_model, &wav)
                }
                None => log::warn!("GROQ_API_KEY não configurada; usando o Whisper local"),
            }
        }
        self.server.stt(wav)
    }

    fn on_push_to_talk(&mut self) {
        self.queue.clear();
        match &self.recording {
            Some(r) if r.purpose == Purpose::Command => {
                if r.heard {
                    self.finish_recording();
                } else {
                    self.recording = None;
                    (self.emit)(VoiceEvent::State(VoiceState::Idle));
                    self.update_mic();
                }
            }
            Some(_) => {}
            None => {
                if self.mic.is_none() {
                    (self.emit)(VoiceEvent::Error("Nenhum microfone disponível.".into()));
                    return;
                }
                self.recording = Some(Recording::new(Purpose::Command));
                (self.emit)(VoiceEvent::State(VoiceState::Listening));
                self.update_mic();
            }
        }
    }

    fn on_game_mode(&mut self, on: bool) {
        if self.game == on {
            return;
        }
        self.game = on;
        if on {
            // Libera a GPU para o jogo; a voz passa para o Kokoro, que roda na CPU.
            self.server.unload_heavy();
            if let Some(w) = &mut self.wake {
                w.reset();
            }
        }
        self.update_mic();
    }

    fn speak_next(&mut self) {
        let engine = if self.game {
            &self.cfg.tts_engine_game
        } else {
            &self.cfg.tts_engine
        }
        .clone();
        if engine == "off" {
            self.queue.clear();
            return;
        }
        let Some(text) = self.queue.pop_front() else {
            if self.recording.is_none() {
                (self.emit)(VoiceEvent::State(VoiceState::Idle));
            }
            self.update_mic();
            return;
        };
        self.stop.store(false, Ordering::Relaxed);
        let wav = self.server.tts(&text, &engine).or_else(|e| {
            if engine != "kokoro" {
                log::warn!("{e:#}; tentando com o Kokoro");
                self.server.tts(&text, "kokoro")
            } else {
                Err(e)
            }
        });
        let (samples, rate) = match wav.and_then(|w| wav::decode(&w)) {
            Ok(v) => v,
            Err(e) => {
                self.queue.clear();
                (self.emit)(VoiceEvent::Error(format!("Não consegui falar: {e:#}")));
                (self.emit)(VoiceEvent::State(VoiceState::Idle));
                return;
            }
        };
        self.speaking = true;
        self.update_mic();
        (self.emit)(VoiceEvent::State(VoiceState::Speaking));
        let (tx, stop) = (self.tx.clone(), self.stop.clone());
        std::thread::spawn(move || {
            if let Err(e) = audio::play_blocking(&samples, rate, stop) {
                log::warn!("falha ao tocar áudio: {e:#}");
            }
            let _ = tx.send(Msg::PlaybackDone);
        });
    }
}

/// Remove silêncio do começo e do fim, mantendo uma pequena margem.
fn trim_silence(samples: &[f32], threshold: f32) -> Vec<f32> {
    const WIN: usize = 320; // 20 ms
    let loud = |c: &[f32]| wav::rms(c) > threshold;
    let chunks: Vec<&[f32]> = samples.chunks(WIN).collect();
    let first = chunks.iter().position(|c| loud(c)).unwrap_or(0);
    let last = chunks
        .iter()
        .rposition(|c| loud(c))
        .unwrap_or(chunks.len().saturating_sub(1));
    let start = first.saturating_sub(10) * WIN;
    let end = ((last + 10) * WIN).min(samples.len());
    samples[start.min(end)..end].to_vec()
}

/// Tira marcações que não devem ser lidas em voz alta.
fn clean_for_speech(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '*' | '#' | '`' | '_' | '>' | '|'))
        .collect()
}

/// Quebra o texto em frases, para começar a falar antes de sintetizar tudo.
fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        cur.push(ch);
        if matches!(ch, '.' | '!' | '?' | '\n') && cur.trim().chars().count() >= 20 {
            out.push(cur.trim().to_string());
            cur.clear();
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_into_sentences_without_tiny_fragments() {
        let parts = split_sentences("Ok. Abrindo a Steam agora para você. Mais alguma coisa?");
        assert_eq!(
            parts,
            vec!["Ok. Abrindo a Steam agora para você.", "Mais alguma coisa?"]
        );
    }

    #[test]
    fn trims_leading_and_trailing_silence() {
        let mut s = vec![0.0; 16_000];
        s.extend(vec![0.5; 3_200]);
        s.extend(vec![0.0; 16_000]);
        let t = trim_silence(&s, 0.05);
        assert!(t.len() < 10_000 && t.len() >= 3_200);
    }

    #[test]
    fn removes_markdown_for_speech() {
        assert_eq!(clean_for_speech("**CPU** em `90%`"), "CPU em 90%");
    }
}

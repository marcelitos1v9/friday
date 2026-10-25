//! Palavra de ativação "Sexta-Feira", feita em Rust puro e com custo muito baixo.
//!
//! Não existe um modelo pronto em português, então a palavra é treinada uma vez pelo próprio
//! app: você fala "Sexta-Feira" três vezes. Cada gravação vira uma sequência de MFCCs
//! (o "formato" da voz) e a detecção compara o áudio recente com esses modelos usando DTW,
//! que tolera falar mais rápido ou mais devagar. O DTW só roda quando há voz no microfone.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::anyhow;
use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use serde::{Deserialize, Serialize};

use super::wav::{rms, RATE};

pub const TRAINING_SAMPLES: usize = 3;

const FRAME: usize = 400; // 25 ms
const HOP: usize = 160; // 10 ms
const NFFT: usize = 512;
const MELS: usize = 26;
const COEFFS: usize = 12;
/// Roda o DTW a cada 5 quadros (50 ms) para economizar CPU.
const CHECK_EVERY: usize = 5;

type Feature = [f32; COEFFS];

pub fn model_path() -> PathBuf {
    crate::config::Config::data_dir().join("sexta-feira-wake.json")
}

#[derive(Serialize, Deserialize)]
struct Model {
    templates: Vec<Vec<Feature>>,
    /// Custo DTW médio entre as próprias gravações de treino.
    reference_cost: f32,
}

/// Extrator de MFCC reutilizável.
struct Mfcc {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    filters: Vec<Vec<(usize, f32)>>,
    scratch: Vec<Complex32>,
}

impl Mfcc {
    fn new() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(NFFT);
        let window = (0..FRAME)
            .map(|i| {
                0.54 - 0.46 * (2.0 * std::f32::consts::PI * i as f32 / (FRAME - 1) as f32).cos()
            })
            .collect();
        Self {
            fft,
            window,
            filters: mel_filters(),
            scratch: vec![Complex32::default(); NFFT],
        }
    }

    /// Retorna (MFCC, energia RMS) de um quadro de `FRAME` amostras.
    fn frame(&mut self, samples: &[f32]) -> (Feature, f32) {
        for (i, c) in self.scratch.iter_mut().enumerate() {
            let s = if i < FRAME {
                samples[i] * self.window[i]
            } else {
                0.0
            };
            *c = Complex32::new(s, 0.0);
        }
        self.fft.process(&mut self.scratch);
        let power: Vec<f32> = self.scratch[..NFFT / 2 + 1]
            .iter()
            .map(|c| c.norm_sqr())
            .collect();
        let log_mel: Vec<f32> = self
            .filters
            .iter()
            .map(|f| (f.iter().map(|(i, w)| power[*i] * w).sum::<f32>() + 1e-10).ln())
            .collect();
        // DCT-II, descartando o coeficiente 0 (volume) para ficar independente da distância do microfone.
        let mut out = [0.0; COEFFS];
        for (k, o) in out.iter_mut().enumerate() {
            let k = k + 1;
            *o = log_mel
                .iter()
                .enumerate()
                .map(|(n, v)| {
                    v * (std::f32::consts::PI * k as f32 * (n as f32 + 0.5) / MELS as f32).cos()
                })
                .sum();
        }
        (out, rms(samples))
    }

    fn sequence(&mut self, samples: &[f32]) -> Vec<Feature> {
        let mut frames = Vec::new();
        let mut i = 0;
        while i + FRAME <= samples.len() {
            frames.push(self.frame(&samples[i..i + FRAME]));
            i += HOP;
        }
        voiced(&frames, 0.0)
    }
}

fn mel_filters() -> Vec<Vec<(usize, f32)>> {
    let hz_to_mel = |hz: f32| 2595.0 * (1.0 + hz / 700.0).log10();
    let mel_to_hz = |m: f32| 700.0 * (10f32.powf(m / 2595.0) - 1.0);
    let (lo, hi) = (hz_to_mel(80.0), hz_to_mel(RATE as f32 / 2.0));
    let bins: Vec<usize> = (0..MELS + 2)
        .map(|i| {
            let hz = mel_to_hz(lo + (hi - lo) * i as f32 / (MELS + 1) as f32);
            ((NFFT + 1) as f32 * hz / RATE as f32) as usize
        })
        .collect();
    (0..MELS)
        .map(|m| {
            let (a, b, c) = (bins[m], bins[m + 1], bins[m + 2]);
            let mut f = Vec::new();
            for k in a..c.max(a + 1) {
                let w = if k < b {
                    (k - a) as f32 / (b - a).max(1) as f32
                } else {
                    (c - k) as f32 / (c - b).max(1) as f32
                };
                if w > 0.0 {
                    f.push((k.min(NFFT / 2), w));
                }
            }
            f
        })
        .collect()
}

/// Recorta o trecho com voz (com a mesma regra no treino e na detecção) e normaliza.
fn voiced(frames: &[(Feature, f32)], gate: f32) -> Vec<Feature> {
    let peak = frames.iter().map(|(_, e)| *e).fold(0.0, f32::max);
    let gate = gate.max(peak * 0.1);
    let first = frames.iter().position(|(_, e)| *e > gate).unwrap_or(0);
    let last = frames.iter().rposition(|(_, e)| *e > gate).unwrap_or(0);
    let mut seq: Vec<Feature> = frames[first..=last.max(first).min(frames.len().saturating_sub(1))]
        .iter()
        .map(|(f, _)| *f)
        .collect();
    normalize(&mut seq);
    seq
}

/// Normalização de média cepstral: remove o "colorido" fixo do microfone e da sala.
fn normalize(seq: &mut [Feature]) {
    if seq.is_empty() {
        return;
    }
    let mut mean = [0.0; COEFFS];
    for f in seq.iter() {
        for (m, v) in mean.iter_mut().zip(f) {
            *m += v / seq.len() as f32;
        }
    }
    for f in seq.iter_mut() {
        for (v, m) in f.iter_mut().zip(&mean) {
            *v -= m;
        }
    }
}

fn dist(a: &Feature, b: &Feature) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y) * (x - y))
        .sum::<f32>()
        .sqrt()
}

/// DTW com custo normalizado pelo tamanho do caminho.
fn dtw(a: &[Feature], b: &[Feature]) -> f32 {
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        return f32::INFINITY;
    }
    let mut prev = vec![(f32::INFINITY, 0u32); m + 1];
    let mut cur = vec![(f32::INFINITY, 0u32); m + 1];
    prev[0] = (0.0, 0);
    for i in 1..=n {
        cur[0] = (f32::INFINITY, 0);
        for j in 1..=m {
            let c = dist(&a[i - 1], &b[j - 1]);
            let best = [prev[j - 1], prev[j], cur[j - 1]]
                .into_iter()
                .min_by(|x, y| x.0.total_cmp(&y.0))
                .unwrap();
            cur[j] = (best.0 + c, best.1 + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let (cost, steps) = prev[m];
    cost / steps.max(1) as f32
}

pub struct WakeWord {
    model: Model,
    threshold: f32,
    mfcc: Mfcc,
    pending: Vec<f32>,
    /// Quadros recentes (MFCC sem normalizar, energia).
    recent: Vec<(Feature, f32)>,
    window_len: usize,
    since_check: usize,
}

impl WakeWord {
    /// Carrega o modelo treinado, se existir.
    pub fn load(sensitivity: f32) -> Option<Self> {
        let text = std::fs::read_to_string(model_path()).ok()?;
        match serde_json::from_str::<Model>(&text) {
            Ok(model) => Some(Self::from_model(model, sensitivity)),
            Err(e) => {
                log::warn!("modelo da palavra de ativação inválido: {e}");
                None
            }
        }
    }

    fn from_model(model: Model, sensitivity: f32) -> Self {
        // Rigor 0.5 = aceita até 40% acima da variação natural entre as gravações de treino.
        let threshold = model.reference_cost * (1.65 - sensitivity.clamp(0.0, 1.0) / 2.0);
        let longest = model.templates.iter().map(Vec::len).max().unwrap_or(100);
        Self {
            model,
            threshold,
            mfcc: Mfcc::new(),
            pending: Vec::new(),
            recent: Vec::new(),
            window_len: longest * 13 / 10,
            since_check: 0,
        }
    }

    /// Alimenta um bloco de áudio a 16 kHz. Retorna true quando ouviu a palavra.
    pub fn feed(&mut self, samples: &[f32], noise: f32) -> bool {
        self.pending.extend_from_slice(samples);
        let mut detected = false;
        while self.pending.len() >= FRAME {
            let frame = self.mfcc.frame(&self.pending[..FRAME]);
            self.pending.drain(..HOP);
            self.recent.push(frame);
            if self.recent.len() > self.window_len {
                self.recent.remove(0);
            }
            self.since_check += 1;
            if self.since_check >= CHECK_EVERY && !detected {
                self.since_check = 0;
                detected = self.check(noise);
            }
        }
        if detected {
            self.reset();
        }
        detected
    }

    fn check(&self, noise: f32) -> bool {
        if self.recent.len() < self.window_len / 2 {
            return false;
        }
        // Só compara se o trecho recente tem voz e acabou de terminar (a palavra inteira cabe na janela).
        let gate = (noise * 3.0).max(0.01);
        let voiced_frames = self.recent.iter().filter(|(_, e)| *e > gate).count();
        let tail_quiet = self
            .recent
            .iter()
            .rev()
            .take(8)
            .all(|(_, e)| *e <= gate * 1.5);
        if voiced_frames < 15 || !tail_quiet {
            return false;
        }
        let seq = voiced(&self.recent, gate);
        let mut costs: Vec<f32> = self.model.templates.iter().map(|t| dtw(t, &seq)).collect();
        costs.sort_by(f32::total_cmp);
        let best_two = costs.iter().take(2).sum::<f32>() / costs.len().min(2) as f32;
        log::debug!(
            "palavra de ativação: custo {:.2} (limite {:.2})",
            best_two,
            self.threshold
        );
        best_two <= self.threshold
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.recent.clear();
        self.since_check = 0;
    }
}

/// Cria e salva o modelo a partir das gravações de treino.
pub fn train(samples: &[Vec<f32>], sensitivity: f32) -> anyhow::Result<WakeWord> {
    if samples.len() < TRAINING_SAMPLES {
        return Err(anyhow!("preciso de {TRAINING_SAMPLES} gravações"));
    }
    let model = build_model(samples)?;
    std::fs::write(model_path(), serde_json::to_string(&model)?)?;
    Ok(WakeWord::from_model(model, sensitivity))
}

fn build_model(samples: &[Vec<f32>]) -> anyhow::Result<Model> {
    let mut mfcc = Mfcc::new();
    let templates: Vec<Vec<Feature>> = samples.iter().map(|s| mfcc.sequence(s)).collect();
    if templates.iter().any(|t| t.len() < 20) {
        return Err(anyhow!("gravações muito curtas"));
    }
    let mut pair = Vec::new();
    for i in 0..templates.len() {
        for j in i + 1..templates.len() {
            pair.push(dtw(&templates[i], &templates[j]));
        }
    }
    let reference_cost = pair.iter().sum::<f32>() / pair.len() as f32;
    Ok(Model {
        templates,
        reference_cost,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sinal sintético com formato de "palavra": sequência de tons com harmônicos.
    fn word(freqs: &[f32], seed: u32) -> Vec<f32> {
        let mut out = Vec::new();
        let mut rng = seed;
        for (k, f) in freqs.iter().enumerate() {
            let len = 2400 + (seed as usize * 37 + k * 11) % 400; // pequenas variações de ritmo
            for i in 0..len {
                rng = rng.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let noise = ((rng >> 16) as f32 / 65_536.0 - 0.5) * 0.01;
                let t = i as f32 / RATE as f32;
                let v = (2.0 * std::f32::consts::PI * f * t).sin() * 0.3
                    + (2.0 * std::f32::consts::PI * f * 2.7 * t).sin() * 0.15;
                out.push(v + noise);
            }
        }
        out
    }

    fn stream_with(word_audio: &[f32]) -> Vec<f32> {
        let mut s = vec![0.0; 8000];
        s.extend_from_slice(word_audio);
        s.extend(vec![0.0; 8000]);
        s
    }

    fn detects(w: &mut WakeWord, audio: &[f32]) -> bool {
        audio.chunks(480).any(|c| w.feed(c, 0.001))
    }

    #[test]
    fn detects_trained_word_and_ignores_other_words() {
        let target = [300.0, 700.0, 450.0, 900.0, 600.0];
        let other = [1200.0, 250.0, 1500.0, 350.0, 1100.0];
        let samples: Vec<Vec<f32>> = (1..=3).map(|s| word(&target, s)).collect();
        let model = build_model(&samples).unwrap();
        let mut w = WakeWord::from_model(model, 0.5);

        assert!(
            detects(&mut w, &stream_with(&word(&target, 7))),
            "não detectou a palavra treinada"
        );
        w.reset();
        assert!(
            !detects(&mut w, &stream_with(&word(&other, 7))),
            "detectou uma palavra diferente"
        );
        w.reset();
        assert!(!detects(&mut w, &vec![0.0; 32_000]), "detectou silêncio");
    }

    #[test]
    fn dtw_is_zero_for_identical_sequences() {
        let seq = vec![[1.0; COEFFS], [2.0; COEFFS], [3.0; COEFFS]];
        assert!(dtw(&seq, &seq) < 1e-6);
    }
}

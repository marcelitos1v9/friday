//! Microfone e alto-falante via cpal.
//!
//! O microfone entrega blocos mono a 16 kHz para a thread de voz. O stream pode ser pausado
//! quando ninguém precisa ouvir (sem palavra de ativação ou no modo jogo), e aí custa zero.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::wav::{resample, RATE};

pub struct Mic {
    stream: cpal::Stream,
    running: bool,
}

impl Mic {
    /// Abre o microfone padrão. Cada bloco capturado vira `make_msg(bloco)` no canal.
    pub fn open<M: Send + 'static>(
        tx: Sender<M>,
        make_msg: fn(Vec<f32>) -> M,
    ) -> anyhow::Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow!("nenhum microfone encontrado"))?;
        let supported = device
            .default_input_config()
            .context("microfone sem configuração padrão")?;
        let config = supported.config();
        let channels = config.channels as usize;
        let rate = config.sample_rate;
        let err = |e| log::warn!("erro no microfone: {e}");

        let to_mono = move |data: &[f32]| -> Vec<f32> {
            let mono: Vec<f32> = data
                .chunks(channels)
                .map(|c| c.iter().sum::<f32>() / channels as f32)
                .collect();
            resample(&mono, rate, RATE)
        };

        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream::<f32, _, _>(
                config,
                move |data, _| {
                    let _ = tx.send(make_msg(to_mono(data)));
                },
                err,
                None,
            )?,
            cpal::SampleFormat::I16 => device.build_input_stream::<i16, _, _>(
                config,
                move |data, _| {
                    let f: Vec<f32> = data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                    let _ = tx.send(make_msg(to_mono(&f)));
                },
                err,
                None,
            )?,
            other => return Err(anyhow!("formato de microfone não suportado: {other:?}")),
        };
        stream.pause().ok();
        Ok(Self {
            stream,
            running: false,
        })
    }

    pub fn set_running(&mut self, on: bool) {
        if on == self.running {
            return;
        }
        let r = if on {
            self.stream.play()
        } else {
            self.stream.pause()
        };
        match r {
            Ok(()) => self.running = on,
            Err(e) => log::warn!(
                "não consegui {} o microfone: {e}",
                if on { "ligar" } else { "pausar" }
            ),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running
    }
}

/// Toca áudio mono no alto-falante padrão, bloqueando até terminar ou até `stop` virar true.
pub fn play_blocking(samples: &[f32], rate: u32, stop: Arc<AtomicBool>) -> anyhow::Result<()> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow!("nenhuma saída de áudio"))?;
    let supported = device.default_output_config()?;
    let config = supported.config();
    let channels = config.channels as usize;
    let data = Arc::new(resample(samples, rate, config.sample_rate));
    let total = data.len();
    let pos = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let err = |e| log::warn!("erro na saída de áudio: {e}");

    let (d, p) = (data.clone(), pos.clone());
    let next = move || {
        let i = p.fetch_add(1, Ordering::Relaxed);
        d.get(i).copied().unwrap_or(0.0)
    };
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_output_stream::<f32, _, _>(
            config,
            move |out, _| {
                for frame in out.chunks_mut(channels) {
                    let v = next();
                    frame.fill(v);
                }
            },
            err,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_output_stream::<i16, _, _>(
            config,
            move |out, _| {
                for frame in out.chunks_mut(channels) {
                    let v = (next() * i16::MAX as f32) as i16;
                    frame.fill(v);
                }
            },
            err,
            None,
        )?,
        other => return Err(anyhow!("formato de saída não suportado: {other:?}")),
    };
    stream.play()?;
    while pos.load(Ordering::Relaxed) < total && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(30));
    }
    // Deixa o último buffer sair antes de fechar o stream.
    std::thread::sleep(Duration::from_millis(120));
    Ok(())
}

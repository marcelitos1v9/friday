//! Conversões de áudio: WAV em memória e reamostragem simples.

use std::io::Cursor;

/// Taxa interna usada para escuta, palavra de ativação e transcrição.
pub const RATE: u32 = 16_000;

/// Codifica amostras mono f32 como WAV PCM 16 bits.
pub fn encode(samples: &[f32], rate: u32) -> anyhow::Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut out = Cursor::new(Vec::with_capacity(samples.len() * 2 + 44));
    {
        let mut w = hound::WavWriter::new(&mut out, spec)?;
        for s in samples {
            w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
        }
        w.finalize()?;
    }
    Ok(out.into_inner())
}

/// Decodifica um WAV (qualquer formato PCM) para mono f32, devolvendo também a taxa.
pub fn decode(bytes: &[u8]) -> anyhow::Result<(Vec<f32>, u32)> {
    let mut r = hound::WavReader::new(Cursor::new(bytes))?;
    let spec = r.spec();
    let channels = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<Result<_, _>>()?
        }
    };
    let mono = interleaved
        .chunks(channels)
        .map(|c| c.iter().sum::<f32>() / channels as f32)
        .collect();
    Ok((mono, spec.sample_rate))
}

/// Reamostragem linear. Suficiente para voz; evita depender de uma biblioteca de DSP.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let len = (input.len() as f64 / ratio) as usize;
    (0..len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let idx = pos as usize;
            let frac = (pos - idx as f64) as f32;
            let a = input[idx.min(input.len() - 1)];
            let b = input[(idx + 1).min(input.len() - 1)];
            a + (b - a) * frac
        })
        .collect()
}

/// Volume médio (RMS) de um trecho.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_roundtrip_keeps_samples() {
        let input: Vec<f32> = (0..1600).map(|i| (i as f32 / 20.0).sin() * 0.5).collect();
        let bytes = encode(&input, RATE).unwrap();
        let (out, rate) = decode(&bytes).unwrap();
        assert_eq!(rate, RATE);
        assert_eq!(out.len(), input.len());
        assert!(input.iter().zip(&out).all(|(a, b)| (a - b).abs() < 0.001));
    }

    #[test]
    fn resample_changes_length_by_ratio() {
        let input = vec![0.0; 48_000];
        assert_eq!(resample(&input, 48_000, 16_000).len(), 16_000);
        assert_eq!(resample(&input, 16_000, 16_000).len(), 48_000);
    }
}

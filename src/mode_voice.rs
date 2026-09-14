//! Offline PCM WAV playback through the same native audio stack as capture.
use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub struct Feedback {
    pub mode: String,
    pub switched: bool,
    pub settings: crate::mode_command::Settings,
}

/// Playback owns no recording handle. Slow output must never stop microphone capture.
pub fn start_feedback_worker() -> tokio::sync::mpsc::Sender<Feedback> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Feedback>(8);
    tokio::spawn(async move {
        while let Some(feedback) = rx.recv().await {
            let result = tokio::task::spawn_blocking(move || {
                if feedback.settings.notification {
                    crate::mode_command::notify(&feedback.mode, feedback.switched);
                }
                if feedback.settings.sound {
                    play(&feedback.mode)
                } else {
                    Ok(())
                }
            })
            .await;
            if !matches!(result, Ok(Ok(()))) {
                tracing::warn!(?result, "模式回饋失敗，錄音繼續");
            }
        }
    });
    tx
}

fn bytes(mode: &str) -> Result<Vec<u8>> {
    let custom = crate::home_dir()
        .join(".mori/mori-ear/voices")
        .join(format!("{mode}.wav"));
    match std::fs::read(&custom) {
        Ok(bytes) => return Ok(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(match mode {
        "auto" => include_bytes!("../assets/voices/auto.wav").as_slice(),
        "groq" => include_bytes!("../assets/voices/groq.wav").as_slice(),
        "local" => include_bytes!("../assets/voices/local.wav").as_slice(),
        "error" => include_bytes!("../assets/voices/error.wav").as_slice(),
        _ => anyhow::bail!("未知模式提示音"),
    }
    .to_vec())
}

fn decode(bytes: Vec<u8>) -> Result<(Vec<f32>, u32)> {
    let mut wav = hound::WavReader::new(std::io::Cursor::new(bytes))?;
    let spec = wav.spec();
    anyhow::ensure!(
        spec.sample_format == hound::SampleFormat::Int
            && spec.bits_per_sample == 16
            && spec.channels > 0
            && spec.sample_rate > 0,
        "提示音需為 16-bit PCM WAV"
    );
    let samples = wav
        .samples::<i16>()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mono: Vec<f32> = samples
        .chunks_exact(spec.channels as usize)
        .map(|frame| frame.iter().map(|s| *s as f32 / 32768.0).sum::<f32>() / spec.channels as f32)
        .collect();
    anyhow::ensure!(
        !mono.is_empty() && mono.len() <= spec.sample_rate as usize * 30,
        "提示音長度必須介於 0 到 30 秒"
    );
    Ok((mono, spec.sample_rate))
}

pub fn play(mode: &str) -> Result<()> {
    let (samples, rate) = decode(bytes(mode)?)?;
    let device = cpal::default_host()
        .default_output_device()
        .context("找不到音訊輸出裝置")?;
    let supported = device.default_output_config()?;
    let config: cpal::StreamConfig = supported.clone().into();
    match supported.sample_format() {
        cpal::SampleFormat::F32 => output::<f32>(&device, &config, samples, rate),
        cpal::SampleFormat::I16 => output::<i16>(&device, &config, samples, rate),
        cpal::SampleFormat::U16 => output::<u16>(&device, &config, samples, rate),
        format => anyhow::bail!("不支援的音訊輸出格式: {format:?}"),
    }
}

fn output<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    samples: Vec<f32>,
    rate: u32,
) -> Result<()> {
    let channels = config.channels as usize;
    let step = rate as f64 / config.sample_rate.0 as f64;
    let duration = samples.len() as f64 / rate as f64;
    let mut cursor = 0.0_f64;
    let (tx, rx) = std::sync::mpsc::channel();
    let errors = tx.clone();
    let mut finished = false;
    let stream = device.build_output_stream(
        config,
        move |out: &mut [T], _| {
            for frame in out.chunks_mut(channels) {
                let i = cursor as usize;
                let value = if let Some(&a) = samples.get(i) {
                    let b = samples.get(i + 1).copied().unwrap_or(a);
                    a + (b - a) * cursor.fract() as f32
                } else {
                    0.0
                };
                frame.fill(T::from_sample(value));
                cursor += step;
            }
            if !finished && cursor >= samples.len() as f64 {
                finished = true;
                let _ = tx.send(Ok(()));
            }
        },
        move |e| {
            let _ = errors.send(Err(e));
        },
        None,
    )?;
    stream.play()?;
    rx.recv_timeout(std::time::Duration::from_secs_f64(duration + 5.0))??;
    // Last callback buffer still needs to reach the output device.
    std::thread::sleep(std::time::Duration::from_millis(250));
    drop(stream);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundled_voices_are_valid_audible_pcm() {
        for bytes in [
            include_bytes!("../assets/voices/auto.wav").as_slice(),
            include_bytes!("../assets/voices/groq.wav").as_slice(),
            include_bytes!("../assets/voices/local.wav").as_slice(),
            include_bytes!("../assets/voices/error.wav").as_slice(),
        ] {
            let (samples, _) = decode(bytes.to_vec()).unwrap();
            assert!(samples.iter().any(|s| s.abs() > 0.05));
        }
    }
}

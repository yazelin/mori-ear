//! 錄音 — cpal 預設 input device,F32 samples 累積在 buffer,stop 時編 WAV(16-bit PCM)。
//!
//! 不做 noise gate / VAD / DC offset 修正 — Whisper 自己很 robust。

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::sync::Arc;

pub struct Recorder {
    /// cpal Stream(Send 但內含 platform-specific 資源)。drop 時自動 stop。
    _stream: cpal::Stream,
    samples: Arc<Mutex<Vec<f32>>>,
    sample_rate: u32,
    channels: u16,
}

impl Recorder {
    pub fn start() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .context("找不到預設 input device")?;
        let config = device
            .default_input_config()
            .context("讀 input config 失敗")?;
        let sample_rate = config.sample_rate().0;
        let channels = config.channels();
        let sample_format = config.sample_format();
        let stream_config: cpal::StreamConfig = config.into();

        let samples = Arc::new(Mutex::new(Vec::<f32>::with_capacity(
            (sample_rate as usize) * (channels as usize) * 30, // 預留 30 秒
        )));
        let samples_for_cb = samples.clone();

        let err_fn = |err| tracing::warn!(error = ?err, "audio stream error");

        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &stream_config,
                move |data: &[f32], _| {
                    samples_for_cb.lock().extend_from_slice(data);
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::I16 => device.build_input_stream(
                &stream_config,
                move |data: &[i16], _| {
                    let mut buf = samples_for_cb.lock();
                    buf.extend(data.iter().map(|&s| s as f32 / i16::MAX as f32));
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::U16 => device.build_input_stream(
                &stream_config,
                move |data: &[u16], _| {
                    let mut buf = samples_for_cb.lock();
                    buf.extend(data.iter().map(|&s| {
                        (s as f32 - u16::MAX as f32 / 2.0) / (u16::MAX as f32 / 2.0)
                    }));
                },
                err_fn,
                None,
            )?,
            other => anyhow::bail!("不支援的 sample format: {other:?}"),
        };
        stream.play().context("stream play")?;

        Ok(Self {
            _stream: stream,
            samples,
            sample_rate,
            channels,
        })
    }

    /// 停止錄音 + 編 WAV(16-bit PCM,mono)。回 (wav bytes, duration_secs, rms_db)
    /// 給 caller 用 rms / duration 決定要不要送 STT(Whisper 安靜 audio 會幻覺
    /// 出「謝謝」「字幕」「請訂閱」之類訓練資料尾巴)。
    pub fn stop_and_encode_wav(self) -> Result<(Vec<u8>, f32, f32)> {
        let Self {
            samples,
            sample_rate,
            channels,
            _stream,
        } = self;
        drop(_stream);

        let raw = std::mem::take(&mut *samples.lock());
        if raw.is_empty() {
            anyhow::bail!("錄到 0 samples");
        }

        // 多聲道 → mono(每 channels 個取平均)
        let mono: Vec<f32> = if channels == 1 {
            raw
        } else {
            raw.chunks(channels as usize)
                .map(|frame| frame.iter().sum::<f32>() / (channels as f32))
                .collect()
        };

        // RMS + duration(在 mono samples 上算,給 caller 判斷 silence)
        let sum_sq: f32 = mono.iter().map(|&s| s * s).sum();
        let rms = (sum_sq / mono.len() as f32).sqrt();
        let rms_db = if rms > 0.0 { 20.0 * rms.log10() } else { -90.0 };
        let duration_secs = mono.len() as f32 / sample_rate as f32;

        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut buf = std::io::Cursor::new(Vec::<u8>::new());
        {
            let mut w = hound::WavWriter::new(&mut buf, spec).context("hound writer")?;
            for s in mono {
                let s = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                w.write_sample(s).context("write sample")?;
            }
            w.finalize().context("finalize WAV")?;
        }
        Ok((buf.into_inner(), duration_secs, rms_db))
    }
}

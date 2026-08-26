//! 錄音 — cpal 預設 input device,F32 samples 累積在 buffer,stop 時 downmix mono、
//! 剪靜音、編 WAV(16-bit PCM)。
//!
//! 不做 noise gate / DC offset 修正,但送 STT 前會剪掉「首尾靜音 + 中間連續長停頓」
//! (見 `stop_and_encode_wav` / `apply_trim`,對齊 mori-desktop 的 trim_silence_runs)。
//! 剪裁可由 `TrimConfig` 關閉 / 調參(ear.json `voice_input.trim_silence_*`)。

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::sync::Arc;

/// 靜音剪裁設定。對齊 mori-desktop `config.json` 的 `voice_input.trim_silence_*`。
#[derive(Clone, Copy, Debug)]
pub struct TrimConfig {
    /// 關掉 → 整段原樣送 STT(行為同舊版,不剪)。
    pub enabled: bool,
    /// 線性振幅門檻(0.0~1.0),低於視為靜音。0.02 ≈ -34 dBFS,蓋過多數 mic hum / 風扇噪。
    pub threshold: f32,
    /// 中間連續靜音 ≥ 此毫秒才壓掉(短的自然停頓留著給 Whisper 斷句)。
    pub min_silence_ms: u32,
}

impl Default for TrimConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: 0.02,
            min_silence_ms: 300,
        }
    }
}

/// RMS 視窗大小(毫秒)— 首尾掃描 & 連續靜音判定都用這個粒度。
const FRAME_MS: u32 = 20;
/// 首尾剪裁後,前後各保留的 padding(毫秒)— 避免切掉字首軟起音 / 收尾氣音。
const EDGE_PAD_MS: u32 = 80;

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

    /// 錄音中也能取用的緩衝把手 —— 給「講到一半的停頓就先送 STT」用。
    /// 只帶 samples buffer + 格式,不帶 cpal Stream(那個不是 Sync,跨執行緒不好搬)。
    pub fn buffer(&self) -> BufferHandle {
        BufferHandle {
            samples: self.samples.clone(),
            sample_rate: self.sample_rate,
            channels: self.channels,
        }
    }

    /// 停止錄音 + 編 WAV(16-bit PCM,mono)。回 (wav bytes, duration_secs, rms_db)。
    ///
    /// `rms_db` / `duration_secs` 用**整段(剪裁前)**算,給 caller 的 silence skip
    /// 守門用(行為不變);實際編進 WAV 的是**剪裁後**的 samples(`trim` 決定)。
    /// Whisper 對安靜 audio 會幻覺出「謝謝」「請訂閱」之類訓練資料尾巴,所以前後
    /// 靜音 + 中間長停頓在送出前先剪掉。
    pub fn stop_and_encode_wav(self, trim: TrimConfig) -> Result<(Vec<u8>, f32, f32)> {
        let Self {
            samples,
            sample_rate,
            channels,
            _stream,
        } = self;
        drop(_stream);

        let raw = std::mem::take(&mut *samples.lock());
        encode(raw, sample_rate, channels, &trim)
    }
}

/// 錄音進行中的緩衝把手。可以問「錄了多久」「尾巴是不是靜音」,也可以把目前
/// 累積的部分整段取走編成 WAV(取走之後錄音繼續,新的 samples 從頭累積)。
#[derive(Clone)]
pub struct BufferHandle {
    samples: Arc<Mutex<Vec<f32>>>,
    sample_rate: u32,
    channels: u16,
}

impl BufferHandle {
    /// 測試用:直接從 samples 造一個把手,不需要真的 audio device。
    #[cfg(test)]
    fn from_samples(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Self {
        Self { samples: Arc::new(Mutex::new(samples)), sample_rate, channels }
    }

    /// 目前累積了幾秒。
    pub fn secs_buffered(&self) -> f32 {
        let n = self.samples.lock().len();
        n as f32 / (self.sample_rate as f32 * self.channels as f32)
    }

    /// 最後 `tail_ms` 毫秒是不是都低於門檻(= 講話停頓了)。
    /// 累積不足 `tail_ms` 一律回 false,避免一開始就被判成停頓。
    pub fn tail_is_silent(&self, tail_ms: u32, threshold: f32) -> bool {
        let want = (self.sample_rate as usize * self.channels as usize) * tail_ms as usize / 1000;
        let buf = self.samples.lock();
        if want == 0 || buf.len() < want {
            return false;
        }
        frame_rms(&buf[buf.len() - want..]) < threshold
    }

    /// 把目前累積的 samples 整段取走並編成 WAV,錄音繼續。沒東西可取回 None。
    pub fn take_wav(&self, trim: &TrimConfig) -> Option<(Vec<u8>, f32, f32)> {
        let raw = std::mem::take(&mut *self.samples.lock());
        encode(raw, self.sample_rate, self.channels, trim).ok()
    }
}

/// mono 化 → 算整段 RMS/長度 → 剪靜音 → 編 16-bit PCM WAV。
///
/// `rms_db` / `duration_secs` 用**整段(剪裁前)**算,給 caller 的 silence skip
/// 守門用(行為不變);實際編進 WAV 的是**剪裁後**的 samples。
/// Whisper 對安靜 audio 會幻覺出「謝謝」「請訂閱」之類訓練資料尾巴,所以前後
/// 靜音 + 中間長停頓在送出前先剪掉。
fn encode(raw: Vec<f32>, sample_rate: u32, channels: u16, trim: &TrimConfig) -> Result<(Vec<u8>, f32, f32)> {
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

    let sum_sq: f32 = mono.iter().map(|&s| s * s).sum();
    let rms = (sum_sq / mono.len() as f32).sqrt();
    let rms_db = if rms > 0.0 { 20.0 * rms.log10() } else { -90.0 };
    let duration_secs = mono.len() as f32 / sample_rate as f32;

    let trimmed = apply_trim(&mono, sample_rate, trim);
    let to_encode: &[f32] = if trimmed.is_empty() { &mono } else { &trimmed };
    if to_encode.len() != mono.len() {
        tracing::info!(
            orig_samples = mono.len(),
            kept_samples = to_encode.len(),
            threshold = trim.threshold,
            min_silence_ms = trim.min_silence_ms,
            "靜音剪裁(首尾 + 中間連續靜音)"
        );
    }

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = std::io::Cursor::new(Vec::<u8>::new());
    {
        let mut w = hound::WavWriter::new(&mut buf, spec).context("hound writer")?;
        for &s in to_encode {
            let s = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            w.write_sample(s).context("write sample")?;
        }
        w.finalize().context("finalize WAV")?;
    }
    Ok((buf.into_inner(), duration_secs, rms_db))
}

/// 單一視窗的 RMS(線性,0~1)。空 → 0。
fn frame_rms(seg: &[f32]) -> f32 {
    if seg.is_empty() {
        return 0.0;
    }
    (seg.iter().map(|&x| x * x).sum::<f32>() / seg.len() as f32).sqrt()
}

/// 剪裁 mono:首尾(任意長度)靜音 + 中間連續靜音 ≥ `min_silence_ms`。
/// 回剪裁後的 mono samples;全靜音 → 空 Vec(caller 自行 fallback)。
fn apply_trim(mono: &[f32], sample_rate: u32, cfg: &TrimConfig) -> Vec<f32> {
    if !cfg.enabled || mono.is_empty() {
        return mono.to_vec();
    }
    let th = cfg.threshold.clamp(0.0, 1.0);
    if th <= 0.0 {
        return mono.to_vec();
    }
    // 1. 首尾(任意長度)+ pad
    let (start, end) = trim_edges(mono, sample_rate, th, EDGE_PAD_MS, FRAME_MS);
    if end <= start {
        return Vec::new(); // 全靜音
    }
    let edged = &mono[start..end];
    // 2. 中間連續長停頓
    drop_internal_runs(edged, sample_rate, th, cfg.min_silence_ms, FRAME_MS)
}

/// 找首尾「有聲」邊界(frame_ms 視窗 RMS 判定),前後各留 `pad_ms`。
/// 回 (start, end) sample index;全靜音 → (0, 0)。
fn trim_edges(
    mono: &[f32],
    sample_rate: u32,
    threshold: f32,
    pad_ms: u32,
    frame_ms: u32,
) -> (usize, usize) {
    let n = mono.len();
    if n == 0 {
        return (0, 0);
    }
    let win = ((sample_rate as u64 * frame_ms as u64 / 1000) as usize).max(1);
    let pad = (sample_rate as u64 * pad_ms as u64 / 1000) as usize;

    let mut first: Option<usize> = None;
    let mut last_end = 0usize;
    let mut s = 0usize;
    while s < n {
        let e = (s + win).min(n);
        if frame_rms(&mono[s..e]) >= threshold {
            if first.is_none() {
                first = Some(s);
            }
            last_end = e;
        }
        s += win;
    }
    match first {
        Some(f) => (f.saturating_sub(pad), (last_end + pad).min(n)),
        None => (0, 0),
    }
}

/// 移除中間「連續靜音 ≥ `min_silence_ms`」的區段(短停頓保留)。
fn drop_internal_runs(
    mono: &[f32],
    sample_rate: u32,
    threshold: f32,
    min_silence_ms: u32,
    frame_ms: u32,
) -> Vec<f32> {
    let n = mono.len();
    if n == 0 || min_silence_ms == 0 {
        return mono.to_vec();
    }
    let win = ((sample_rate as u64 * frame_ms as u64 / 1000) as usize).max(1);
    let min_silence = ((sample_rate as u64 * min_silence_ms as u64 / 1000) as usize).max(1);

    let mut out: Vec<f32> = Vec::with_capacity(n);
    let mut i = 0usize;
    while i < n {
        let e = (i + win).min(n);
        if frame_rms(&mono[i..e]) >= threshold {
            out.extend_from_slice(&mono[i..e]); // 有聲 → 留
            i = e;
            continue;
        }
        // 靜音 run:往後掃到下一個有聲視窗
        let run_start = i;
        let mut j = i;
        while j < n {
            let je = (j + win).min(n);
            if frame_rms(&mono[j..je]) < threshold {
                j = je;
            } else {
                break;
            }
        }
        if (j - run_start) < min_silence {
            out.extend_from_slice(&mono[run_start..j]); // 短停頓 → 留(Whisper 斷句線索)
        }
        // 長停頓 → 整段丟掉
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 16_000;

    fn silence(secs: f32) -> Vec<f32> {
        vec![0.0; (SR as f32 * secs) as usize]
    }
    fn voiced(secs: f32) -> Vec<f32> {
        // 0.5 恆定振幅 → frame_rms = 0.5,遠高於 0.02 門檻
        vec![0.5; (SR as f32 * secs) as usize]
    }

    #[test]
    fn trim_edges_strips_leading_and_trailing() {
        let mut sig = silence(0.5); // 8000
        sig.extend(voiced(1.0)); // 16000
        sig.extend(silence(0.5)); // 8000 → 共 32000
        let (s, e) = trim_edges(&sig, SR, 0.02, EDGE_PAD_MS, FRAME_MS);
        // voiced 落在 8000..24000;前後各留 80ms(=1280)padding
        assert_eq!(s, 8000 - 1280);
        assert_eq!(e, 24000 + 1280);
    }

    #[test]
    fn trim_edges_all_silence() {
        let sig = silence(1.0);
        assert_eq!(trim_edges(&sig, SR, 0.02, EDGE_PAD_MS, FRAME_MS), (0, 0));
    }

    #[test]
    fn drop_internal_runs_removes_long_pause() {
        let mut sig = voiced(0.5); // 8000
        sig.extend(silence(1.0)); // 16000 — > 300ms,整段丟
        sig.extend(voiced(0.5)); // 8000
        let out = drop_internal_runs(&sig, SR, 0.02, 300, FRAME_MS);
        // 只剩兩段 voiced
        assert_eq!(out.len(), 16_000);
    }

    #[test]
    fn drop_internal_runs_keeps_short_pause() {
        let mut sig = voiced(0.5); // 8000
        sig.extend(silence(0.2)); // 3200 — < 300ms,保留
        sig.extend(voiced(0.5)); // 8000
        let out = drop_internal_runs(&sig, SR, 0.02, 300, FRAME_MS);
        assert_eq!(out.len(), sig.len());
    }

    #[test]
    fn apply_trim_full_silence_returns_empty() {
        let sig = silence(1.0);
        assert!(apply_trim(&sig, SR, &TrimConfig::default()).is_empty());
    }

    #[test]
    fn apply_trim_disabled_is_passthrough() {
        let mut sig = silence(0.5);
        sig.extend(voiced(0.5));
        let cfg = TrimConfig {
            enabled: false,
            ..TrimConfig::default()
        };
        assert_eq!(apply_trim(&sig, SR, &cfg), sig);
    }

    // ── 講到一半就先送 STT 用的緩衝把手 ──────────────────────────────

    /// 造 `ms` 毫秒的訊號,振幅 `amp`(0 = 靜音)。
    fn buf_tone(ms: u32, amp: f32) -> Vec<f32> {
        let n = (SR as usize) * ms as usize / 1000;
        (0..n).map(|i| if i % 2 == 0 { amp } else { -amp }).collect()
    }

    #[test]
    fn secs_buffered_counts_channels() {
        let h = BufferHandle::from_samples(buf_tone(1000, 0.5), SR, 1);
        assert!((h.secs_buffered() - 1.0).abs() < 0.01);
        // 雙聲道:同樣的 sample 數只有一半的時間
        let h2 = BufferHandle::from_samples(buf_tone(1000, 0.5), SR, 2);
        assert!((h2.secs_buffered() - 0.5).abs() < 0.01);
    }

    #[test]
    fn tail_is_silent_detects_a_pause() {
        let mut samples = buf_tone(2000, 0.5);
        samples.extend(buf_tone(800, 0.0)); // 講完之後停 800ms
        let h = BufferHandle::from_samples(samples, SR, 1);
        assert!(h.tail_is_silent(700, 0.02), "尾巴 800ms 靜音應該判成停頓");
        assert!(!h.tail_is_silent(1200, 0.02), "看回 1200ms 會吃到人聲,不算停頓");
    }

    #[test]
    fn tail_is_silent_false_while_still_talking() {
        let h = BufferHandle::from_samples(buf_tone(3000, 0.5), SR, 1);
        assert!(!h.tail_is_silent(700, 0.02));
    }

    #[test]
    fn tail_is_silent_false_when_buffer_too_short() {
        // 只錄了 300ms,不該因為「還沒累積到 700ms」就被當成停頓
        let h = BufferHandle::from_samples(buf_tone(300, 0.0), SR, 1);
        assert!(!h.tail_is_silent(700, 0.02));
    }

    #[test]
    fn take_wav_drains_so_next_segment_starts_fresh() {
        let mut samples = buf_tone(1500, 0.5);
        samples.extend(buf_tone(800, 0.0));
        let h = BufferHandle::from_samples(samples, SR, 1);
        let trim = TrimConfig::default();

        let (wav, dur, rms_db) = h.take_wav(&trim).expect("第一段應該取得到");
        assert!(!wav.is_empty());
        assert!(dur > 2.0, "duration 用整段(剪裁前)算,應該 2.3 秒左右,實際 {dur}");
        assert!(rms_db > -45.0, "有人聲,不該被當成靜音,實際 {rms_db}");

        assert!(h.take_wav(&trim).is_none(), "取走之後 buffer 應該空了");
        assert_eq!(h.secs_buffered(), 0.0);
    }
}

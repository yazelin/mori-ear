//! 本地 whisper-server fallback client — 讀 `~/.mori/whisper-server.json` 發現檔 → 驗活 →
//! resample 到 16kHz → `POST /inference` multipart → 回 text。
//!
//! mori-ear 是純 **Adopter**(只讀 descriptor、不啟動/不寫/不擁有 server;server 由
//! mori-meeting-recorder 之類 Starter 啟)。契約見
//! `agentos-notebook/05-mori-migration/whisper-server-contract.md`。
//!
//! 安全(對齊 AgentOS whisper_client):host pin loopback、`redirect(none)`、`no_proxy()`、
//! `inference_path` 正規化前導 `/`(擋 `@evil.com` userinfo 掉包 host)。
//! resample:本地 whisper.cpp 不保證 resample(契約 §8),而 mori-ear 錄音是裝置原生取樣率
//! → 送本地前**必須**轉 16kHz mono 16-bit(Groq 路徑不受影響,它自己 resample)。

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::multipart::{Form, Part};
use serde::Deserialize;

const LOOPBACK: &[&str] = &["127.0.0.1", "::1"];

#[derive(Debug, Deserialize)]
struct Descriptor {
    host: String,
    port: u16,
    #[serde(default = "default_inference_path")]
    inference_path: String,
    /// server 載入的模型短名(契約有給,例如 "large-v3-turbo")。純資訊用 —— 讓 mori-ear
    /// 能 log「這次走的是哪個本機模型」,解掉「不知道現在用誰」的盲區。
    #[serde(default)]
    model: Option<String>,
}

fn default_inference_path() -> String {
    "/inference".into()
}

#[derive(Debug, Deserialize)]
struct InferenceResponse {
    #[serde(default)]
    text: String,
}

impl Descriptor {
    fn is_loopback(&self) -> bool {
        LOOPBACK.contains(&self.host.as_str())
    }

    fn base(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    /// 推論 URL,前導 `/` 正規化:`inference_path` 無前導 `/` 時補上,host 始終綁 descriptor
    /// 的 loopback(擋 `inference_path = "@evil.com/"` 之類把 host 掉包的注入)。
    fn inference_url(&self) -> String {
        if self.inference_path.starts_with('/') {
            format!("{}{}", self.base(), self.inference_path)
        } else {
            format!("{}/{}", self.base(), self.inference_path)
        }
    }
}

fn default_descriptor_path() -> std::path::PathBuf {
    crate::home_dir().join(".mori").join("whisper-server.json")
}

fn read_descriptor(p: &std::path::Path) -> Result<Descriptor> {
    let text = std::fs::read_to_string(p)
        .with_context(|| format!("讀不到 whisper descriptor {}", p.display()))?;
    serde_json::from_str(&text).with_context(|| format!("解析 whisper descriptor {}", p.display()))
}

fn http_client(timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .context("build http client")
}

/// 驗活:host 必為 loopback **且** `GET /` 回 200(契約 §3.1 的 authoritative ready gate)。
async fn verify_alive(d: &Descriptor) -> bool {
    if !d.is_loopback() {
        return false;
    }
    let Ok(client) = http_client(Duration::from_secs(2)) else {
        return false;
    };
    matches!(client.get(d.base()).send().await, Ok(r) if r.status().is_success())
}

/// 把(裝置原生取樣率的)16-bit PCM mono/多聲道 WAV → 16kHz mono 16-bit WAV bytes。
fn resample_wav_to_16k(wav: &[u8]) -> Result<Vec<u8>> {
    const TARGET: u32 = 16_000;
    // size cap:整檔解進記憶體前先擋(防惡意/壞檔宣告海量樣本 → OOM;對齊 AgentOS handler 512MB)。
    const MAX_WAV_BYTES: usize = 512 * 1024 * 1024;
    if wav.len() > MAX_WAV_BYTES {
        anyhow::bail!("WAV 過大: {} bytes(上限 {MAX_WAV_BYTES})", wav.len());
    }
    let mut reader =
        hound::WavReader::new(std::io::Cursor::new(wav)).context("decode WAV(本地路徑需可解碼 WAV)")?;
    let spec = reader.spec();
    let samples: Vec<i16> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("read i16 samples")?,
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.map(|f| (f.clamp(-1.0, 1.0) * 32767.0) as i16))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("read f32 samples")?,
    };
    let ch = spec.channels.max(1) as usize;
    let mono: Vec<i16> = if ch == 1 {
        samples
    } else {
        samples
            .chunks(ch)
            .map(|c| (c.iter().map(|&s| s as i32).sum::<i32>() / ch as i32) as i16)
            .collect()
    };
    let out: Vec<i16> = if spec.sample_rate == TARGET {
        mono
    } else {
        resample_linear(&mono, spec.sample_rate, TARGET)
    };
    if out.is_empty() {
        anyhow::bail!("音訊轉換後為空(太短?),不送本地 whisper-server");
    }

    let out_spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = std::io::Cursor::new(Vec::<u8>::new());
    let mut w = hound::WavWriter::new(&mut buf, out_spec).context("init WAV writer")?;
    for s in out {
        w.write_sample(s).context("write sample")?;
    }
    w.finalize().context("finalize WAV")?;
    Ok(buf.into_inner())
}

/// 線性內插 resample(語音夠用、不引第三方 crate)。
fn resample_linear(input: &[i16], src: u32, dst: u32) -> Vec<i16> {
    if input.is_empty() || src == 0 {
        return Vec::new();
    }
    let ratio = dst as f64 / src as f64;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let last = input.len() - 1;
    let mut out = Vec::with_capacity(out_len);
    for j in 0..out_len {
        let src_pos = j as f64 / ratio;
        let i = src_pos.floor() as usize;
        let frac = src_pos - i as f64;
        let a = input[i.min(last)] as f64;
        let b = input[(i + 1).min(last)] as f64;
        out.push((a + (b - a) * frac).round() as i16);
    }
    out
}

async fn transcribe_local(d: &Descriptor, language: &str, wav16: Vec<u8>) -> Result<String> {
    let client = http_client(Duration::from_secs(120))?;
    let mut form = Form::new().part(
        "file",
        Part::bytes(wav16)
            .file_name("audio.wav")
            .mime_str("audio/wav")?,
    );
    // whisper.cpp server 沒給 language 時**預設 "en"** → 對中文語音會整段轉成英文亂碼。
    // 空 language 時送 "auto" 讓它自動偵測(對齊 Groq 雲端的行為)。
    let lang = if language.is_empty() { "auto" } else { language };
    form = form.text("language", lang.to_string());
    let resp = client
        .post(d.inference_url())
        .multipart(form)
        .send()
        .await
        .context("local whisper-server request")?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("local whisper-server HTTP {status}");
    }
    let parsed: InferenceResponse = resp.json().await.context("parse local whisper-server response")?;
    Ok(parsed.text.trim().to_string())
}

/// 高階入口:讀預設 descriptor → 驗活 → resample 16k → forward。任一步失敗回 Err
/// (caller 在 auto 模式下會在 Groq 失敗後呼叫它;它再失敗 = 整體失敗)。
pub async fn transcribe_default(language: &str, wav_bytes: Vec<u8>) -> Result<String> {
    let path = default_descriptor_path();
    let d = read_descriptor(&path)?;
    if !verify_alive(&d).await {
        anyhow::bail!(
            "本地 whisper-server 未在線(descriptor {} / 非 loopback 或 GET / 不通)",
            path.display()
        );
    }
    let wav16 = resample_wav_to_16k(&wav_bytes).context("resample 到 16kHz")?;
    tracing::info!(
        backend = "local",
        model = d.model.as_deref().unwrap_or("unknown"),
        host = %d.host,
        port = d.port,
        "STT 走本地 whisper-server"
    );
    transcribe_local(&d, language, wav16).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_at(rate: u32, n: usize) -> Vec<u8> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut w = hound::WavWriter::new(&mut buf, spec).unwrap();
        for i in 0..n {
            w.write_sample(((i % 100) as i16) * 100).unwrap();
        }
        w.finalize().unwrap();
        buf.into_inner()
    }

    #[test]
    fn resample_produces_16k_mono_16bit() {
        let src = wav_at(48_000, 4800); // 0.1s @ 48k
        let out = resample_wav_to_16k(&src).unwrap();
        let r = hound::WavReader::new(std::io::Cursor::new(out)).unwrap();
        let spec = r.spec();
        assert_eq!(spec.sample_rate, 16_000);
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.bits_per_sample, 16);
        let n = r.into_samples::<i16>().count();
        assert!((n as i32 - 1600).abs() < 20, "16k 0.1s 應 ~1600 樣本,得 {n}");
    }

    #[test]
    fn resample_passthrough_when_already_16k() {
        let out = resample_wav_to_16k(&wav_at(16_000, 1600)).unwrap();
        let r = hound::WavReader::new(std::io::Cursor::new(out)).unwrap();
        assert_eq!(r.spec().sample_rate, 16_000);
        assert_eq!(r.into_samples::<i16>().count(), 1600, "16k passthrough 樣本數不變");
    }

    #[test]
    fn resample_rejects_garbage_and_too_short() {
        // 非 WAV 垃圾 → Err(不 panic;batch 餵 mp3 給 local backend 會走到這)。
        assert!(resample_wav_to_16k(b"not a wav file at all").is_err());
        // 極短(1 sample @48k → resample 後 0 樣本)→ Err,不送空音訊。
        assert!(
            resample_wav_to_16k(&wav_at(48_000, 1)).is_err(),
            "極短音訊應 Err 而非送空"
        );
    }

    #[test]
    fn descriptor_parses_with_default_inference_path() {
        let d: Descriptor = serde_json::from_str(r#"{"host":"127.0.0.1","port":12345}"#).unwrap();
        assert_eq!(d.inference_path, "/inference");
        assert!(d.is_loopback());
    }

    #[test]
    fn inference_url_normalizes_and_pins_loopback() {
        let d = Descriptor { host: "127.0.0.1".into(), port: 12345, inference_path: "inference".into(), model: None };
        assert_eq!(d.inference_url(), "http://127.0.0.1:12345/inference");
        let evil = Descriptor { host: "127.0.0.1".into(), port: 12345, inference_path: "@evil.com/".into(), model: None };
        assert!(
            evil.inference_url().starts_with("http://127.0.0.1:12345/"),
            "userinfo 注入應被前導 / 擋住: {}",
            evil.inference_url()
        );
    }

    #[test]
    fn non_loopback_host_is_rejected() {
        let d = Descriptor { host: "10.0.0.5".into(), port: 9, inference_path: "/inference".into(), model: None };
        assert!(!d.is_loopback(), "非 loopback host 應被拒(會議音訊不外送)");
    }

    #[tokio::test]
    async fn verify_alive_and_transcribe_forward_against_stub() {
        let server = std::sync::Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let srv = server.clone();
        let h = std::thread::spawn(move || {
            for mut req in srv.incoming_requests() {
                let is_inf = req.url().starts_with("/inference");
                let mut body = Vec::new();
                if is_inf {
                    let _ = req.as_reader().read_to_end(&mut body);
                }
                // 驗 multipart 真的帶 file part(否則 malformed form 也假綠)。
                let payload = if is_inf && String::from_utf8_lossy(&body).contains("name=\"file\"") {
                    r#"{"text":"哈囉世界"}"#
                } else if is_inf {
                    r#"{"error":"missing file"}"#
                } else {
                    "ok"
                };
                let _ = req.respond(tiny_http::Response::from_string(payload).with_status_code(200));
            }
        });

        let d = Descriptor { host: "127.0.0.1".into(), port, inference_path: "/inference".into(), model: None };
        assert!(verify_alive(&d).await, "活著的 stub 應驗活通過");
        let text = transcribe_local(&d, "zh", wav_at(16_000, 1600)).await.unwrap();
        assert_eq!(text, "哈囉世界");

        server.unblock();
        let _ = h.join();
    }
}

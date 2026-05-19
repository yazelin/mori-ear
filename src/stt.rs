//! Groq Whisper API client — 極簡版。multipart upload WAV, parse 回傳 text。

use anyhow::{Context, Result};
use reqwest::multipart::{Form, Part};
use serde::Deserialize;

const URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const MODEL: &str = "whisper-large-v3-turbo";

#[derive(Debug, Deserialize)]
struct Response {
    text: String,
}

pub async fn transcribe(api_key: &str, language: &str, wav_bytes: Vec<u8>) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    let mut form = Form::new()
        .text("model", MODEL)
        .part(
            "file",
            Part::bytes(wav_bytes)
                .file_name("audio.wav")
                .mime_str("audio/wav")?,
        );
    if !language.is_empty() {
        form = form.text("language", language.to_string());
    }

    let resp = client
        .post(URL)
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .context("groq request")?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("groq HTTP {status}: {body}");
    }
    let parsed: Response = resp.json().await.context("parse groq response")?;
    Ok(parsed.text.trim().to_string())
}

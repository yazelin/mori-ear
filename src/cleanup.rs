//! LLM cleanup — 把 raw Whisper transcript 餵 Groq chat,做最小幅度繁中校正。
//!
//! 鏡像 mori-desktop 的 `~/.mori/voice_input/USER-00.純文字輸入.md` 預設:
//!   - 修錯字(同音字、相近詞)
//!   - 補標點(逗號、句號、問號)
//!   - 切段(長文有自然停頓處換行)
//!   - **保留原意**,不改詞、不縮寫、不擴寫
//!   - **強制繁中(台灣用語)**:Whisper 偶爾吐簡體,LLM 順手轉
//!
//! 失敗 fallback raw 字串 — 不擋 paste-back。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const URL: &str = "https://api.groq.com/openai/v1/chat/completions";
const MODEL: &str = "openai/gpt-oss-120b";

const SYSTEM_PROMPT: &str = "你是 mori 語音輸入助理。把 STT 輸出的純文字做最小幅度校正後輸出。

**第一條,最重要:輸出必須是 100% 繁體中文(台灣用語)**

- 不允許出現任何簡體字。所有簡體一律轉成對應繁體。
- 用台灣慣用詞,不要中國大陸用語(例:軟體不是軟件、影片不是視頻、滑鼠不是鼠標、印表機不是打印機、優化不是優化但若無對應就保留)。
- 即使 STT 輸出的是簡體,你的輸出也必須是繁體。

例:
  輸入:这个软件设计的复杂度
  輸出:這個軟體設計的複雜度

  輸入:后面再说
  輸出:後面再說

第二條,其他校正(在守住第一條的前提下):
- 修錯字(同音字、相近詞)
- 補標點(逗號、句號、問號)
- 切段(長文有自然停頓處換行)
- 保留原意,不改詞、不縮寫、不擴寫

只輸出處理後的繁中純文字,不要解釋、不要前言、不要 markdown code fence。";

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    temperature: f32,
}

#[derive(Debug, Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: MessageOut,
}

#[derive(Debug, Deserialize)]
struct MessageOut {
    content: String,
}

pub async fn cleanup(api_key: &str, raw: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let req = ChatRequest {
        model: MODEL,
        messages: vec![
            Message {
                role: "system",
                content: SYSTEM_PROMPT,
            },
            Message {
                role: "user",
                content: raw,
            },
        ],
        temperature: 0.0,
    };

    let resp = client
        .post(URL)
        .bearer_auth(api_key)
        .json(&req)
        .send()
        .await
        .context("groq chat request")?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("groq chat HTTP {status}: {body}");
    }
    let parsed: ChatResponse = resp.json().await.context("parse groq chat response")?;
    let text = parsed
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .unwrap_or_default()
        .trim()
        .to_string();
    if text.is_empty() {
        anyhow::bail!("groq chat returned empty content");
    }
    Ok(text)
}

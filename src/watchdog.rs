//! 轉譯看門狗 — 給一次 STT(+cleanup)整體一個硬上限,逾時就放棄該句,絕不卡死 daemon。
//!
//! 為什麼:hotkey 路徑的轉譯跑在 `tokio::spawn`,過去只有各 HTTP request 的 reqwest
//! timeout(Groq 60s / 本地 120s / cleanup 30s)。最壞情況(auto:本地 120s 卡住 →
//! Groq 60s → cleanup 30s)會疊到 ~210s 才跳出,期間那一輪轉譯就一直懸著。這裡包一層
//! `tokio::time::timeout`,給「一次轉譯」一個可設定的整體上限(`transcribe_timeout_secs`,
//! 預設 90s)。逾時 = drop 那個 future → 進行中的 reqwest 連線關閉即止血。
//!
//! 子程序:mori-ear 目前 STT 純走 HTTP(Adopter,不 spawn whisper),所以今天逾時靠
//! drop future 就夠。未來若加 ffmpeg / whisper-cli 的 per-call spawn(Phase 2 本地路徑),
//! 把 child handle 包進 [`ChildGuard`] 並讓它活在被 timeout 包住的 future 內 —— 逾時
//! future 被 cancel/drop 時 `ChildGuard::drop` 會 `kill()` 子程序,不留殭屍續吃 CPU。

use std::future::Future;
use std::time::Duration;

use anyhow::Result;

/// 把任一轉譯 future 包進整體 timeout。
///
/// - 內層在期限內完成 → 回傳它的 `Result`(成功或它自己的錯)。
/// - 超過 `timeout` → 回 `Err`(明確訊息),caller 視同該句轉譯失敗 → 放棄,不卡死。
pub async fn guard<F, T>(timeout: Duration, label: &str, fut: F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    match tokio::time::timeout(timeout, fut).await {
        Ok(inner) => inner,
        Err(_elapsed) => anyhow::bail!(
            "轉譯逾時 {}s（{label}）— 放棄該句避免卡死。\
             如常態太慢,調 ~/.mori/ear.json 的 transcribe_timeout_secs",
            timeout.as_secs()
        ),
    }
}

/// 子程序守衛:綁住一個已 spawn 的 child;被 drop(含 timeout cancel/drop 包住它的 future
/// 時)就 best-effort `kill()` + 收屍,避免逾時後 ffmpeg / whisper-cli 變殭屍續吃資源。
///
/// 註:目前 ear STT 走純 HTTP(無子程序),此型別為 Phase 2(本地 ffmpeg / whisper-cli
/// per-call spawn)預留 + 自我守護;先建好讓「逾時必殺子程序」這條鐵則在加本地路徑時
/// 是預設行為,而不是事後補。
#[allow(dead_code)]
pub struct ChildGuard(pub std::process::Child);

#[allow(dead_code)]
impl ChildGuard {
    pub fn new(child: std::process::Child) -> Self {
        Self(child)
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        // 已退出 → 直接 wait 收屍;還在跑 → kill 再 wait,確保不留殭屍。
        match self.0.try_wait() {
            Ok(Some(_)) => {}
            _ => {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn guard_passes_through_fast_ok() {
        let r: Result<u32> = guard(Duration::from_secs(5), "t", async { Ok(7) }).await;
        assert_eq!(r.unwrap(), 7);
    }

    #[tokio::test]
    async fn guard_passes_through_inner_err() {
        let r: Result<u32> = guard(Duration::from_secs(5), "t", async {
            anyhow::bail!("inner boom")
        })
        .await;
        assert!(r.unwrap_err().to_string().contains("inner boom"));
    }

    #[tokio::test]
    async fn guard_times_out_slow_future() {
        // 真實時鐘、短上限:1s timeout 包住一個睡 10s 的 future → ~1s 內回逾時。
        // (不用 start_paused,以免要求 tokio "test-util" feature。)
        let slow = async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok::<u32, anyhow::Error>(1)
        };
        let r = guard(Duration::from_secs(1), "slow", slow).await;
        let msg = r.unwrap_err().to_string();
        assert!(msg.contains("逾時"), "應回逾時錯: {msg}");
        assert!(msg.contains("transcribe_timeout_secs"), "錯訊應指路設定: {msg}");
    }
}

//! Optional Linux mode picker. Runs independently of the recording daemon.
use anyhow::{Context, Result};
use std::{
    path::Path,
    process::{Command, ExitCode, Stdio},
};

fn choice(code: Option<i32>) -> Option<&'static str> {
    match code {
        Some(10) => Some("groq"),
        Some(11) => Some("local"),
        Some(12) => Some("auto"),
        _ => None,
    }
}

fn label(mode: &str) -> &'static str {
    match mode {
        "groq" => "線上模式（Groq）",
        "local" => "本機模式",
        _ => "自動模式",
    }
}

pub fn run() -> Result<ExitCode> {
    anyhow::ensure!(cfg!(target_os = "linux"), "模式選擇視窗目前支援 Linux");
    // ear talk targets pgrep -x mori-ear. This window is not the daemon.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_NAME, c"mori-ear-ui".as_ptr(), 0, 0, 0);
    }
    run_at(
        &crate::ear_config_path(),
        crate::Config::load().resolved_api_key().is_some(),
    )?;
    Ok(ExitCode::SUCCESS)
}

fn run_at(path: &Path, has_key: bool) -> Result<()> {
    let mut message = "請選擇辨識方式。切換後可繼續錄音。".to_owned();
    loop {
        let mode = crate::mode_command::current_at(path, "auto");
        let text = format!(
            "目前：{}\n\n線上：使用 Groq，需要網路。\n\n本機：在這台電腦上辨識語音。\n\n自動：先用本機，本機失敗才改用 Groq。\n\n{}\n\n已送出辨識的片段不受影響，新片段使用新模式。",
            label(&mode), message,
        );
        let result = Command::new("yad")
            .args([
                "--title=Mori 語音辨識模式",
                "--window-icon=audio-input-microphone",
                "--width=660",
                "--height=360",
                "--center",
                "--borders=20",
                "--no-markup",
                "--button=線上 Groq:10",
                "--button=本機:11",
                "--button=自動:12",
                "--button=關閉:0",
                "--text",
                &text,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .context("無法開啟模式視窗，請確認已安裝 yad")?;
        let Some(selected) = choice(result.code()) else {
            anyhow::ensure!(matches!(result.code(), Some(0 | 252)), "模式視窗非正常結束");
            return Ok(());
        };
        message = match crate::mode_command::save(path, selected, has_key) {
            Ok(()) => format!("已切換為{}。錄音可繼續，不必重新啟動。", label(selected)),
            Err(_) => {
                if selected == "groq" && !has_key {
                    "切換失敗：尚未設定 Groq 金鑰，仍維持原模式。".to_owned()
                } else {
                    "切換失敗：請確認 ear.json 格式與寫入權限，仍維持原模式。".to_owned()
                }
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_never_selects_a_mode() {
        assert_eq!(choice(Some(10)), Some("groq"));
        assert_eq!(choice(Some(11)), Some("local"));
        assert_eq!(choice(Some(12)), Some("auto"));
        for code in [Some(0), Some(252), Some(1), None] {
            assert_eq!(choice(code), None);
        }
    }

    #[test]
    #[ignore = "Requires interactive display; uses only a temporary configuration"]
    fn interactive_picker_smoke() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ear.json");
        std::fs::write(&path, r#"{"backend":"auto","untouched":42}"#).unwrap();
        println!("UI_TEST_CONFIG={}", path.display());
        run_at(&path, true).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["backend"], "groq");
        assert_eq!(saved["untouched"], 42);
    }
}

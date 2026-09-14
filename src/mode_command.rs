//! Deliberately narrow, whole-utterance commands; never interpret batch/HTTP dictation.
use anyhow::{Context, Result};
use std::{io::Write, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Status,
    Switch(&'static str),
}

#[derive(serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    pub sound: bool,
    pub notification: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            sound: true,
            notification: true,
        }
    }
}
pub fn settings() -> Settings {
    std::fs::read(crate::ear_config_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| serde_json::from_value(v.get("mode_commands")?.clone()).ok())
        .unwrap_or_default()
}

pub fn parse(text: &str) -> Option<Command> {
    let s: String = text
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace() && !",，。.!！?？:：".contains(*c))
        .collect();
    // Curated ASR aliases, not arbitrary substring/fuzzy matching over dictation.
    let s = s
        .replace('现', "現")
        .replace('换', "換")
        .replace('么', "麼")
        .replace('帮', "幫")
        .replace('个', "個")
        .replace('线', "線")
        .replace('离', "離")
        .replace('请', "請");
    let s = s
        .strip_prefix("hey")
        .or_else(|| s.strip_prefix("嘿"))
        .unwrap_or(&s);
    let addressed = [
        "mori",
        "moris",
        "morris",
        "maurice",
        "morley",
        "mory",
        "molly",
        "morning",
        "莫莉",
        "茉莉",
        "摩利",
        "摩里",
        "莫里",
        "莫里斯",
        "毛利",
        "毛蕊",
        "貓咪",
        "猫咪",
        "魔力",
    ]
    .iter()
    // Prefer the longest alias: "mori" must not consume the start of "moris".
    .filter(|name| s.starts_with(**name))
    .max_by_key(|name| name.len())
    .and_then(|name| s.strip_prefix(name));
    let s = addressed.unwrap_or(s);
    let s = s
        .strip_prefix("請問")
        .or_else(|| s.strip_prefix("請"))
        .unwrap_or(s);
    let s = s
        .strip_suffix("呢")
        .or_else(|| s.strip_suffix("啊"))
        .unwrap_or(s);
    let question = s.strip_prefix("你").unwrap_or(s);
    if [
        "現在是什麼模式",
        "現在怎麼模式",
        "現在在做什麼模式",
        "現在在什麼模式",
        "現在用的是什麼模式",
        "現在是甚麼模式",
        "甚麼模式",
        "目前是什麼模式",
        "現在什麼模式",
        "現在用什麼模式",
        "現在使用什麼模式",
        "目前用什麼模式",
        "目前的模式是什麼",
        "現在的模式是什麼",
        "是什麼模式",
        "什麼模式",
        "現在用哪個模式",
        "目前用哪個模式",
        "現在是哪個模式",
        "目前是哪個模式",
        "現在是什麼辨識模式",
        "現在是什麼识别模式",
    ]
    .contains(&question)
    {
        return Some(Command::Status);
    }
    // A short status question is read-only; changing settings still requires a name.
    addressed?;
    let s = s
        .strip_prefix("幫我")
        .or_else(|| s.strip_prefix("幫忙"))
        .unwrap_or(s);
    let s = [
        "切換到",
        "切換成",
        "切換為",
        "切到",
        "切成",
        "換成",
        "換到",
        "改成",
        "改用",
        "切換",
    ]
    .iter()
    .find_map(|prefix| s.strip_prefix(prefix))?;
    let s = s.strip_suffix("模式").unwrap_or(s);
    match s {
        "auto" | "otto" | "自動" | "自动" => Some(Command::Switch("auto")),
        "groq" | "grok" | "groque" | "雲端" | "云端" | "線上" | "在線" | "在綫" => {
            Some(Command::Switch("groq"))
        }
        "local" | "loko" | "本機" | "本机" | "本地" | "離線" => {
            Some(Command::Switch("local"))
        }
        _ => None,
    }
}

/// Cleanup may repair a short status question, but can never authorize a setting change.
pub fn status_after_cleanup(text: &str) -> Option<Command> {
    match parse(text) {
        Some(Command::Status) => Some(Command::Status),
        _ => None,
    }
}

pub fn notify(mode: &str, switched: bool) {
    let detail = match mode {
        "auto" => "自動模式：先用本機，本機失敗才使用 Groq。",
        "groq" => "Groq 雲端模式：使用 Groq 辨識，需要網路。",
        "local" => "本機模式：使用這台電腦辨識。",
        _ => "切換失敗，仍維持原本模式。請查看 mori-ear 錯誤紀錄。",
    };
    let title = if mode == "error" {
        "Mori：模式未切換"
    } else if switched {
        "Mori：模式已切換"
    } else {
        "Mori：目前辨識模式"
    };
    tracing::info!(title, detail, "模式回饋");
    #[cfg(target_os = "linux")]
    {
        let result = std::process::Command::new("notify-send")
            .args(["--app-name=mori-ear", "--expire-time=6000", title, detail])
            .status();
        if !matches!(result, Ok(status) if status.success()) {
            tracing::warn!("桌面通知失敗，請確認已安裝 notify-send 並有通知服務");
        }
    }
}

pub fn current_at(path: &Path, fallback: &str) -> String {
    let mode = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("backend")?.as_str().map(str::to_owned));
    mode.filter(|s| matches!(s.as_str(), "auto" | "groq" | "local"))
        .unwrap_or_else(|| fallback.to_owned())
}

pub fn current(fallback: &str) -> String {
    current_at(&crate::ear_config_path(), fallback)
}

pub(crate) fn save(path: &Path, mode: &str, has_key: bool) -> Result<()> {
    anyhow::ensure!(matches!(mode, "auto" | "groq" | "local"), "未知辨識模式");
    anyhow::ensure!(mode != "groq" || has_key, "沒有 Groq API key，模式未切換");
    let mut value: serde_json::Value = match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b).context("設定檔 JSON 損壞，模式未切換")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e.into()),
    };
    let obj = value.as_object_mut().context("設定檔必須是 JSON object")?;
    obj.insert("backend".into(), mode.into());
    let parent = path.parent().context("設定路徑沒有上層目錄")?;
    std::fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    if let Ok(meta) = std::fs::metadata(path) {
        tmp.as_file().set_permissions(meta.permissions())?;
    }
    serde_json::to_writer_pretty(&mut tmp, &value)?;
    writeln!(tmp)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).context("寫入模式設定失敗")?;
    anyhow::ensure!(current_at(path, "") == mode, "模式設定驗證失敗");
    Ok(())
}

pub fn execute(command: Command, fallback: &str, has_key: bool) -> Result<String> {
    if let Command::Switch(mode) = command {
        save(&crate::ear_config_path(), mode, has_key)?;
    }
    let mode = current(fallback);
    tracing::info!(%mode, "目前語音辨識模式");
    Ok(mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn online_offline_aliases_and_read_only_cleanup_recovery() {
        for text in [
            "Mori 切換成線上模式。",
            "mori切换成线上模式",
            "Mori改用在線模式",
        ] {
            assert_eq!(parse(text), Some(Command::Switch("groq")), "{text}");
            assert_eq!(status_after_cleanup(text), None);
        }
        assert_eq!(parse("毛蕊切換成離線模式"), Some(Command::Switch("local")));
        assert_eq!(parse("貓咪現在是什麼模式？"), Some(Command::Status));
        assert_eq!(
            status_after_cleanup("現在是什麼模式？"),
            Some(Command::Status)
        );
        for text in [
            "切換成現場模式",
            "Mori切換成現場模式",
            "毛蕊現在是怎麼摸的",
            "Mori不要切換成線上模式",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }
    #[test]
    fn status_does_not_require_a_name_but_switches_do() {
        for text in [
            "現在是什麼模式？",
            "什麼模式？",
            "請問現在用哪個模式",
            "Maurice現在在做什麼模式？",
            "现在是什么模式",
            "現在是甚麼模式？",
        ] {
            assert_eq!(parse(text), Some(Command::Status), "{text}");
        }
        for text in [
            "切換到groq模式",
            "幫我換成本機模式",
            "如果只要說是這些什麼模式也沒有反應",
            "我想問現在是什麼模式",
            "這些什麼模式也沒有發現",
            "「什麼模式？」",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
        assert_eq!(
            parse("Maurice幫我換成本機模式"),
            Some(Command::Switch("local"))
        );
    }
    #[test]
    fn morris_and_longer_name_aliases_are_not_dictation() {
        for name in ["Morris", "Moris", "Morley", "Mori", "莫里斯"] {
            assert_eq!(
                parse(&format!("{name}現在是什麼模式？")),
                Some(Command::Status),
                "{name}"
            );
            assert_eq!(
                parse(&format!("{name}切換到groq模式")),
                Some(Command::Switch("groq")),
                "{name}"
            );
            assert_eq!(parse(&format!("{name}不要切換到groq模式")), None);
            assert_eq!(parse(&format!("我說{name}現在是什麼模式")), None);
        }
    }
    #[test]
    fn settings_can_disable_each_feedback_independently() {
        let cfg: Settings = serde_json::from_str(r#"{"enabled":false}"#).unwrap();
        assert!(!cfg.enabled);
        assert!(cfg.sound && cfg.notification);
        let cfg: Settings =
            serde_json::from_str(r#"{"sound":false,"notification":false}"#).unwrap();
        assert!(cfg.enabled);
        assert!(!cfg.sound && !cfg.notification);
    }

    #[test]
    fn live_mode_read_follows_switches_without_reloading_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ear.json");
        assert_eq!(current_at(&path, "auto"), "auto");
        for mode in ["local", "groq", "auto"] {
            save(&path, mode, true).unwrap();
            assert_eq!(current_at(&path, "local"), mode);
        }
        let before = std::fs::read(&path).unwrap();
        assert!(save(&path, "unknown", true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::write(&path, "[]").unwrap();
        assert!(save(&path, "local", true).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[]");
    }
    #[test]
    fn commands_require_a_complete_addressed_utterance() {
        for text in ["Mori，現在是什麼模式？", "hey Mori 现在是什么模式"] {
            assert_eq!(parse(text), Some(Command::Status));
        }
        for (text, mode) in [
            ("Mori 切換到 auto 模式。", "auto"),
            ("mori切换到GROQ模式", "groq"),
            ("mori 切換到 local 模式", "local"),
            ("mori切換到本機模式", "local"),
        ] {
            assert_eq!(parse(text), Some(Command::Switch(mode)));
        }
        for text in [
            "切換到auto模式",
            "我說mori切換到auto模式",
            "mori切換到auto模式然後寫信",
            "「mori切換到auto模式」",
            "mori不要切換到auto模式",
            "mori切換到未知模式",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
        for (text, expected) in [
            ("茉莉，請問你現在用哪個模式啊？", Command::Status),
            ("莫莉幫我換成本機模式", Command::Switch("local")),
            ("Mori 改用 Grok", Command::Switch("groq")),
            ("嘿摩利切到自動模式", Command::Switch("auto")),
        ] {
            assert_eq!(parse(text), Some(expected), "{text}");
        }
    }
    #[test]
    fn save_preserves_settings_and_rejects_unusable_or_broken_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ear.json");
        std::fs::write(
            &path,
            r#"{"backend":"local","secret":"keep","voice_input":{"live_paste":true},"unknown":42}"#,
        )
        .unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(save(&path, "groq", false).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        save(&path, "groq", true).unwrap();
        assert_eq!(current_at(&path, "auto"), "groq");
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["unknown"], 42);
        assert_eq!(v["secret"], "keep");
        assert_eq!(v["voice_input"]["live_paste"], true);
        std::fs::write(&path, "broken").unwrap();
        assert!(save(&path, "local", true).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken");
    }
}

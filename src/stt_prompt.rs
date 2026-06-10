//! STT initial prompt loader.
//!
//! This is Whisper's decoder context, not the cleanup LLM system prompt.
//! Keep it short and vocabulary-oriented.

fn expand_tilde(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        return crate::home_dir().join(rest);
    }
    std::path::PathBuf::from(path)
}

fn read_nonempty(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Resolve STT initial prompt.
///
/// Priority:
/// 1. per-request prompt
/// 2. explicit `stt_initial_prompt_file` in `~/.mori/ear.json`
/// 3. app override `~/.mori/mori-ear/stt-initial-prompt.md`
/// 4. global default `~/.mori/stt/initial-prompt.md`
pub fn resolve(request_prompt: Option<&str>, prompt_file: &str) -> Option<String> {
    if let Some(p) = request_prompt.map(str::trim).filter(|p| !p.is_empty()) {
        return Some(p.to_string());
    }
    if !prompt_file.trim().is_empty() {
        if let Some(p) = read_nonempty(&expand_tilde(prompt_file.trim())) {
            return Some(p);
        }
    }
    let home = crate::home_dir();
    read_nonempty(
        &home
            .join(".mori")
            .join("mori-ear")
            .join("stt-initial-prompt.md"),
    )
    .or_else(|| read_nonempty(&home.join(".mori").join("stt").join("initial-prompt.md")))
}

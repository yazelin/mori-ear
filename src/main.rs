// Windows release build:windowless subsystem,避免 autostart 跳黑框 console。
// 從 terminal 跑時用 AttachConsole(ATTACH_PARENT_PROCESS) 接回父 console,log 照印。
// Debug build 保留 console,方便 `cargo run` 開發時直接看 log。
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

//! mori-ear:Mori 的「耳朵」器官 — 極簡 CLI。
//!
//! 預設流程:
//!   全域熱鍵(toggle 按下) → 開麥克風錄音 → 停頓時分段送 STT
//!   再按一下 → 收尾最後一段 → 可選 cleanup → 印 stdout 並貼回焦點視窗
//!
//! 也支援 hold 模式(按住錄、放開停)、`--input <wav>` 批次轉譯,以及
//! `--serve` loopback HTTP service。`backend=auto` 先找本機 whisper-server,
//! 不可用才 fallback Groq；`raw` 或沒有 cleanup key 時會保留原始 STT。
//!
//! 不做(故意):
//!   - 沒內建 GUI / tray(hold 預覽使用可選的外部 `yad`)
//!   - 沒 voice profile / 校正詞庫
//!
//! 熱鍵有兩條來源,都收斂成同一個 [`KeyEdge`] 餵給 `handle_event`:
//!   - X11 / Windows:`global-hotkey` crate(見 `spawn_hotkey_thread`)
//!   - Wayland:`GlobalShortcuts` portal(見 `wayland_hotkey`)
//!
//! 這是「身體 + 器官」拆分的第一個器官 — mori-desktop 重啟它不重啟,
//! user 永遠有路講話。

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use parking_lot::Mutex;
use serde::Deserialize;
use tracing::{error, info, warn};

mod audio;
mod cleanup;
mod local_stt;
mod multipart;
mod preview;
mod service;
mod stt;
mod stt_prompt;
mod watchdog;
#[cfg(target_os = "linux")]
mod wayland_hotkey;

const DEFAULT_HOTKEY: &str = "Ctrl+Alt+E";

/// 熱鍵的一次邊緣事件 —— hold 用按下/放開,toggle 只消費按下事件。
///
/// 存在的理由是**解耦事件來源**:X11/Windows 走 `global-hotkey` 的
/// [`HotKeyState`],Wayland 走 portal 的 Activated/Deactivated,兩邊收斂成
/// 同一個型別餵給 [`handle_event`],錄音邏輯就不必知道自己跑在哪個顯示協定上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyEdge {
    Pressed,
    Released,
}

impl From<HotKeyState> for KeyEdge {
    fn from(s: HotKeyState) -> Self {
        match s {
            HotKeyState::Pressed => KeyEdge::Pressed,
            HotKeyState::Released => KeyEdge::Released,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Config {
    /// "Ctrl+Alt+E" 之類字串。預設 Ctrl+Alt+E(跟 mori-desktop 的 Ctrl+Alt+Space 不衝)。
    #[serde(default = "default_hotkey")]
    hotkey: String,
    /// Groq API key。空時從 GROQ_API_KEY env 讀。
    #[serde(default)]
    groq_api_key: String,
    /// 轉錄語言提示。空 = 自動。zh / en / ...
    #[serde(default)]
    language: String,
    /// 跳過 LLM cleanup(預設 false = 走 cleanup,跟 mori-desktop voice_input smart level 對齊)。
    /// 設 true → raw Whisper 直送 paste-back,省 200~500ms 跟一輪 token cost,但會有錯字 / 簡體。
    #[serde(default)]
    raw: bool,
    /// cleanup system prompt 來源檔(`.md` / `.txt`)。空 = 用 cleanup::DEFAULT_SYSTEM_PROMPT。
    /// 可指向 `~/.mori/voice_input/USER-00.純文字輸入.md` 跟 mori-desktop 共用。
    /// 每次 cleanup live-read,改 prompt 不必重啟 mori-ear。
    #[serde(default)]
    cleanup_prompt_file: String,
    /// Whisper/Groq STT initial prompt 來源檔。空時依序讀:
    /// `~/.mori/mori-ear/stt-initial-prompt.md` → `~/.mori/stt/initial-prompt.md`。
    /// 這是轉錄 decoder context,不是 cleanup LLM system prompt。
    #[serde(default)]
    stt_initial_prompt_file: String,
    /// 轉錄完是否貼進焦點視窗(預設 true,維持舊行為)。
    /// 設 false → 只印 stdout,不碰 clipboard、不按 Ctrl+V — pipe 用法 / headless 場景適用。
    #[serde(default = "default_paste_back")]
    paste_back: bool,
    /// 貼上時送的按鍵組合(預設 `ctrl+v`)。**只有 Wayland 需要設**。
    ///
    /// X11 / Windows 會用 `xdotool getactivewindow` / `GetForegroundWindow` 偵測
    /// 焦點視窗的 process name 自動決定要不要加 Shift(terminal 要 Ctrl+Shift+V),
    /// 這個欄位對它們沒作用。Wayland 刻意不讓 client 查焦點視窗,偵測不了,
    /// 所以主要在 terminal 打字的人要自己設 `"paste_key": "ctrl+shift+v"`。
    #[serde(default = "default_paste_key")]
    paste_key: String,
    /// STT backend:`auto`(預設,本地 whisper-server 優先、失敗/無法使用 → Groq)
    /// / `groq`(只 Groq)/ `local`(只本地 whisper-server,隱私、不碰 Groq)。
    #[serde(default = "default_backend")]
    backend: String,
    /// 送 STT 前的靜音剪裁(對齊 mori-desktop `config.json` 的 `voice_input.*`)。
    /// ear.json 沒寫整段 → 用預設(剪裁開、min_ms=300、threshold=0.02)。
    #[serde(default)]
    voice_input: VoiceInputConfig,
    /// 一次轉譯(STT+cleanup)的整體上限秒數 —— 看門狗。逾時就放棄該句、不卡死 daemon。
    /// 預設 90s(對 hotkey 短句很寬鬆;長批次檔要轉很久可調大)。
    #[serde(default = "default_transcribe_timeout_secs")]
    transcribe_timeout_secs: u64,
    /// 對外轉譯服務(HTTP `GET /` 驗活、`POST /inference`)。預設開,
    /// 讓 AgentOS(http-service skill)/ mori-desktop 能當 client 消費 ear 的轉錄能力。
    #[serde(default)]
    service: ServiceConfig,
}

/// 對外 HTTP 轉譯服務設定(ear.json `service.*`)。
#[derive(Debug, Clone, Deserialize)]
struct ServiceConfig {
    /// 是否開服務(預設開)。headless / 純 pipe 場景可設 false 關掉,回到「只有 hotkey」的極簡。
    #[serde(default = "default_service_enabled")]
    enabled: bool,
    /// 綁定 port,0 = 由 OS 配 ephemeral(預設,寫進 descriptor 給 client 發現)。
    #[serde(default)]
    port: u16,
}

fn default_service_enabled() -> bool {
    true
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            enabled: default_service_enabled(),
            port: 0,
        }
    }
}

fn default_transcribe_timeout_secs() -> u64 {
    90
}

/// 靜音剪裁設定(ear.json `voice_input.trim_silence_*`,形狀對齊 mori-desktop)。
#[derive(Debug, Clone, Deserialize)]
struct VoiceInputConfig {
    /// 送 STT 前剪首尾 + 中間連續靜音(預設 true)。
    #[serde(default = "default_trim_enabled")]
    trim_silence_enabled: bool,
    /// 中間連續靜音 ≥ 此毫秒才壓掉(預設 300,對齊 mori-desktop)。
    #[serde(default = "default_trim_min_ms")]
    trim_silence_min_ms: u32,
    /// 線性振幅門檻(預設 0.02 ≈ -34 dBFS,對齊 mori-desktop)。
    #[serde(default = "default_trim_threshold")]
    trim_silence_threshold: f32,
    /// 講到一半的停頓就把前段先轉譯掉(預設 true),停止時只剩尾巴要等。
    /// toggle 會把完成的段落直接貼回；hold 仍在放開後一次貼完(按住熱鍵時
    /// X11 keyboard grab 會攔掉注入的 Ctrl+V)。關掉則回到停止後整段送出。
    #[serde(default = "default_stream_enabled")]
    stream_chunks_enabled: bool,
    /// 尾巴連續這麼多毫秒低於門檻就算一個停頓、可以切段(預設 700)。
    #[serde(default = "default_stream_pause_ms")]
    stream_pause_ms: u32,
    /// 一段至少要累積這麼久才准切(預設 1500),避免切出碎片害 Whisper 認不準。
    #[serde(default = "default_stream_min_segment_ms")]
    stream_min_segment_ms: u32,
    /// hold 模式開一個懸浮視窗即時顯示目前解出的文字。需要 `yad`。
    ///
    /// **不設的話跟著 `hotkey_mode` 走**:hold 開、toggle 關。
    /// toggle 是邊講邊貼,字直接出現在游標處,再多一個懸浮視窗只是重複顯示。
    ///
    /// 這是 CLAUDE.md「不加 GUI」那條規則的**明確例外**(2026-08-26 yazelin 拍板):
    /// 語音輸入看不到進度會沒安全感。實作上仍然沒把 GUI 寫進 mori-ear —— 跟
    /// xclip / xdotool 一樣是 spawn 外部程式,mori-ear 只餵文字給它。
    #[serde(default)]
    preview_enabled: Option<bool>,
    /// 轉出來的字超過這個數量,就跳多行編輯視窗讓你先改再送(預設 150)。
    /// 只在 hold 模式有意義 —— toggle 是邊講邊貼,要改直接在游標處改。
    #[serde(default = "default_preview_confirm_chars")]
    preview_confirm_chars: usize,
    /// 熱鍵行為:`toggle`(預設,按一下開始、再按一下停)或 `hold`(按住講話、放開停)。
    ///
    /// 預設 toggle 是為了跟 mori-desktop 的按鈕一致(它也是 toggle),而且 toggle
    /// 期間沒有按鍵被按住,才做得到邊講邊貼。
    ///
    /// toggle 的好處是講話期間沒有按鍵被按住 —— hold 模式下 X11 會因為 XGrabKey
    /// 攔掉我們注入的 Ctrl+V,所以不可能邊講邊貼;toggle 沒這個限制。
    /// 代價是忘記按第二下會一直錄,所以有 `toggle_max_secs` 兜底。
    #[serde(default = "default_hotkey_mode")]
    hotkey_mode: String,
    /// toggle 模式下錄超過這麼久就自動停(預設 120 秒)。設 0 = 不自動停。
    #[serde(default = "default_toggle_max_secs")]
    toggle_max_secs: u64,
    /// 每段轉完就直接貼到游標位。**只有 toggle 模式能用**,不設的話 toggle 自動開啟。
    ///
    /// hold 模式下不可能:按住熱鍵時 X11 的 XGrabKey 會攔掉我們注入的 Ctrl+V,
    /// 實測三段都回報「貼回完成」但只有放開後那段真的落地。toggle 期間沒有按鍵
    /// 被按住,所以沒這個問題。
    ///
    /// 跟 `preview_confirm_chars` 互斥:字都邊講邊貼出去了,就沒有「先給你確認」
    /// 這回事。開了這個,長句確認視窗不會出現。
    #[serde(default)]
    live_paste: Option<bool>,
}

fn default_hotkey_mode() -> String {
    "toggle".to_string()
}

fn default_toggle_max_secs() -> u64 {
    120
}

fn default_preview_confirm_chars() -> usize {
    150
}

fn default_stream_enabled() -> bool {
    true
}

fn default_stream_pause_ms() -> u32 {
    700
}

fn default_stream_min_segment_ms() -> u32 {
    1500
}

fn default_trim_enabled() -> bool {
    true
}

fn default_trim_min_ms() -> u32 {
    300
}

fn default_trim_threshold() -> f32 {
    0.02
}

impl Default for VoiceInputConfig {
    fn default() -> Self {
        Self {
            trim_silence_enabled: default_trim_enabled(),
            trim_silence_min_ms: default_trim_min_ms(),
            trim_silence_threshold: default_trim_threshold(),
            stream_chunks_enabled: default_stream_enabled(),
            stream_pause_ms: default_stream_pause_ms(),
            stream_min_segment_ms: default_stream_min_segment_ms(),
            preview_enabled: None,
            preview_confirm_chars: default_preview_confirm_chars(),
            hotkey_mode: default_hotkey_mode(),
            toggle_max_secs: default_toggle_max_secs(),
            live_paste: None,
        }
    }
}

/// 「講到一半就先送」的參數,由 `VoiceInputConfig` 攤平出來。
#[derive(Clone, Copy, Debug)]
struct StreamConfig {
    enabled: bool,
    pause_ms: u32,
    min_segment_ms: u32,
    /// 判定停頓的振幅門檻,跟剪裁共用一個值(語意一樣:多小算沒在講話)。
    threshold: f32,
}

impl VoiceInputConfig {
    fn to_stream(&self) -> StreamConfig {
        StreamConfig {
            enabled: self.stream_chunks_enabled,
            pause_ms: self.stream_pause_ms,
            min_segment_ms: self.stream_min_segment_ms,
            threshold: self.trim_silence_threshold,
        }
    }

    fn to_trim(&self) -> audio::TrimConfig {
        audio::TrimConfig {
            enabled: self.trim_silence_enabled,
            threshold: self.trim_silence_threshold,
            min_silence_ms: self.trim_silence_min_ms,
        }
    }
}

fn default_paste_back() -> bool {
    true
}

/// Wayland paste-back 的預設按鍵。`ctrl+v` 對絕大多數 GUI app 正確;
/// terminal 需要 `ctrl+shift+v`,但 Wayland 偵測不到焦點視窗,只能讓使用者自己設。
fn default_paste_key() -> String {
    "ctrl+v".to_string()
}

/// 設定裡的 `paste_key`,給 paste-back 路徑讀。
///
/// 用 `OnceLock` 而非一路傳參數:它是啟動時讀一次就不變的值,而
/// `handle_event` 的參數已經多到掛 `#[allow(clippy::too_many_arguments)]`,
/// 再加一個純設定值只會讓簽章更難讀。
#[cfg(target_os = "linux")]
static PASTE_KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();

#[cfg(target_os = "linux")]
fn paste_key() -> &'static str {
    PASTE_KEY.get().map(String::as_str).unwrap_or("ctrl+v")
}

fn default_backend() -> String {
    "auto".into()
}

fn default_hotkey() -> String {
    DEFAULT_HOTKEY.into()
}

impl Config {
    /// 兩層 merge:`~/.mori/ear.json` 提供覆寫,`~/.mori/config.json` 補洞。
    ///
    /// 過去版本是「ear.json 存在 → 整份用它,完全不 fallback」,結果只想用 ear.json
    /// 改 hotkey 的 user(沒寫 groq_api_key 欄位)會被當成 key 沒設；若當時走 Groq,
    /// 就會在啟動直接死在「GROQ_API_KEY 缺」,即使 config.json 早就有共用的 key。
    /// 改成 partial merge:ear.json 沒寫 / 空字串的 groq_api_key 自動補 config.json
    /// 的 `providers.groq.api_key`。
    fn load() -> Self {
        let mut cfg = Self::try_load_path(&ear_config_path()).unwrap_or_else(|| Self {
            hotkey: default_hotkey(),
            groq_api_key: String::new(),
            language: String::new(),
            raw: false,
            cleanup_prompt_file: String::new(),
            stt_initial_prompt_file: String::new(),
            paste_back: true,
            paste_key: default_paste_key(),
            backend: default_backend(),
            voice_input: VoiceInputConfig::default(),
            transcribe_timeout_secs: default_transcribe_timeout_secs(),
            service: ServiceConfig::default(),
        });
        if cfg.groq_api_key.is_empty() {
            if let Some(k) = Self::try_load_groq_key_from_mori(&mori_config_path()) {
                cfg.groq_api_key = k;
            }
        }
        cfg
    }

    fn try_load_path(p: &std::path::Path) -> Option<Self> {
        let text = std::fs::read_to_string(p).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// 從 `~/.mori/config.json` 撈 `providers.groq.api_key`(跟 mori-desktop 同一份)。
    /// placeholder(`REPLACE...`)或空字串都當沒設。
    fn try_load_groq_key_from_mori(p: &std::path::Path) -> Option<String> {
        let text = std::fs::read_to_string(p).ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        v.pointer("/providers/groq/api_key")
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty() && !s.starts_with("REPLACE"))
            .map(|s| s.to_string())
    }

    fn resolved_api_key(&self) -> Option<String> {
        if !self.groq_api_key.is_empty() {
            return Some(self.groq_api_key.clone());
        }
        std::env::var("GROQ_API_KEY").ok().filter(|s| !s.is_empty())
    }
}

/// 跨平台 home dir。
/// - Unix:`$HOME`
/// - Windows:`%USERPROFILE%`(沒設 `HOME` 時 fallback,跟 mori-desktop 同套)
/// 兩個都缺 → 回空 path,下游 read_to_string 就讓它正常失敗
/// (config 缺也能跑,只要 `GROQ_API_KEY` env 有設)
pub(crate) fn home_dir() -> std::path::PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}

fn ear_config_path() -> std::path::PathBuf {
    home_dir().join(".mori").join("ear.json")
}

fn mori_config_path() -> std::path::PathBuf {
    home_dir().join(".mori").join("config.json")
}

/// 解析 "Ctrl+Alt+E" 為 (Modifiers, Code)。
fn parse_hotkey(s: &str) -> Result<HotKey> {
    let mut mods = Modifiers::empty();
    let mut code: Option<Code> = None;
    for part in s.split('+').map(|p| p.trim()) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "alt" => mods |= Modifiers::ALT,
            "shift" => mods |= Modifiers::SHIFT,
            "meta" | "super" | "win" | "cmd" => mods |= Modifiers::META,
            key => {
                let k = key_to_code(key).with_context(|| format!("unknown key: {part}"))?;
                code = Some(k);
            }
        }
    }
    let code = code.context("hotkey 沒指定主鍵")?;
    Ok(HotKey::new(Some(mods), code))
}

fn key_to_code(k: &str) -> Option<Code> {
    Some(match k {
        "a" => Code::KeyA,
        "b" => Code::KeyB,
        "c" => Code::KeyC,
        "d" => Code::KeyD,
        "e" => Code::KeyE,
        "f" => Code::KeyF,
        "g" => Code::KeyG,
        "h" => Code::KeyH,
        "i" => Code::KeyI,
        "j" => Code::KeyJ,
        "k" => Code::KeyK,
        "l" => Code::KeyL,
        "m" => Code::KeyM,
        "n" => Code::KeyN,
        "o" => Code::KeyO,
        "p" => Code::KeyP,
        "q" => Code::KeyQ,
        "r" => Code::KeyR,
        "s" => Code::KeyS,
        "t" => Code::KeyT,
        "u" => Code::KeyU,
        "v" => Code::KeyV,
        "w" => Code::KeyW,
        "x" => Code::KeyX,
        "y" => Code::KeyY,
        "z" => Code::KeyZ,
        "space" => Code::Space,
        "enter" | "return" => Code::Enter,
        "tab" => Code::Tab,
        "esc" | "escape" => Code::Escape,
        _ => return None,
    })
}

/// Windows release(windows_subsystem = "windows")沒自帶 console — 從 terminal 啟動時
/// 主動 attach 回父 process 的 console,讓 log/stdout 還是看得到。
/// scheduled task 啟動(沒父 console)時 AttachConsole 自然失敗,維持完全靜默。
#[cfg(all(target_os = "windows", not(debug_assertions)))]
fn attach_parent_console_if_any() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// global-hotkey 0.6 在 Windows 上 register 到一顆 hidden window,WM_HOTKEY 走 thread
/// message queue —— **caller 必須在同條 thread 跑 GetMessage/DispatchMessage**
/// WindowProc 才會被叫,event 才會 send 進 GlobalHotKeyEvent receiver。
/// (Linux X11 crate 自己 spawn event thread,不受影響。)
///
/// 這條 thread 持有 manager 一直活下去(drop 會 DestroyWindow,所有 hotkey 失效),
/// 然後在裡面跑 GetMessage loop。註冊結果用 sync mpsc 同步回主 thread,失敗就 propagate。
fn spawn_hotkey_thread(hotkey: HotKey, hotkey_label: String) -> Result<()> {
    let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<()>>();
    std::thread::Builder::new()
        .name("mori-ear-hotkey".into())
        .spawn(move || {
            let manager = match GlobalHotKeyManager::new().context("init global hotkey manager") {
                Ok(m) => m,
                Err(e) => {
                    let _ = init_tx.send(Err(e));
                    return;
                }
            };
            if let Err(e) = manager.register(hotkey).with_context(|| {
                format!(
                    "register hotkey {hotkey_label} 失敗 — 可能其他程式(IBus / 桌面環境快捷鍵 / \
                     另一個 mori-ear)也綁了這把,換 ~/.mori/ear.json `hotkey` 欄位試試別組,\
                     例 Ctrl+Alt+Y / Ctrl+Shift+V"
                )
            }) {
                let _ = init_tx.send(Err(e));
                return;
            }
            let _ = init_tx.send(Ok(()));

            // Windows:pump message → WindowProc → GlobalHotKeyEvent::send → receiver。
            // 沒這條 loop 整個 hotkey 鏈路啞掉。
            #[cfg(target_os = "windows")]
            unsafe {
                use windows::Win32::Foundation::HWND;
                use windows::Win32::UI::WindowsAndMessaging::{
                    DispatchMessageW, GetMessageW, TranslateMessage, MSG,
                };
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0).0 > 0 {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            // 非 Windows:crate 已自己 spawn event thread,這條只負責持有 manager 不 drop。
            #[cfg(not(target_os = "windows"))]
            {
                let _keep_alive = manager;
                loop {
                    std::thread::park();
                }
            }

            // Windows 路徑:讓 manager 跟 thread 同壽命(這行讓編譯器知道 manager 不能更早 drop)
            #[cfg(target_os = "windows")]
            drop(manager);
        })
        .context("spawn hotkey thread")?;

    init_rx
        .recv()
        .context("hotkey thread init channel closed")??;
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
    #[cfg(all(target_os = "windows", not(debug_assertions)))]
    attach_parent_console_if_any();

    // log → stderr,讓 stdout 純粹只給 transcript(user pipe 用)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,mori_ear=info")),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    // 極簡 CLI 解析 — 不引入 clap,只認 `--input <file>` 跟 `--help`/`-h`。
    // 沒參數 → 走原本的全域熱鍵 daemon 模式。
    let args: Vec<String> = std::env::args().collect();
    let result = match parse_cli(&args) {
        CliMode::Help => {
            print_help();
            return ExitCode::SUCCESS;
        }
        CliMode::Batch(path) => batch(&path).await,
        CliMode::Serve => serve_only().await,
        CliMode::Daemon => run().await,
        CliMode::Error(msg) => {
            eprintln!("mori-ear: {msg}\n");
            print_help();
            return ExitCode::FAILURE;
        }
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            error!(error = ?e, "mori-ear exited with error");
            ExitCode::FAILURE
        }
    }
}

enum CliMode {
    Daemon,
    Batch(String),
    /// 純轉譯服務模式(無 hotkey、無 hotkey single-instance):只開 HTTP `/inference` + 寫
    /// descriptor。給 mori-desktop / AgentOS 在「需要時自動拉起」用(像 whisper-server 的
    /// lazy-spawn);已有在線服務時自動讓位、直接 exit。
    Serve,
    Help,
    Error(String),
}

fn parse_cli(args: &[String]) -> CliMode {
    let mut i = 1;
    let mut input: Option<String> = None;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => return CliMode::Help,
            "--serve" => return CliMode::Serve,
            "--input" => {
                let Some(v) = args.get(i + 1) else {
                    return CliMode::Error("--input 需要 file path".into());
                };
                input = Some(v.clone());
                i += 2;
            }
            other => return CliMode::Error(format!("未知參數:{other}")),
        }
    }
    match input {
        Some(p) => CliMode::Batch(p),
        None => CliMode::Daemon,
    }
}

fn print_help() {
    eprintln!(
        "mori-ear — Mori 的耳朵 / 語音輸入器官\n\
         \n\
         用法:\n\
           mori-ear                  全域熱鍵 daemon 模式(預設,聽 Ctrl+Alt+E,同時開轉譯服務)\n\
           mori-ear --serve          純轉譯服務模式:只開 HTTP /inference + 寫 descriptor,\n\
                                     無熱鍵、無 hotkey single-instance。給 mori-desktop /\n\
                                     AgentOS 在需要時自動拉起;已有在線服務時自動讓位 exit\n\
           mori-ear --input <file>   batch 模式:轉錄一個音檔 → cleanup → 印 stdout 後 exit\n\
                                     (跳過 single-instance lock、不裝熱鍵、不 paste-back)\n\
                                     支援 Groq Whisper 認的格式:wav/mp3/m4a/flac/webm/ogg\n\
           mori-ear --help           印這段\n\
         \n\
         設定:`~/.mori/ear.json`(可選)— hotkey / cleanup_prompt_file / paste_back / raw 等\n\
         API key:`~/.mori/config.json` 的 providers.groq.api_key 或 env GROQ_API_KEY"
    );
}

/// STT backend 選擇 + fallback。`auto`:**本機優先** — 先試本地 whisper-server,
/// 本地不可用(server 沒起來 / WAV 不可解)才退 Groq(有 key 時);無 key → 只走本地。
/// `groq`/`local` 強制單一 backend。
/// 註:本地路徑需可解碼的 WAV(daemon 路徑由 audio.rs 產 WAV;batch 餵 mp3/m4a 等非 WAV
/// 時本地會在 resample 階段失敗 → auto 模式下退回 Groq)。
pub(crate) async fn transcribe_with_fallback(
    backend: &str,
    api_key: Option<&str>,
    language: &str,
    initial_prompt: Option<&str>,
    wav: Vec<u8>,
) -> Result<String> {
    match backend {
        "groq" => {
            let key = api_key.context("backend=groq 但無 Groq API key")?;
            info!(backend = "groq", "STT 走 Groq 雲端 Whisper");
            stt::transcribe(key, language, initial_prompt, wav).await
        }
        "local" => local_stt::transcribe_default(language, initial_prompt, wav).await,
        _ => {
            // auto:本機優先。本地 whisper-server 拿得到就用(音檔不離機,資料主權);
            // 本地失敗(server 沒起來 / 非 WAV 不可 resample)才退 Groq(有 key 時)。
            match local_stt::transcribe_default(language, initial_prompt, wav.clone()).await {
                Ok(t) => Ok(t),
                Err(e_local) => match api_key {
                    Some(key) => {
                        warn!(error = ?e_local, "本地 whisper-server 不可用 → fallback Groq");
                        stt::transcribe(key, language, initial_prompt, wav).await
                    }
                    None => Err(e_local),
                },
            }
        }
    }
}

/// Batch 模式:讀檔 → STT → cleanup(可選)→ 印 stdout → exit。
/// 不裝 single-instance / 不裝 hotkey / 不 paste-back — 純 pipeline 工具。
async fn batch(input_path: &str) -> Result<ExitCode> {
    let cfg = Config::load();
    // paste-back 路徑不經過 handle_event 的參數鏈,從這裡拿(見 PASTE_KEY 註解)。
    #[cfg(target_os = "linux")]
    let _ = PASTE_KEY.set(cfg.paste_key.clone());
    let api_key = cfg.resolved_api_key();
    if cfg.backend == "groq" && api_key.is_none() {
        anyhow::bail!("backend=groq 但無 Groq API key — 設 GROQ_API_KEY / config.json,或把 backend 設成 auto/local");
    }

    info!(file = %input_path, backend = %cfg.backend, "batch 模式 — 讀檔送 STT");
    let bytes =
        std::fs::read(input_path).with_context(|| format!("讀 input file 失敗:{input_path}"))?;
    if bytes.is_empty() {
        anyhow::bail!("input file 是空檔:{input_path}");
    }

    let timeout = Duration::from_secs(cfg.transcribe_timeout_secs);
    let initial_prompt = stt_prompt::resolve(None, &cfg.stt_initial_prompt_file);
    let raw = watchdog::guard(
        timeout,
        "batch:STT",
        transcribe_with_fallback(
            &cfg.backend,
            api_key.as_deref(),
            &cfg.language,
            initial_prompt.as_deref(),
            bytes,
        ),
    )
    .await
    .context("STT 失敗")?;

    let text = if cfg.raw || api_key.is_none() {
        raw
    } else {
        let key = api_key.as_deref().unwrap();
        let prompt = cleanup::resolve_system_prompt(&cfg.cleanup_prompt_file);
        match cleanup::cleanup(key, &raw, &prompt).await {
            Ok(cleaned) => {
                info!(
                    raw_chars = raw.chars().count(),
                    clean_chars = cleaned.chars().count(),
                    "✓ cleanup OK"
                );
                cleaned
            }
            Err(e) => {
                warn!(error = ?e, "cleanup 失敗,用 raw whisper output");
                raw
            }
        }
    };

    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", text).context("write stdout")?;
    out.flush().ok();
    Ok(ExitCode::SUCCESS)
}

/// 純轉譯服務模式(`--serve`):只開 HTTP `/inference` + 寫 descriptor,無 hotkey、
/// 無 hotkey single-instance lock。給 mori-desktop / AgentOS 在「需要時自動拉起」用
/// (lazy-spawn,像 whisper-server)。已偵測到在線的 mori-ear 服務 → 讓位、直接 exit,
/// 避免雙開雙寫 descriptor。
async fn serve_only() -> Result<ExitCode> {
    let cfg = Config::load();
    if service::existing_service_alive().await {
        info!("已偵測到在線的 mori-ear 轉譯服務,--serve 讓位、直接退出(不重複啟動)");
        return Ok(ExitCode::SUCCESS);
    }
    let _service = service::serve(
        tokio::runtime::Handle::current(),
        service::ServiceParams {
            backend: cfg.backend.clone(),
            api_key: cfg.resolved_api_key(),
            language: cfg.language.clone(),
            cleanup_prompt_file: cfg.cleanup_prompt_file.clone(),
            stt_initial_prompt_file: cfg.stt_initial_prompt_file.clone(),
            skip_cleanup_default: cfg.raw,
            timeout: Duration::from_secs(cfg.transcribe_timeout_secs),
        },
        cfg.service.port,
    )
    .context("啟動 serve-only 轉譯服務")?;
    info!("mori-ear --serve:純轉譯服務上線(無 hotkey)。Ctrl+C / SIGTERM 退出");
    tokio::signal::ctrl_c().await.ok();
    info!("收到 Ctrl+C,serve-only 退出");
    Ok(ExitCode::SUCCESS)
}

async fn run() -> Result<ExitCode> {
    // single-instance lock —— 兩個 mori-ear 同時 grab 同一把熱鍵會撞 X_GrabKey
    // BadAccess(global-hotkey 0.6 不會 propagate 那個 error,結果就是用戶
    // 看 log 寫 "ready" 但熱鍵根本沒生效)。先檢查再啟動。
    // (instance 一定要 own,drop 時才釋放鎖,所以 bind 進 local var 撐到 run() 結束)
    let _instance =
        single_instance::SingleInstance::new("mori-ear-yazelin").context("create instance lock")?;
    if !_instance.is_single() {
        // OS-specific restart hint —— Linux/macOS 是 abstract socket / lock file,
        // Windows 是 named mutex(`single-instance` 內部分流),命令也不同。
        #[cfg(target_os = "windows")]
        let hint = "收掉舊的:工作管理員結束 `mori-ear.exe`,\
                    或 PowerShell `Stop-Process -Force -Name mori-ear`,然後重啟。";
        #[cfg(not(target_os = "windows"))]
        let hint = "收掉舊的:`pkill -9 -x mori-ear`(SIGKILL 強制,確保 abstract socket 立刻釋放),\
                    然後再 `mori-ear &`。\n\
                    或直接用 repo 內 `bash scripts/restart.sh --release` 一鍵搞定。";
        anyhow::bail!(
            "已經有另一個 mori-ear 在跑 — 一個 user session 只能跑一個\
             (X11/Windows 同一把全域熱鍵只能讓一個 process grab)。\n{hint}"
        );
    }

    let cfg = Config::load();
    // paste-back 路徑不經過 handle_event 的參數鏈,從這裡拿(見 PASTE_KEY 註解)。
    #[cfg(target_os = "linux")]
    let _ = PASTE_KEY.set(cfg.paste_key.clone());
    let api_key = cfg.resolved_api_key();
    if cfg.backend == "groq" && api_key.is_none() {
        anyhow::bail!(
            "backend=groq 但無 Groq API key — 設 GROQ_API_KEY / 寫進 ~/.mori/config.json 的 providers.groq.api_key,或把 backend 設成 auto/local"
        );
    }
    info!(
        hotkey = %cfg.hotkey,
        backend = %cfg.backend,
        hotkey_mode = %cfg.voice_input.hotkey_mode,
        "mori-ear ready — 語音輸入熱鍵已啟用"
    );

    // 熱鍵事件的統一入口 —— 不管來源是 X11/Windows 的 global-hotkey 還是 Wayland
    // 的 portal,最後都變成 KeyEdge 從這條 channel 出來。
    let (edge_tx, mut edge_rx) = tokio::sync::mpsc::unbounded_channel::<KeyEdge>();

    // Wayland:先試 portal。成功就完全不碰 X11 —— XGrabKey 在 Wayland 下註冊會
    // 「成功」但永遠收不到事件,兩條並行只會製造混淆。
    // `_portal` 要 hold 到 run() 結束:drop 掉 session,綁定就沒了。
    #[cfg(target_os = "linux")]
    let _portal = if wayland_hotkey::is_wayland() {
        match wayland_hotkey::spawn(&cfg.hotkey, edge_tx.clone()).await {
            Ok(h) => Some(h),
            Err(e) => {
                warn!(
                    error = ?e,
                    "Wayland portal 熱鍵註冊失敗 —— fallback 回 X11(XGrabKey)。\
                     這條路只有在焦點停在 X11 / XWayland 視窗時才收得到按鍵"
                );
                None
            }
        }
    } else {
        None
    };
    #[cfg(target_os = "linux")]
    let use_x11_hotkey = _portal.is_none();
    #[cfg(not(target_os = "linux"))]
    let use_x11_hotkey = true;

    if use_x11_hotkey {
        let hotkey =
            parse_hotkey(&cfg.hotkey).with_context(|| format!("parse hotkey: {}", cfg.hotkey))?;
        // global-hotkey 0.6 在 Windows 上把 RegisterHotKey 綁到 hidden window,WM_HOTKEY
        // 進 thread queue 後**需要那條 thread 自己 pump message** WindowProc 才會被呼叫。
        // tokio runtime 的 worker thread 不 pump Win32 message;直接在 main 建 manager
        // 會 register 成功但 event 永遠收不到。
        // 解法:Windows 上專開一條 OS thread 建 manager + register + 跑 GetMessage loop。
        // Linux X11 crate 自己 spawn event thread,不受影響,維持原本 main thread 路徑即可。
        spawn_hotkey_thread(hotkey, cfg.hotkey.clone())?;

        // global-hotkey 的 receiver 是同步 crossbeam channel,沒有 async 介面。
        // 拿一條 blocking thread 把它抽乾、轉成 KeyEdge 推進統一 channel,
        // 主 loop 就只要 await 一個來源。50ms 的 poll 間隔沿用原本的節奏
        // (熱鍵是人手速度,這個延遲感覺不出來)。
        let tx = edge_tx.clone();
        std::thread::Builder::new()
            .name("mori-ear-hotkey-bridge".into())
            .spawn(move || {
                let rx = GlobalHotKeyEvent::receiver();
                loop {
                    while let Ok(ev) = rx.try_recv() {
                        if tx.send(KeyEdge::from(ev.state)).is_err() {
                            return; // 主 loop 收工了
                        }
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            })
            .context("spawn hotkey bridge thread")?;
    }
    // SIGUSR1 = 一次「按下」。存在的理由:Ubuntu 24.04 的 xdg-desktop-portal 是 1.18,
    // 根本沒有 `org.freedesktop.portal.GlobalShortcuts` 介面(要 1.19+),所以 Wayland
    // 下 portal 註冊必失敗、退回的 XGrabKey 又只在焦點停在 XWayland 視窗時才收得到 ——
    // 熱鍵等於沒有。GNOME 自訂快捷鍵是 compositor 層的,綁 `pkill -USR1 -x mori-ear`
    // 就繞過整條 portal / grab 鏈路,X11 與 Wayland 都會響。
    //
    // 只有「按下」語意(GNOME 快捷鍵拿不到放開),所以配 toggle 模式;hold 模式下
    // 這條路等於「按一下開始,再按一下停」,不會有邊講邊貼以外的差別。
    // ponytail: 用信號不用 HTTP 端點 —— 少一層 port 查找;要跨機觸發再改走 service。
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::user_defined1()) {
            Ok(mut sig) => {
                let tx = edge_tx.clone();
                tokio::spawn(async move {
                    while sig.recv().await.is_some() {
                        info!("SIGUSR1 → 熱鍵按下(ear talk / GNOME 快捷鍵)");
                        if tx.send(KeyEdge::Pressed).is_err() {
                            return; // 主 loop 收工了
                        }
                    }
                });
            }
            Err(e) => warn!(error = ?e, "SIGUSR1 handler 掛不上 —— `ear talk` 不會有反應"),
        }
    }

    // 本地 tx 留著沒用會讓 edge_rx 永遠不 close,drop 掉。
    drop(edge_tx);

    // 共用狀態:目前是不是錄音中。audio::Recorder handle 也存這
    let recorder = Arc::new(Mutex::new(None::<audio::Recorder>));

    // Ctrl+C / SIGTERM graceful shutdown
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    let api_key_arc = Arc::new(api_key); // Arc<Option<String>>:無 key 時走本地 backend
    let lang_arc = Arc::new(cfg.language.clone());
    let raw_arc = Arc::new(cfg.raw);
    let prompt_file_arc = Arc::new(cfg.cleanup_prompt_file.clone());
    let stt_initial_prompt_file_arc = Arc::new(cfg.stt_initial_prompt_file.clone());
    let paste_back_arc = Arc::new(cfg.paste_back);
    let backend_arc = Arc::new(cfg.backend.clone());
    let trim_cfg = cfg.voice_input.to_trim(); // Copy,每輪傳值即可
    let stream_cfg = cfg.voice_input.to_stream();
    if stream_cfg.enabled {
        info!(
            pause_ms = stream_cfg.pause_ms,
            min_segment_ms = stream_cfg.min_segment_ms,
            "講到一半的停頓就先把前段轉譯掉,放開時只等尾巴(voice_input.stream_chunks_enabled)"
        );
    }

    let transcribe_timeout = Duration::from_secs(cfg.transcribe_timeout_secs);
    let pipeline = Pipeline {
        api_key: api_key_arc.clone(),
        language: lang_arc.clone(),
        skip_cleanup: raw_arc.clone(),
        cleanup_prompt_file: prompt_file_arc.clone(),
        stt_initial_prompt_file: stt_initial_prompt_file_arc.clone(),
        paste_back_enabled: paste_back_arc.clone(),
        backend: backend_arc.clone(),
        timeout: transcribe_timeout,
        confirm_chars: cfg.voice_input.preview_confirm_chars,
    };
    let preview_slot: Arc<Mutex<Option<preview::Live>>> = Arc::new(Mutex::new(None));
    let (autostop_tx, mut autostop_rx) = tokio::sync::mpsc::channel::<()>(1);
    let toggle_mode = cfg.voice_input.hotkey_mode.eq_ignore_ascii_case("toggle");
    let toggle_max_secs = cfg.voice_input.toggle_max_secs;
    // 兩種模式各自有合理的預設,沒明寫就跟著 hotkey_mode 走:
    //   hold   —— 按住期間 XGrabKey 會攔掉注入的 Ctrl+V,不可能邊講邊貼,
    //             所以用懸浮視窗給回饋,長句再跳多行編輯。
    //   toggle —— 沒有按鍵被按住,每段直接貼到游標處,懸浮視窗就多餘了。
    let live_paste = cfg.voice_input.live_paste.unwrap_or(toggle_mode)
        && toggle_mode
        && cfg.voice_input.stream_chunks_enabled;
    if cfg.voice_input.live_paste == Some(true) && !live_paste {
        warn!(
            hotkey_mode = %cfg.voice_input.hotkey_mode,
            stream_chunks_enabled = cfg.voice_input.stream_chunks_enabled,
            "live_paste 需要 hotkey_mode=toggle 且 stream_chunks_enabled=true,這次不生效"
        );
    }
    if live_paste {
        info!("邊講邊貼:每段轉完就直接貼到游標位(懸浮視窗與確認視窗因此不需要)");
    }
    let want_preview = cfg.voice_input.preview_enabled.unwrap_or(!live_paste);
    let preview_on = want_preview && preview::available();
    if want_preview && !preview_on {
        warn!("要開預覽視窗但找不到 yad,先關掉(`sudo apt install yad`)");
    }
    if toggle_mode {
        info!(
            toggle_max_secs,
            "熱鍵是 toggle 模式:按一下開始、再按一下停止(voice_input.hotkey_mode)"
        );
    }
    let session = Session {
        recorder: recorder.clone(),
        segments: Arc::new(Mutex::new(Vec::new())),
        chunker: Arc::new(Mutex::new(None)),
        autostop: Arc::new(Mutex::new(None)),
        autostop_tx,
        preview: preview_slot.clone(),
        pipeline: pipeline.clone(),
        trim: trim_cfg,
        stream: stream_cfg,
        preview_on,
        live_paste,
        emit_turn: Arc::new(Emitter::default()),
    };

    // 對外轉譯服務 —— 讓 AgentOS(http-service skill)/ mori-desktop 當 client 消費 ear 的轉錄。
    // `_service` 綁進 daemon 生命週期(drop = unblock server + 刪 mori-ear-server.json descriptor)。
    // 服務跑在自己一條 std thread,每 request 用這個 runtime 的 Handle block_on 跑 async 轉譯。
    let _service = if cfg.service.enabled {
        match service::serve(
            tokio::runtime::Handle::current(),
            service::ServiceParams {
                backend: cfg.backend.clone(),
                api_key: (*api_key_arc).clone(),
                language: cfg.language.clone(),
                cleanup_prompt_file: cfg.cleanup_prompt_file.clone(),
                stt_initial_prompt_file: cfg.stt_initial_prompt_file.clone(),
                skip_cleanup_default: cfg.raw,
                timeout: transcribe_timeout,
            },
            cfg.service.port,
        ) {
            Ok(h) => Some(h),
            Err(e) => {
                warn!(error = ?e, "轉譯服務啟動失敗(hotkey 仍照常;只是 AgentOS/desktop 暫時無法當 client)");
                None
            }
        }
    } else {
        info!("service.enabled=false,跳過對外轉譯服務(只跑 hotkey daemon)");
        None
    };

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("收到 Ctrl+C,退出");
                return Ok(ExitCode::SUCCESS);
            }
            edge = edge_rx.recv() => {
                let Some(edge) = edge else {
                    // 所有 sender 都沒了 —— portal stream 斷線且 X11 bridge 也收工。
                    // 沒有熱鍵來源就沒有存在意義,退出讓 autostart / supervisor 重拉。
                    error!("熱鍵來源全部中斷,退出");
                    return Ok(ExitCode::FAILURE);
                };
                handle_event(edge, session.clone(), toggle_mode, toggle_max_secs).await;
            }
            _ = autostop_rx.recv() => {
                // toggle 逾時看門狗:錄太久了,替使用者收尾
                session.stop();
            }
        }
    }
}

/// 把轉錄文字貼進當前焦點視窗。
///
/// 三條 path,都是「clipboard + 模擬 Ctrl+V」,跟 mori-desktop 同套(逐字 SendInput 太慢):
///   Linux:xclip 寫 CLIPBOARD → xdotool key ctrl+v(terminal 自動改 ctrl+shift+v)
///   Windows:Win32 SetClipboardData → SendInput Ctrl+V(terminal 自動改 Ctrl+Shift+V)
///   macOS:enigo `text()` fallback —— 暫時走 SendInput SCANCODE_UNICODE,
///     未來可改 NSPasteboard + Cmd+V。
fn paste_back(text: &str) -> anyhow::Result<()> {
    // 短暫 sleep 讓 user 釋放 Ctrl+Alt 物理鍵,避免 modifier 被帶進 paste
    std::thread::sleep(std::time::Duration::from_millis(40));

    #[cfg(target_os = "linux")]
    {
        // Wayland 下 xclip/xdotool 只對 XWayland 視窗有效,得換 wl-copy + ydotool。
        // 兩條都失敗才回錯 —— XWayland fallback 對「GUI 是 X11、compositor 是
        // Wayland」的混合環境仍然有用(mori-desktop 就是那種:強制 GDK_BACKEND=x11)。
        if wayland_hotkey::is_wayland() {
            match paste_back_wayland(text, paste_key()) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::warn!(error = ?e, "Wayland paste-back 失敗,試 XWayland(xclip/xdotool)");
                }
            }
        }
        paste_back_linux_clipboard(text)
    }
    #[cfg(target_os = "windows")]
    {
        paste_back_windows_clipboard(text)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        use enigo::{Enigo, Keyboard, Settings};
        let mut enigo = Enigo::new(&Settings::default()).context("init enigo")?;
        enigo.text(text).context("enigo type text")?;
        Ok(())
    }
}

/// Linux: pre_exec hook,close 所有 fd >= 3。
///
/// 為什麼:`single-instance` 0.3 在 Linux 上創 abstract Unix socket 時沒設 FD_CLOEXEC。
/// `xclip` 寫完 fork 為 daemon 守 X11 selection — fork 時繼承了 mori-ear 持的所有 FD,
/// 包括那個 single-instance socket。即使 mori-ear 被 kill,xclip 還活著、socket 還被佔,
/// 新 mori-ear 啟動時 `SingleInstance::is_single()` 回 false → 報「已經有另一個 mori-ear 在跑」
/// 直到 xclip 也被殺。
///
/// SAFETY: pre_exec 內只能用 async-signal-safe 函式。close(2) 跟 getrlimit(2) 都 AS-safe。
#[cfg(target_os = "linux")]
pub(crate) fn pre_exec_close_fds(cmd: &mut std::process::Command) -> &mut std::process::Command {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            let mut rlim: libc::rlimit = std::mem::zeroed();
            let max_fd = if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) == 0 {
                (rlim.rlim_cur as libc::c_long).min(libc::c_int::MAX as libc::c_long) as libc::c_int
            } else {
                1024
            };
            for fd in 3..max_fd {
                libc::close(fd);
            }
            Ok(())
        });
    }
    cmd
}

/// Wayland 的 paste-back:`wl-copy` 寫 clipboard + `ydotool` 注入 Ctrl+V。
///
/// 為什麼不能沿用 X11 那條:`xclip` 寫的是 X11 selection、`xdotool` 走 XTEST,
/// 兩者在 Wayland 下**只對 XWayland 視窗有效**。Wayland-native 視窗看不到那份
/// clipboard,也收不到那個按鍵注入。
///
/// `ydotool` 走 `/dev/uinput` 造一顆虛擬鍵盤,對 compositor 來說跟實體鍵盤沒兩樣,
/// 所以任何視窗都吃 —— 代價是需要 `ydotoold` 在跑、使用者在 `input` 群組。
///
/// # 已知限制:偵測不到焦點視窗
///
/// X11 那條靠 `xdotool getactivewindow` 判斷該送 Ctrl+V 還是 Ctrl+Shift+V。
/// Wayland 刻意不給 client 查詢焦點視窗(GNOME 45+ 連 Shell Eval 也封了),
/// 所以這裡**無法自動偵測 terminal**。預設送 Ctrl+V;terminal 使用者要在
/// `~/.mori/ear.json` 設 `"paste_key": "ctrl+shift+v"`。
#[cfg(target_os = "linux")]
fn paste_back_wayland(text: &str, paste_key: &str) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    // 1. wl-copy 寫 Wayland clipboard
    let mut copy_cmd = Command::new("wl-copy");
    copy_cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = pre_exec_close_fds(&mut copy_cmd)
        .spawn()
        .context("spawn wl-copy — 沒裝?`sudo apt install wl-clipboard ydotool`")?;
    {
        let stdin = child.stdin.as_mut().context("get wl-copy stdin")?;
        stdin
            .write_all(text.as_bytes())
            .context("write to wl-copy")?;
    }
    // wl-copy 跟 xclip 一樣 fork 成 daemon 守著 selection,不 wait(會卡)。
    drop(child);

    std::thread::sleep(std::time::Duration::from_millis(60));

    // 2. ydotool 注入按鍵。它要 ydotoold 的 socket;env 沒設就用預設路徑,
    //    免得 autostart 環境(沒有 shell rc)找不到。
    // 先驗 paste_key 認得,再決定要餵哪種語法(兩版 ydotool 的參數格式不同)。
    let keycodes =
        ydotool_keycodes(paste_key).with_context(|| format!("認不得的 paste_key: {paste_key}"))?;
    let combo: Vec<String> = if ydotool_wants_named_keys() {
        vec![paste_key.trim().to_lowercase()]
    } else {
        keycodes
    };
    let mut yd = Command::new("ydotool");
    yd.arg("key").args(&combo);
    if std::env::var_os("YDOTOOL_SOCKET").is_none() {
        yd.env(
            "YDOTOOL_SOCKET",
            format!("/run/user/{}/.ydotool_socket", unsafe { libc::getuid() }),
        );
    }
    let status = pre_exec_close_fds(&mut yd).status().context(
        "spawn ydotool — 沒裝?`sudo apt install ydotool` 且 `systemctl --user enable --now ydotool`",
    )?;
    if !status.success() {
        anyhow::bail!(
            "ydotool key {paste_key} 失敗({status})—— ydotoold 沒跑?\
             或使用者不在 input 群組(`sudo usermod -aG input $USER` 後重登)"
        );
    }

    tracing::info!(paste_key, "paste-back via wl-copy + ydotool (Wayland)");
    Ok(())
}

/// 這台的 ydotool 要不要「名字」語法(`ctrl+v`)而不是 keycode 語法(`29:1 47:1 ...`)?
///
/// 為什麼要問:0.1.x(Ubuntu 24.04 的版本)只吃名字,1.x 只吃 keycode,而**餵錯版本
/// 不會報錯** —— 實測 0.1.8 收到 `42:1 42:0` 會 exit 0 並送出 keycode 5(數字鍵 4),
/// 也就是焦點視窗被打進一串垃圾字元、log 卻回報「貼回完成」。這是最惡劣的那種
/// 靜默失敗,所以寧可多跑一次 `--help`。
///
/// 判準:0.1.x 的 `key --help` 會寫「separated by plus (+)」,1.x 沒有這句。
/// 問不到(ydotool 不在)就當 1.x —— 新版是往後的預設。
#[cfg(target_os = "linux")]
fn ydotool_wants_named_keys() -> bool {
    static NAMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *NAMED.get_or_init(|| {
        std::process::Command::new("ydotool")
            .args(["key", "--help"])
            .output()
            .map(|o| {
                let help = format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                );
                help.contains("separated by plus")
            })
            .unwrap_or(false)
    })
}

/// 把 `ctrl+shift+v` 轉成 ydotool 的 `<keycode>:<1按下|0放開>` 序列。
///
/// 用的是 Linux input event code(`linux/input-event-codes.h`),不是 ASCII ——
/// 兩者不一樣,別用字元值去算。
#[cfg(target_os = "linux")]
fn ydotool_keycodes(combo: &str) -> Option<Vec<String>> {
    let mut down = Vec::new();
    let mut up = Vec::new();
    for part in combo.split('+') {
        let code = match part.trim().to_lowercase().as_str() {
            "ctrl" | "control" => 29, // KEY_LEFTCTRL
            "shift" => 42,            // KEY_LEFTSHIFT
            "alt" => 56,              // KEY_LEFTALT
            "super" | "meta" => 125,  // KEY_LEFTMETA
            "v" => 47,                // KEY_V
            "insert" => 110,          // KEY_INSERT
            _ => return None,
        };
        down.push(format!("{code}:1"));
        up.insert(0, format!("{code}:0")); // 反序放開,跟真人按鍵一致
    }
    if down.is_empty() {
        return None;
    }
    down.extend(up);
    Some(down)
}

#[cfg(target_os = "linux")]
fn paste_back_linux_clipboard(text: &str) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    // 1. xclip 寫 X11 CLIPBOARD
    let mut xclip_cmd = Command::new("xclip");
    xclip_cmd
        .args(["-selection", "clipboard"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = pre_exec_close_fds(&mut xclip_cmd)
        .spawn()
        .context("spawn xclip — 沒裝?`sudo apt install xclip xdotool`")?;
    {
        let stdin = child.stdin.as_mut().context("get xclip stdin")?;
        stdin.write_all(text.as_bytes()).context("write to xclip")?;
    }
    // xclip 寫完 fork 變 daemon 持有 selection,不 wait(會卡)。
    drop(child);

    // xclip 設好 selection 到實際可讀有少許延遲
    std::thread::sleep(std::time::Duration::from_millis(50));

    // 2. 偵測 active window process name → 決定要不要 +Shift
    let process_name = detect_active_window_process_linux().unwrap_or_default();
    let use_shift = needs_shift_for_paste(&process_name);
    let combo = if use_shift { "ctrl+shift+v" } else { "ctrl+v" };

    // 3. xdotool key 送組合
    let mut xdotool_cmd = Command::new("xdotool");
    xdotool_cmd.args(["key", "--clearmodifiers", combo]);
    let status = pre_exec_close_fds(&mut xdotool_cmd)
        .status()
        .context("spawn xdotool — 沒裝?`sudo apt install xdotool`")?;
    if !status.success() {
        anyhow::bail!("xdotool key {combo} exit non-zero: {status}");
    }

    tracing::info!(
        target_process = %process_name,
        use_shift,
        combo,
        "paste-back via clipboard + xdotool"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn detect_active_window_process_linux() -> Option<String> {
    use std::process::Command;
    let mut cmd = Command::new("xdotool");
    cmd.args(["getactivewindow", "getwindowpid"]);
    let out = pre_exec_close_fds(&mut cmd).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let pid: u32 = String::from_utf8(out.stdout).ok()?.trim().parse().ok()?;
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Terminal app 要 Ctrl+Shift+V(Ctrl+V 在 terminal 是送 literal ^V),其他 Ctrl+V。
#[cfg(target_os = "linux")]
fn needs_shift_for_paste(process_name: &str) -> bool {
    let p = process_name.to_lowercase();
    [
        "gnome-terminal",
        "kgx",
        "ptyxis",
        "kitty",
        "alacritty",
        "wezterm",
        "foot",
        "tilix",
        "terminator",
        "xterm",
        "konsole",
        "urxvt",
        "rxvt",
    ]
    .iter()
    .any(|t| p.contains(t))
}

// =============== Windows paste-back ===============
//
// 跟 mori-desktop 的 selection_windows.rs 同一套設計:
//   1. SetClipboardData(CF_UNICODETEXT) 把字寫進 clipboard
//   2. 偵測焦點視窗 process,決定要 Ctrl+V 還是 Ctrl+Shift+V
//   3. SendInput 注入組合鍵
//
// 為什麼不直接 enigo.text():在 Windows 上 enigo 走 SendInput SCANCODE_UNICODE
// 一字一字打,長段中文會明顯卡頓。Clipboard 路徑是瞬間的。

#[cfg(target_os = "windows")]
fn paste_back_windows_clipboard(text: &str) -> anyhow::Result<()> {
    write_clipboard_unicode_windows(text).context("write Windows clipboard")?;

    // clipboard 寫完到別的 app GetClipboardData 拿到中間有幾 ms latency
    std::thread::sleep(std::time::Duration::from_millis(50));

    let process_name = detect_active_window_process_windows().unwrap_or_default();
    let use_shift = needs_shift_for_paste_windows(&process_name);
    let combo = if use_shift { "Ctrl+Shift+V" } else { "Ctrl+V" };

    send_paste_keys_windows(use_shift).context("SendInput Ctrl+V failed")?;

    tracing::info!(
        target_process = %process_name,
        use_shift,
        combo,
        "paste-back via clipboard + SendInput"
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn write_clipboard_unicode_windows(text: &str) -> anyhow::Result<()> {
    use windows::Win32::Foundation::{HANDLE, HWND};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    // RAII guard 確保中途 ? 早退也會 CloseClipboard —— Windows clipboard 是全 OS
    // 共享資源,沒關會卡其他 app 寫不進去。
    struct ClipboardGuard;
    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }

    // UTF-16 + 結尾 \0(CF_UNICODETEXT 規格要 null-terminated)
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let n_bytes = wide.len() * std::mem::size_of::<u16>();

    unsafe {
        // null HWND owner —— mori-ear 是 CLI,沒視窗,clipboard 就讓系統當 owner
        OpenClipboard(HWND(std::ptr::null_mut())).context("OpenClipboard")?;
        let _guard = ClipboardGuard;

        EmptyClipboard().context("EmptyClipboard")?;
        let h_mem = GlobalAlloc(GMEM_MOVEABLE, n_bytes).context("GlobalAlloc")?;
        let dst = GlobalLock(h_mem);
        if dst.is_null() {
            anyhow::bail!("GlobalLock returned null");
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), dst as *mut u16, wide.len());
        let _ = GlobalUnlock(h_mem);
        // SetClipboardData 後 h_mem 所有權歸 OS,不要再用
        SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(h_mem.0)).context("SetClipboardData")?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn send_paste_keys_windows(use_shift: bool) -> anyhow::Result<()> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        VIRTUAL_KEY, VK_CONTROL, VK_SHIFT, VK_V,
    };

    fn make_key(vk: VIRTUAL_KEY, key_up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if key_up {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    let mut inputs: Vec<INPUT> = Vec::with_capacity(6);
    inputs.push(make_key(VK_CONTROL, false));
    if use_shift {
        inputs.push(make_key(VK_SHIFT, false));
    }
    inputs.push(make_key(VK_V, false));
    inputs.push(make_key(VK_V, true));
    if use_shift {
        inputs.push(make_key(VK_SHIFT, true));
    }
    inputs.push(make_key(VK_CONTROL, true));

    let expected = inputs.len() as u32;
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent != expected {
        anyhow::bail!("SendInput injected {sent}/{expected} events");
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn detect_active_window_process_windows() -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, MAX_PATH};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return None;
    }

    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32)) };
    if pid == 0 {
        return None;
    }

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;

    let mut buf = vec![0u16; MAX_PATH as usize];
    let mut size = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
    };
    unsafe {
        let _ = CloseHandle(handle);
    }

    if ok.is_err() || size == 0 {
        return None;
    }

    let path = String::from_utf16_lossy(&buf[..size as usize]);
    // basename without .exe,跟 Linux 的 /proc/<pid>/comm 對齊(後面 terminal 偵測比對用)
    std::path::Path::new(&path)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

/// Windows 上要 Ctrl+Shift+V 的 terminal —— Windows Terminal / mintty / 跨平台 term。
/// cmd.exe / powershell.exe Ctrl+V 本身就會貼,不算在這。
#[cfg(target_os = "windows")]
fn needs_shift_for_paste_windows(process_name: &str) -> bool {
    let p = process_name.to_lowercase();
    [
        "windowsterminal",
        "wt", // Windows Terminal
        "alacritty",
        "kitty",
        "wezterm", // 跨平台
        "mintty",  // Git Bash / Cygwin
        "conemu",
        "cmder", // 第三方 console
    ]
    .iter()
    .any(|t| p.contains(t))
}

/// 太短 / 太安靜的錄音 Whisper 會幻覺出「謝謝」「請訂閱」,整段跳過。
/// 分段與尾巴共用同一組守門值。
const MIN_DURATION: f32 = 0.25; // 0.25s 以下視為熱鍵誤觸(快按下又放開,沒實際說話)
                                // 註:單音節中文「好 / 對 / 是」一般 0.25~0.35s
                                //     講太快被擋的話再下調 0.18
const MIN_RMS_DB: f32 = -45.0; // -45 dB 以下視為靜音(背景噪音通常 -50 ~ -55 dB)
/// 剪掉靜音之後至少要剩這麼久的人聲才送 STT。
///
/// 平均 RMS 擋不住「1.5 秒裡只有 0.2 秒有聲音」那種:平均起來可能還在門檻之上,
/// 送給 Whisper 就會吐「祝你生日快樂」「謝謝大家再見」「字幕製作」這類訓練資料
/// 尾巴。切段模式下這種音訊特別多 —— 停頓久一點就會切出一段幾乎全靜音的。
const MIN_SPEECH_SECS: f32 = 0.35;

/// 一段語音走完全程需要的東西。每段一份 clone,參數列才不會爆炸。
#[derive(Clone)]
struct Pipeline {
    api_key: Arc<Option<String>>,
    language: Arc<String>,
    skip_cleanup: Arc<bool>,
    cleanup_prompt_file: Arc<String>,
    stt_initial_prompt_file: Arc<String>,
    paste_back_enabled: Arc<bool>,
    backend: Arc<String>,
    timeout: Duration,
    /// 超過這麼多字就不直接貼,留在確認視窗等 Enter
    confirm_chars: usize,
}

impl Pipeline {
    async fn transcribe_and_clean(
        &self,
        label: &str,
        wav: Vec<u8>,
        enc: audio::Encoded,
    ) -> Option<String> {
        let audio::Encoded {
            duration_secs,
            rms_db,
            speech_secs,
        } = enc;
        if wav.is_empty()
            || duration_secs < MIN_DURATION
            || rms_db < MIN_RMS_DB
            || speech_secs < MIN_SPEECH_SECS
        {
            info!(
                label,
                duration_secs,
                rms_db,
                speech_secs,
                "這段沒有足夠人聲,skip STT(避免 Whisper 幻覺「謝謝大家再見」之類)"
            );
            return None;
        }
        let t0 = std::time::Instant::now();
        let (backend, api_key, language, prompt_file) = (
            self.backend.clone(),
            self.api_key.clone(),
            self.language.clone(),
            self.stt_initial_prompt_file.clone(),
        );
        let raw = match watchdog::guard(self.timeout, label, async move {
            let initial_prompt = stt_prompt::resolve(None, &prompt_file);
            transcribe_with_fallback(
                &backend,
                api_key.as_deref(),
                &language,
                initial_prompt.as_deref(),
                wav,
            )
            .await
        })
        .await
        {
            Ok(t) => t,
            Err(e) => {
                error!(label, error = ?e, "轉譯失敗或逾時,放棄這段");
                return None;
            }
        };
        let stt_ms = t0.elapsed().as_millis() as u64;
        if raw.trim().is_empty() {
            info!(label, stt_ms, "這段沒轉出內容");
            return None;
        }

        // LLM cleanup(繁中校正 + 標點 + 簡轉繁)。需 Groq key;skip_cleanup 或無 key
        // (離線)→ 直接用 raw。cleanup 失敗也 fallback raw。
        //
        // 分段之後 cleanup 變成逐段做:接縫處的標點會比整句做差一點,換到的是
        // 「講話當下就看得到字」。要回到整句 cleanup 就關 stream_chunks_enabled。
        let t1 = std::time::Instant::now();
        let text = if *self.skip_cleanup {
            raw
        } else if let Some(key) = self.api_key.as_deref() {
            let prompt = cleanup::resolve_system_prompt(&self.cleanup_prompt_file);
            match cleanup::cleanup(key, &raw, &prompt).await {
                Ok(cleaned) => cleaned,
                Err(e) => {
                    warn!(label, error = ?e, "cleanup 失敗,用 raw whisper output");
                    raw
                }
            }
        } else {
            info!(label, "無 Groq key,跳過 cleanup,用 raw whisper output");
            raw
        };
        info!(
            label,
            duration_secs,
            stt_ms,
            cleanup_ms = t1.elapsed().as_millis() as u64,
            chars = text.chars().count(),
            "✓ 這段完成"
        );
        Some(text)
    }
}

/// 輸出一段文字:stdout 永遠印(pipe 用法靠這),paste-back 可選。
fn emit(text: &str, paste_back_enabled: bool) {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", text);
    let _ = out.flush();
    drop(out);

    if !paste_back_enabled {
        info!(chars = text.chars().count(), "✓ 轉錄完成(paste_back=false,只印 stdout)");
        return;
    }
    match paste_back(text) {
        Ok(()) => info!(chars = text.chars().count(), "✓ 轉錄 + 貼回完成"),
        Err(e) => {
            warn!(error = ?e, "貼回失敗(stdout 還是有印,可自己抓)");
            info!(chars = text.chars().count(), "✓ 轉錄完成");
        }
    }
}

/// 一次錄音會用到的共享狀態。hold 與 toggle 兩種模式共用同一組開始 / 停止。
#[derive(Clone)]
struct Session {
    recorder: Arc<Mutex<Option<audio::Recorder>>>,
    /// 停頓切出去的各段任務,按講話順序;停止時收回來接成一句
    segments: Arc<Mutex<Vec<tokio::task::JoinHandle<Option<String>>>>>,
    chunker: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// toggle 模式的逾時看門狗 —— 忘記按第二下時通知主迴圈收尾。
    /// 只能用訊號不能直接呼叫 stop():錄音器內含 cpal Stream,不是 Send,
    /// 搬不進 tokio task。
    autostop: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    autostop_tx: tokio::sync::mpsc::Sender<()>,
    preview: Arc<Mutex<Option<preview::Live>>>,
    pipeline: Pipeline,
    trim: audio::TrimConfig,
    stream: StreamConfig,
    preview_on: bool,
    /// 每段轉完就貼(只有 toggle 能用)
    live_paste: bool,
    /// live_paste 的輸出排序器:下一個該輪到誰貼
    emit_turn: Arc<Emitter>,
}

/// 讓併行跑完的分段照講話順序輸出。
///
/// 各段的轉譯與 cleanup 是併行的(你還在講話的時候就在跑),完成順序不保證,
/// 所以貼出去之前要在這裡排隊。
#[derive(Default)]
struct Emitter {
    next: Mutex<usize>,
    bell: tokio::sync::Notify,
}

impl Emitter {
    fn reset(&self) {
        *self.next.lock() = 0;
    }

    /// 等到輪到第 `n` 號,執行 `f`,然後放行下一個。
    async fn in_order(&self, n: usize, f: impl FnOnce()) {
        loop {
            if *self.next.lock() == n {
                break;
            }
            self.bell.notified().await;
        }
        f();
        *self.next.lock() = n + 1;
        self.bell.notify_waiters();
    }
}

impl Session {
    fn is_recording(&self) -> bool {
        self.recorder.lock().is_some()
    }

    fn start(&self) {
        let mut slot = self.recorder.lock();
        if slot.is_some() {
            warn!("已在錄音中,忽略");
            return;
        }
        let r = match audio::Recorder::start() {
            Ok(r) => r,
            Err(e) => {
                error!(error = ?e, "錄音啟動失敗");
                return;
            }
        };
        info!("🎤 開始錄音");
        self.segments.lock().clear();
        self.emit_turn.reset();
        if self.preview_on {
            match preview::Live::open("mori-ear") {
                // 不寫佔位文字:空視窗本身就是「在聽了」的訊號,第一段轉出來就填進去。
                // 有佔位文字就得清空才能換掉,一清就閃(見 preview.rs 註解)。
                Ok(w) => *self.preview.lock() = Some(w),
                Err(e) => warn!(error = ?e, "預覽視窗開不起來,照常轉錄"),
            }
        }
        if self.stream.enabled {
            let (buf, stream, trim, live) = (r.buffer(), self.stream, self.trim, self.live_paste);
            let (pipe, segs, pv, emitter) = (
                self.pipeline.clone(),
                self.segments.clone(),
                self.preview.clone(),
                self.emit_turn.clone(),
            );
            let chunker = tokio::spawn(async move {
                let mut n = 0usize;
                loop {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    if buf.secs_buffered() * 1000.0 < stream.min_segment_ms as f32 {
                        continue;
                    }
                    if !buf.tail_is_silent(stream.pause_ms, stream.threshold) {
                        continue;
                    }
                    let Some((wav, enc)) = buf.take_wav(&trim) else {
                        continue;
                    };
                    n += 1;
                    let label = format!("seg{n}");
                    info!(
                        label = %label,
                        duration_secs = enc.duration_secs,
                        speech_secs = enc.speech_secs,
                        "偵測到停頓,前段先送出"
                    );
                    let (p, pv, turn) = (pipe.clone(), pv.clone(), emitter.clone());
                    let idx = n - 1;
                    let task = tokio::spawn(async move {
                        let out = p.transcribe_and_clean(&label, wav, enc).await;
                        if let Some(t) = out.as_deref() {
                            // 純追加,不清空(清空會閃、會殘留,見 preview.rs 註解)
                            if let Some(w) = pv.lock().as_mut() {
                                w.append(t.trim());
                            }
                        }
                        if !live {
                            return out;
                        }
                        // 邊講邊貼:排隊等前面的段落貼完,自己貼完就放行下一個。
                        // 回 None 是因為已經貼出去了,不要再併進停止時那一句。
                        let paste = *p.paste_back_enabled;
                        turn.in_order(idx, || {
                            if let Some(t) = out.as_deref() {
                                if !t.trim().is_empty() {
                                    emit(t.trim(), paste);
                                }
                            }
                        })
                        .await;
                        None
                    });
                    segs.lock().push(task);
                }
            });
            *self.chunker.lock() = Some(chunker);
        }
        *slot = Some(r);
    }

    /// 停止錄音、收回各段、決定直接貼還是等確認。hold 的放開、toggle 的第二下、
    /// 以及 toggle 的逾時看門狗都走這裡。
    fn stop(&self) {
        if let Some(h) = self.autostop.lock().take() {
            h.abort();
        }
        // 先停切段器,免得它跟這裡搶同一份 buffer
        if let Some(h) = self.chunker.lock().take() {
            h.abort();
        }
        let Some(r) = self.recorder.lock().take() else {
            return;
        };
        let done: Vec<_> = std::mem::take(&mut *self.segments.lock());
        // 尾巴可能剛被切段器取走 → 0 samples,那不是錯誤,交給守門判
        let (wav, enc) = r.stop_and_encode_wav(self.trim).unwrap_or_else(|_| {
            (
                Vec::new(),
                audio::Encoded {
                    duration_secs: 0.0,
                    rms_db: -90.0,
                    speech_secs: 0.0,
                },
            )
        });
        info!(
            bytes = wav.len(),
            duration_secs = enc.duration_secs,
            speech_secs = enc.speech_secs,
            earlier_segments = done.len(),
            "錄音停止,尾巴送出"
        );
        let t_stop = std::time::Instant::now();
        // 邊講邊貼時不問確認:前面的段落早就貼出去了,只攔尾巴沒有意義,
        // 而且要改直接在游標處用鍵盤改就好。
        let confirm_on = self.preview_on && !self.live_paste;
        let (pipeline, preview) = (self.pipeline.clone(), self.preview.clone());
        tokio::spawn(async move {
            // 尾巴自己轉;先前各段多半在你講話的時候就跑完了,這裡只是收回來
            let tail = pipeline.transcribe_and_clean("tail", wav, enc).await;
            let mut parts: Vec<String> = Vec::with_capacity(done.len() + 1);
            for task in done {
                match task.await {
                    Ok(Some(t)) if !t.trim().is_empty() => parts.push(t.trim().to_string()),
                    Ok(_) => {}
                    Err(e) => warn!(error = ?e, "某一段的任務沒收回來,略過"),
                }
            }
            if let Some(t) = tail {
                if !t.trim().is_empty() {
                    parts.push(t.trim().to_string());
                }
            }
            preview.lock().take(); // 即時預覽的任務到此為止
            if parts.is_empty() {
                info!("所有分段都沒轉出內容,不貼回");
                return;
            }
            let text = parts.join("");
            let chars = text.chars().count();
            info!(
                segments = parts.len(),
                chars,
                total_after_stop_ms = t_stop.elapsed().as_millis() as u64,
                "停止之後總共等了這麼久"
            );

            if !confirm_on || chars <= pipeline.confirm_chars {
                emit(&text, *pipeline.paste_back_enabled);
                return;
            }
            // 太長 → 留在確認視窗等 Enter。yad 的等待會擋住,丟去 blocking thread。
            info!(chars, threshold = pipeline.confirm_chars, "太長,先給你確認");
            let paste = *pipeline.paste_back_enabled;
            tokio::task::spawn_blocking(move || {
                match preview::confirm("mori-ear — 可以直接改,Enter 送出 / Esc 丟棄", &text) {
                    // 送出的是視窗裡當下的文字,使用者可能改過
                    Ok(preview::Verdict::Send(final_text)) => emit(&final_text, paste),
                    Ok(preview::Verdict::Discard) => info!("你丟棄了這一段"),
                    Err(e) => {
                        warn!(error = ?e, "確認視窗開不起來,直接貼出");
                        emit(&text, paste);
                    }
                }
            });
        });
    }

    /// toggle 模式:忘記按第二下就自己收尾。0 = 不設限。
    fn arm_autostop(&self, max_secs: u64) {
        if max_secs == 0 {
            return;
        }
        let tx = self.autostop_tx.clone();
        let h = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(max_secs)).await;
            warn!(max_secs, "toggle 錄太久了,自動停止收尾(忘記按第二下?)");
            let _ = tx.send(()).await;
        });
        *self.autostop.lock() = Some(h);
    }
}

/// 熱鍵事件 → 開始 / 停止。兩種模式的差別只在這裡。
async fn handle_event(edge: KeyEdge, session: Session, toggle: bool, toggle_max_secs: u64) {
    match (toggle, edge) {
        // hold:按下開始、放開停止
        (false, KeyEdge::Pressed) => session.start(),
        (false, KeyEdge::Released) => session.stop(),
        // toggle:只認按下,一次開一次停;放開不做事
        (true, KeyEdge::Pressed) => {
            if session.is_recording() {
                session.stop();
            } else {
                session.start();
                session.arm_autostop(toggle_max_secs);
            }
        }
        (true, KeyEdge::Released) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 各段的轉譯是併行的,完成順序不保證 —— 但貼出去一定要照講話順序。
    /// 這裡故意讓後面的段先完成,看排序器有沒有把它擋住。
    #[tokio::test]
    async fn emitter_outputs_in_speech_order_even_when_later_segments_finish_first() {
        let emitter = Arc::new(Emitter::default());
        let out = Arc::new(Mutex::new(Vec::<usize>::new()));

        let mut tasks = Vec::new();
        // 倒著送:第 2 段最先呼叫、第 0 段最後
        for n in (0..3).rev() {
            let (e, o) = (emitter.clone(), out.clone());
            tasks.push(tokio::spawn(async move {
                // 讓倒序的呼叫真的先跑進去
                tokio::time::sleep(Duration::from_millis(10 * (2 - n) as u64)).await;
                e.in_order(n, || o.lock().push(n)).await;
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(*out.lock(), vec![0, 1, 2], "輸出必須照講話順序");
    }

    #[tokio::test]
    async fn emitter_reset_starts_a_new_utterance_from_zero() {
        let emitter = Arc::new(Emitter::default());
        let out = Arc::new(Mutex::new(Vec::<usize>::new()));
        emitter.in_order(0, || out.lock().push(0)).await;
        emitter.in_order(1, || out.lock().push(1)).await;
        // 下一句從頭開始編號,沒 reset 的話會永遠等不到
        emitter.reset();
        emitter.in_order(0, || out.lock().push(100)).await;
        assert_eq!(*out.lock(), vec![0, 1, 100]);
    }
}

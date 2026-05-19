//! mori-ear:Mori 的「耳朵」器官 — 極簡 CLI。
//!
//! 流程:
//!   全域熱鍵按下 → 開麥克風錄音
//!   全域熱鍵放開 → 停止錄音 → 編 WAV → POST Groq Whisper → 印 transcript 到 stdout
//!
//! 不做(故意):
//!   - 沒 GUI / tray
//!   - 沒 paste-back(transcript 走 stdout,user 自己 pipe / 抓)
//!   - 沒 cleanup LLM(原始轉錄就送出)
//!   - 沒 voice profile / 校正詞庫
//!   - 沒 Wayland(MVP X11 only)
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
mod stt;

const DEFAULT_HOTKEY: &str = "Ctrl+Alt+E";

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
}

fn default_hotkey() -> String {
    DEFAULT_HOTKEY.into()
}

impl Config {
    fn load() -> Self {
        // 順序:~/.mori/ear.json > ~/.mori/config.json (整份共用) > 預設
        let cfg = Self::try_load_path(&ear_config_path())
            .or_else(|| Self::try_load_path_field(&mori_config_path()))
            .unwrap_or_else(|| Self {
                hotkey: default_hotkey(),
                groq_api_key: String::new(),
                language: String::new(),
                raw: false,
            });
        cfg
    }

    fn try_load_path(p: &std::path::Path) -> Option<Self> {
        let text = std::fs::read_to_string(p).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// 從 ~/.mori/config.json 撈 providers.groq.api_key(跟 mori-desktop 同一份)。
    /// 之後可加 ~/.mori/ear.json overrides 整顆 Config。
    fn try_load_path_field(p: &std::path::Path) -> Option<Self> {
        let text = std::fs::read_to_string(p).ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        let groq_api_key = v
            .pointer("/providers/groq/api_key")
            .and_then(|s| s.as_str())
            .filter(|s| !s.starts_with("REPLACE"))
            .unwrap_or("")
            .to_string();
        Some(Self {
            hotkey: default_hotkey(),
            groq_api_key,
            language: String::new(),
            raw: false,
        })
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
fn home_dir() -> std::path::PathBuf {
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
                let k = key_to_code(key)
                    .with_context(|| format!("unknown key: {part}"))?;
                code = Some(k);
            }
        }
    }
    let code = code.context("hotkey 沒指定主鍵")?;
    Ok(HotKey::new(Some(mods), code))
}

fn key_to_code(k: &str) -> Option<Code> {
    Some(match k {
        "a" => Code::KeyA, "b" => Code::KeyB, "c" => Code::KeyC, "d" => Code::KeyD,
        "e" => Code::KeyE, "f" => Code::KeyF, "g" => Code::KeyG, "h" => Code::KeyH,
        "i" => Code::KeyI, "j" => Code::KeyJ, "k" => Code::KeyK, "l" => Code::KeyL,
        "m" => Code::KeyM, "n" => Code::KeyN, "o" => Code::KeyO, "p" => Code::KeyP,
        "q" => Code::KeyQ, "r" => Code::KeyR, "s" => Code::KeyS, "t" => Code::KeyT,
        "u" => Code::KeyU, "v" => Code::KeyV, "w" => Code::KeyW, "x" => Code::KeyX,
        "y" => Code::KeyY, "z" => Code::KeyZ,
        "space" => Code::Space,
        "enter" | "return" => Code::Enter,
        "tab" => Code::Tab,
        "esc" | "escape" => Code::Escape,
        _ => return None,
    })
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
    // log → stderr,讓 stdout 純粹只給 transcript(user pipe 用)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,mori_ear=info")),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    match run().await {
        Ok(code) => code,
        Err(e) => {
            error!(error = ?e, "mori-ear exited with error");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<ExitCode> {
    // single-instance lock —— 兩個 mori-ear 同時 grab 同一把熱鍵會撞 X_GrabKey
    // BadAccess(global-hotkey 0.6 不會 propagate 那個 error,結果就是用戶
    // 看 log 寫 "ready" 但熱鍵根本沒生效)。先檢查再啟動。
    // (instance 一定要 own,drop 時才釋放鎖,所以 bind 進 local var 撐到 run() 結束)
    let _instance = single_instance::SingleInstance::new("mori-ear-yazelin")
        .context("create instance lock")?;
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
    let api_key = cfg
        .resolved_api_key()
        .context("GROQ_API_KEY 缺 — 設環境變數或寫進 ~/.mori/config.json 的 providers.groq.api_key")?;
    let hotkey = parse_hotkey(&cfg.hotkey).with_context(|| format!("parse hotkey: {}", cfg.hotkey))?;

    info!(hotkey = %cfg.hotkey, "mori-ear ready — 按住熱鍵說話、放開停止");

    let manager = GlobalHotKeyManager::new().context("init global hotkey manager")?;
    manager.register(hotkey).with_context(|| {
        format!(
            "register hotkey {} 失敗 — 可能其他程式(IBus / 桌面環境快捷鍵 / 另一個 mori-ear)也綁了這把,\
             換 ~/.mori/ear.json `hotkey` 欄位試試別組,例 Ctrl+Alt+Y / Ctrl+Shift+V",
            cfg.hotkey
        )
    })?;

    // 共用狀態:目前是不是錄音中。audio::Recorder handle 也存這
    let recorder = Arc::new(Mutex::new(None::<audio::Recorder>));

    // Ctrl+C / SIGTERM graceful shutdown
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    let rx = GlobalHotKeyEvent::receiver();
    let api_key_arc = Arc::new(api_key);
    let lang_arc = Arc::new(cfg.language.clone());
    let raw_arc = Arc::new(cfg.raw);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("收到 Ctrl+C,退出");
                return Ok(ExitCode::SUCCESS);
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                // poll hotkey channel
                while let Ok(ev) = rx.try_recv() {
                    handle_event(
                        ev,
                        recorder.clone(),
                        api_key_arc.clone(),
                        lang_arc.clone(),
                        raw_arc.clone(),
                    ).await;
                }
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

#[cfg(target_os = "linux")]
fn paste_back_linux_clipboard(text: &str) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    // 1. xclip 寫 X11 CLIPBOARD
    let mut child = Command::new("xclip")
        .args(["-selection", "clipboard"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
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
    let status = Command::new("xdotool")
        .args(["key", "--clearmodifiers", combo])
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
    let out = Command::new("xdotool")
        .args(["getactivewindow", "getwindowpid"])
        .output()
        .ok()?;
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
        "gnome-terminal", "kgx", "ptyxis",
        "kitty", "alacritty", "wezterm",
        "foot", "tilix", "terminator", "xterm",
        "konsole", "urxvt", "rxvt",
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
    use windows::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
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
        SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(h_mem.0))
            .context("SetClipboardData")?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn send_paste_keys_windows(use_shift: bool) -> anyhow::Result<()> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_SHIFT, VK_V,
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
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };

    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return None;
    }

    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32)) };
    if pid == 0 {
        return None;
    }

    let handle =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;

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
        "windowsterminal", "wt",       // Windows Terminal
        "alacritty", "kitty", "wezterm", // 跨平台
        "mintty",                       // Git Bash / Cygwin
        "conemu", "cmder",              // 第三方 console
    ]
    .iter()
    .any(|t| p.contains(t))
}

async fn handle_event(
    ev: GlobalHotKeyEvent,
    recorder: Arc<Mutex<Option<audio::Recorder>>>,
    api_key: Arc<String>,
    language: Arc<String>,
    skip_cleanup: Arc<bool>,
) {
    match ev.state {
        HotKeyState::Pressed => {
            let mut slot = recorder.lock();
            if slot.is_some() {
                warn!("hotkey press 但已在錄音中,忽略");
                return;
            }
            match audio::Recorder::start() {
                Ok(r) => {
                    info!("🎤 開始錄音");
                    *slot = Some(r);
                }
                Err(e) => error!(error = ?e, "錄音啟動失敗"),
            }
        }
        HotKeyState::Released => {
            let r = recorder.lock().take();
            let Some(r) = r else {
                return;
            };
            let (wav, duration_secs, rms_db) = match r.stop_and_encode_wav() {
                Ok(w) => w,
                Err(e) => {
                    error!(error = ?e, "WAV 編碼失敗");
                    return;
                }
            };
            // 安靜 / 太短的 audio Whisper 會幻覺出「謝謝」「請訂閱」等,直接 skip
            const MIN_DURATION: f32 = 0.25; // 0.25s 以下視為熱鍵誤觸(快按下又放開,沒實際說話)
                                             // 註:單音節中文「好 / 對 / 是」一般 0.25~0.35s
                                             //     講太快被擋的話再下調 0.18
            const MIN_RMS_DB: f32 = -45.0; // -45 dB 以下視為靜音(背景噪音通常 -50 ~ -55 dB)
            if duration_secs < MIN_DURATION || rms_db < MIN_RMS_DB {
                info!(
                    duration_secs,
                    rms_db,
                    "錄音太短或太安靜,skip STT(避免 Whisper 幻覺「謝謝」之類)"
                );
                return;
            }
            info!(bytes = wav.len(), duration_secs, rms_db, "錄音停止,送 STT");
            // STT 在 spawn 跑,主 loop 不卡
            tokio::spawn(async move {
                let raw = match stt::transcribe(&api_key, &language, wav).await {
                    Ok(t) => t,
                    Err(e) => {
                        error!(error = ?e, "STT 失敗");
                        return;
                    }
                };


                // Step 2:LLM cleanup(繁中校正 + 標點 + 簡轉繁)。失敗 fallback raw。
                let text = if *skip_cleanup {
                    raw
                } else {
                    match cleanup::cleanup(&api_key, &raw).await {
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

                // 預設:同時(a)印到 stdout 給 pipe 用、(b)用 enigo type 進焦點視窗 —
                // user `mori-ear &` 背景跑一次就好,按熱鍵在哪個視窗就出在哪個視窗。
                use std::io::Write as _;
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{}", text);
                let _ = out.flush();
                drop(out);
                match paste_back(&text) {
                    Ok(()) => info!(chars = text.chars().count(), "✓ 轉錄 + 貼回完成"),
                    Err(e) => {
                        warn!(error = ?e, "貼回失敗(stdout 還是有印,可自己抓)");
                        info!(chars = text.chars().count(), "✓ 轉錄完成");
                    }
                }
            });
        }
    }
}


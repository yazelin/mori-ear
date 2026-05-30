// Windows release build:windowless subsystem,避免 autostart 跳黑框 console。
// 從 terminal 跑時用 AttachConsole(ATTACH_PARENT_PROCESS) 接回父 console,log 照印。
// Debug build 保留 console,方便 `cargo run` 開發時直接看 log。
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

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
mod local_stt;
mod multipart;
mod service;
mod stt;
mod watchdog;

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
    /// cleanup system prompt 來源檔(`.md` / `.txt`)。空 = 用 cleanup::DEFAULT_SYSTEM_PROMPT。
    /// 可指向 `~/.mori/voice_input/USER-00.純文字輸入.md` 跟 mori-desktop 共用。
    /// 每次 cleanup live-read,改 prompt 不必重啟 mori-ear。
    #[serde(default)]
    cleanup_prompt_file: String,
    /// 轉錄完是否貼進焦點視窗(預設 true,維持舊行為)。
    /// 設 false → 只印 stdout,不碰 clipboard、不按 Ctrl+V — pipe 用法 / headless 場景適用。
    #[serde(default = "default_paste_back")]
    paste_back: bool,
    /// STT backend:`auto`(預設,Groq 優先、失敗/無 key → 本地 whisper-server)
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
        }
    }
}

impl VoiceInputConfig {
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
    /// 改 hotkey 的 user(沒寫 groq_api_key 欄位)會被當成 key 沒設,啟動直接死在
    /// 「GROQ_API_KEY 缺」 — 即使 config.json 早就有跟 mori-desktop 共用那把 key。
    /// 改成 partial merge:ear.json 沒寫 / 空字串的 groq_api_key 自動補 config.json
    /// 的 `providers.groq.api_key`。
    fn load() -> Self {
        let mut cfg = Self::try_load_path(&ear_config_path()).unwrap_or_else(|| Self {
            hotkey: default_hotkey(),
            groq_api_key: String::new(),
            language: String::new(),
            raw: false,
            cleanup_prompt_file: String::new(),
            paste_back: true,
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

    init_rx.recv().context("hotkey thread init channel closed")??;
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
    wav: Vec<u8>,
) -> Result<String> {
    match backend {
        "groq" => {
            let key = api_key.context("backend=groq 但無 Groq API key")?;
            info!(backend = "groq", "STT 走 Groq 雲端 Whisper");
            stt::transcribe(key, language, wav).await
        }
        "local" => local_stt::transcribe_default(language, wav).await,
        _ => {
            // auto:本機優先。本地 whisper-server 拿得到就用(音檔不離機,資料主權);
            // 本地失敗(server 沒起來 / 非 WAV 不可 resample)才退 Groq(有 key 時)。
            match local_stt::transcribe_default(language, wav.clone()).await {
                Ok(t) => Ok(t),
                Err(e_local) => match api_key {
                    Some(key) => {
                        warn!(error = ?e_local, "本地 whisper-server 不可用 → fallback Groq");
                        stt::transcribe(key, language, wav).await
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
    let api_key = cfg.resolved_api_key();
    if cfg.backend == "groq" && api_key.is_none() {
        anyhow::bail!("backend=groq 但無 Groq API key — 設 GROQ_API_KEY / config.json,或把 backend 設成 auto/local");
    }

    info!(file = %input_path, backend = %cfg.backend, "batch 模式 — 讀檔送 STT");
    let bytes = std::fs::read(input_path)
        .with_context(|| format!("讀 input file 失敗:{input_path}"))?;
    if bytes.is_empty() {
        anyhow::bail!("input file 是空檔:{input_path}");
    }

    let timeout = Duration::from_secs(cfg.transcribe_timeout_secs);
    let raw = watchdog::guard(
        timeout,
        "batch:STT",
        transcribe_with_fallback(&cfg.backend, api_key.as_deref(), &cfg.language, bytes),
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
    let api_key = cfg.resolved_api_key();
    if cfg.backend == "groq" && api_key.is_none() {
        anyhow::bail!(
            "backend=groq 但無 Groq API key — 設 GROQ_API_KEY / 寫進 ~/.mori/config.json 的 providers.groq.api_key,或把 backend 設成 auto/local"
        );
    }
    let hotkey = parse_hotkey(&cfg.hotkey).with_context(|| format!("parse hotkey: {}", cfg.hotkey))?;

    info!(hotkey = %cfg.hotkey, backend = %cfg.backend, "mori-ear ready — 按住熱鍵說話、放開停止");

    // global-hotkey 0.6 在 Windows 上把 RegisterHotKey 綁到 hidden window,WM_HOTKEY
    // 進 thread queue 後**需要那條 thread 自己 pump message** WindowProc 才會被呼叫。
    // tokio runtime 的 worker thread 不 pump Win32 message;直接在 main 建 manager
    // 會 register 成功但 event 永遠收不到。
    // 解法:Windows 上專開一條 OS thread 建 manager + register + 跑 GetMessage loop。
    // Linux X11 crate 自己 spawn event thread,不受影響,維持原本 main thread 路徑即可。
    spawn_hotkey_thread(hotkey, cfg.hotkey.clone())?;

    // 共用狀態:目前是不是錄音中。audio::Recorder handle 也存這
    let recorder = Arc::new(Mutex::new(None::<audio::Recorder>));

    // Ctrl+C / SIGTERM graceful shutdown
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    let rx = GlobalHotKeyEvent::receiver();
    let api_key_arc = Arc::new(api_key); // Arc<Option<String>>:無 key 時走本地 backend
    let lang_arc = Arc::new(cfg.language.clone());
    let raw_arc = Arc::new(cfg.raw);
    let prompt_file_arc = Arc::new(cfg.cleanup_prompt_file.clone());
    let paste_back_arc = Arc::new(cfg.paste_back);
    let backend_arc = Arc::new(cfg.backend.clone());
    let trim_cfg = cfg.voice_input.to_trim(); // Copy,每輪傳值即可
    let transcribe_timeout = Duration::from_secs(cfg.transcribe_timeout_secs);

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
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                // poll hotkey channel
                while let Ok(ev) = rx.try_recv() {
                    handle_event(
                        ev,
                        recorder.clone(),
                        api_key_arc.clone(),
                        lang_arc.clone(),
                        raw_arc.clone(),
                        prompt_file_arc.clone(),
                        paste_back_arc.clone(),
                        backend_arc.clone(),
                        trim_cfg,
                        transcribe_timeout,
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
fn pre_exec_close_fds(cmd: &mut std::process::Command) -> &mut std::process::Command {
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

#[allow(clippy::too_many_arguments)]
async fn handle_event(
    ev: GlobalHotKeyEvent,
    recorder: Arc<Mutex<Option<audio::Recorder>>>,
    api_key: Arc<Option<String>>,
    language: Arc<String>,
    skip_cleanup: Arc<bool>,
    cleanup_prompt_file: Arc<String>,
    paste_back_enabled: Arc<bool>,
    backend: Arc<String>,
    trim: audio::TrimConfig,
    timeout: Duration,
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
            let (wav, duration_secs, rms_db) = match r.stop_and_encode_wav(trim) {
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
                // STT(+cleanup)整段包進看門狗:逾時(transcribe_timeout_secs,預設 90s)就
                // 放棄這句、不讓一輪卡住的轉譯把 daemon 拖住。
                let text = match watchdog::guard(timeout, "hotkey:STT", async move {
                    let raw =
                        transcribe_with_fallback(&backend, api_key.as_deref(), &language, wav).await?;

                    // Step 2:LLM cleanup(繁中校正 + 標點 + 簡轉繁)。需 Groq key;
                    // skip_cleanup 或無 key(離線)→ 直接用 raw。cleanup 失敗也 fallback raw。
                    let text = if *skip_cleanup {
                        raw
                    } else if let Some(key) = api_key.as_deref() {
                        let prompt = cleanup::resolve_system_prompt(&cleanup_prompt_file);
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
                    } else {
                        info!("無 Groq key,跳過 cleanup,用 raw whisper output");
                        raw
                    };
                    Ok::<String, anyhow::Error>(text)
                })
                .await
                {
                    Ok(t) => t,
                    Err(e) => {
                        error!(error = ?e, "轉譯失敗或逾時,放棄這句(STT/cleanup 不行或超時)");
                        return;
                    }
                };

                // (a) stdout 永遠印 — pipe 用法 / `mori-ear > log.txt` 都靠這
                use std::io::Write as _;
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{}", text);
                let _ = out.flush();
                drop(out);

                // (b) paste-back 可選 — config `paste_back: false` 跳過(headless / pure pipe 場景)
                if *paste_back_enabled {
                    match paste_back(&text) {
                        Ok(()) => info!(chars = text.chars().count(), "✓ 轉錄 + 貼回完成"),
                        Err(e) => {
                            warn!(error = ?e, "貼回失敗(stdout 還是有印,可自己抓)");
                            info!(chars = text.chars().count(), "✓ 轉錄完成");
                        }
                    }
                } else {
                    info!(chars = text.chars().count(), "✓ 轉錄完成(paste-back 關閉,只印 stdout)");
                }
            });
        }
    }
}


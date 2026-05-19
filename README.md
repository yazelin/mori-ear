# mori-ear

Mori 宇宙的「耳朵」器官 — 極簡 CLI:**全域熱鍵 → 錄音 → STT → stdout**。

獨立於 [mori-desktop](https://github.com/yazelin/mori-desktop)(身體 / GUI)— 重啟 mori-desktop 時這顆耳朵還在工作,你還是能跟 Mori / Claude / 任何 CLI 講話。

## 為什麼存在

mori-desktop 是 Mori 的身體,把 voice input、tray、sprite、chat panel、annuli 對接都包在一起。開發中改 Rust code 觸發 tauri 自動 rebuild,身體會重啟,語音就斷。

`mori-ear` 把「耳朵」從身體拆出來:獨立 process、獨立 lifecycle、極小依賴。即使身體不在,耳朵照樣聽。

同樣的哲學之後會拆出 `mori-eye`(視覺)、`mori-hand`(輸出)、`mori-lip`(說話)等器官。

## 用法

```sh
# 1. 設 GROQ API key — 兩條路任一:
#    (a) ~/.mori/config.json 的 providers.groq.api_key(跟 mori-desktop 共用)
#    (b) 環境變數
export GROQ_API_KEY=gsk_...

# 2. 裝(到 ~/.cargo/bin/,PATH 上)
cargo install --path .

# 3. 背景跑一次,以後忘記它
mori-ear &

# 4. 任何視窗按住 Ctrl+Alt+E 講話、放開 → 字會 type 進焦點視窗
```

### Windows

```powershell
# 1. GROQ key —— 兩條路任一
#    (a) %USERPROFILE%\.mori\config.json 的 providers.groq.api_key(跟 mori-desktop 共用)
#    (b) 環境變數
setx GROQ_API_KEY gsk_...

# 2. clone repo 後本機建置(cargo install 會把 mori-ear.exe 放到 %USERPROFILE%\.cargo\bin\,要在 PATH)
cargo install --path .

# 3. 背景跑 —— 開 PowerShell 用 Start-Process,或直接 double-click .exe
Start-Process mori-ear -WindowStyle Hidden

# 如果是下載 GitHub Actions 的 mori-ear-windows-x86_64 artifact:
# 先解壓 mori-ear-windows-x86_64.zip,再在 mori-ear.exe 所在資料夾跑:
Start-Process .\mori-ear.exe -WindowStyle Hidden

# 4. 任何視窗按住 Ctrl+Alt+E 講話、放開 → 字會貼進焦點視窗
```

Windows 不用裝 xclip / xdotool —— clipboard + Ctrl+V 走 Win32 API,進程式內部。Terminal(Windows Terminal、mintty、alacritty 等)自動切 Ctrl+Shift+V。

預設行為:**轉錄完同時做兩件事**(a)印 stdout(給 pipe 抓)、(b)貼進當前焦點視窗。Linux/Windows 走 **clipboard + Ctrl+V**(瞬間,跟 mori-desktop 同套);macOS 走 enigo `text()` fallback。所以你不用 pipe — `mori-ear &` 開背景就 work。

## 平台支援

| 環境 | hotkey | paste-back | 備註 |
|---|---|---|---|
| Linux **X11** | ✅ | ✅ | 主要驗證環境;走 xclip + xdotool ctrl+v |
| **Windows** | ✅ | ✅ | Win32 SetClipboardData + SendInput Ctrl+V;config 路徑讀 `%USERPROFILE%\.mori\` |
| macOS | ✅(理論) | ✅(理論) | 第一次跑要授權 Accessibility;paste-back 暫時走 enigo `text()` 逐字 |
| Linux **Wayland** | ⚠️ 部分 | ⚠️ 部分 | XWayland 兜底:**只能跟其他 X11 應用互動**(Wayland-native 視窗無效)。完整 Wayland 支援要加 `ashpd` portal hotkey + `ydotool`/`wtype` paste-back,未來改進 |

範例 pipe(轉錄結果寫到剪貼簿):

```sh
mori-ear | while read -r line; do echo "$line" | xclip -selection clipboard; done
```

## 設定

優先序:`~/.mori/ear.json` > `~/.mori/config.json` 的 `api_keys.GROQ_API_KEY` > 環境變數。

`~/.mori/ear.json` 範例:

```json
{
  "hotkey": "Ctrl+Alt+E",
  "groq_api_key": "gsk_...",
  "language": "zh"
}
```

`language` 空 = 自動偵測。`zh` / `en` / 其他 ISO 639-1 都可。

## 跟 mori-desktop 的關係

mori-ear 是 Mori 宇宙的「**第一個器官**」— 從 mori-desktop(身體)拆出來的獨立 process。設計守則:

- **獨立 lifecycle**:身體掛 / rebuild 時耳朵還在聽
- **獨立發版**:跟 mori-desktop 不同 cargo workspace、不同 repo
- **共用設定**:GROQ key 等讀 `~/.mori/config.json`(跟 mori-desktop 同源)
- **未來整合是 enhancement layer,不是 critical path**:加 IPC 把轉錄結果推進 mori-desktop chat panel、加 mori-desktop ConfigTab → mori-ear sub-tab、加 mori-desktop 啟動時 spawn mori-ear(像 D-1 annuli supervisor)等

同樣的拆法之後輪到 `mori-eye`(視覺 / 截圖)、`mori-hand`(輸出 / OS automation)、`mori-lip`(TTS / 講話)。

## 為什麼不用 mori-desktop?

mori-desktop ≠ mori-ear。前者是身體(GUI、chat、整合),後者是單一器官(只耳朵)。兩者:

- 不同 process,**獨立 lifecycle**
- 不同 repo,**獨立發版**
- 不同熱鍵(預設 `Ctrl+Alt+Space` vs `Ctrl+Alt+E`)
- 共用 `~/.mori/config.json` 的 api keys(不重設)

## 不做(故意)

- 沒 GUI / tray icon
- 沒 paste-back(轉錄字走 stdout,user 自己 pipe / 抓)
- 沒 cleanup LLM(原始 Whisper 轉錄就送出)
- 沒 voice profile / 校正詞庫
- 沒 VAD / noise gate / silence trim

這些都在 mori-desktop 那邊。`mori-ear` 是極簡版,只負責「聽聲音 → 變字」這一步。

## License

MIT OR Apache-2.0

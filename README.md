# mori-ear

Mori 宇宙的「耳朵」器官 — 極簡 CLI:**全域熱鍵 → 錄音 → STT → 自動貼進焦點視窗**。

獨立於 [mori-desktop](https://github.com/yazelin/mori-desktop)(身體 / GUI)— 重啟 mori-desktop 時這顆耳朵還在工作,你還是能跟 Mori / Claude / 任何 CLI 講話。

預設熱鍵 `Ctrl+Alt+E`,後端 Groq Whisper(快、便宜、繁中支援良好),paste-back 走 clipboard + Ctrl+V(瞬間貼上,長段中文也不卡)。

## 為什麼存在

mori-desktop 是 Mori 的身體,把 voice input、tray、sprite、chat panel、annuli 對接都包在一起。開發中改 Rust code 觸發 tauri 自動 rebuild,身體會重啟,語音就斷。

`mori-ear` 把「耳朵」從身體拆出來:獨立 process、獨立 lifecycle、極小依賴。即使身體不在,耳朵照樣聽。

同樣的哲學之後會拆出 `mori-eye`(視覺)、`mori-hand`(輸出)、`mori-lip`(說話)等器官。

---

## 安裝

兩種路徑,挑一個:

- **路徑 A — Prebuilt binary**(快,不用裝 Rust):從 GitHub Actions 抓 zip / tar.gz,解壓即用。
- **路徑 B — From source**(改 code / 沒有 prebuilt 的平台):本機 `cargo install --path .`。

**先把 Groq API key 設好**(兩條任一):

- (i) 寫進 `~/.mori/config.json` 的 `providers.groq.api_key`(跟 mori-desktop 共用)
- (ii) 設環境變數 `GROQ_API_KEY`

沒設的話 mori-ear 啟動會直接報 `GROQ_API_KEY 缺` 退出。

### Windows

#### 路徑 A — Prebuilt(推薦)

```powershell
# 0. 設 Groq key
setx GROQ_API_KEY gsk_xxxxx       # 或寫進 %USERPROFILE%\.mori\config.json

# 1. 從 GitHub Actions 最新的 main build 下載 mori-ear-windows-x86_64
#    https://github.com/yazelin/mori-ear/actions  → 找最新一筆 → Download artifact
#    把 mori-ear-windows-x86_64.zip 解壓到固定路徑,例 C:\Tools\mori-ear\

# 2. 註冊登入後自動啟動(會彈 UAC,點「是」)
cd C:\Tools\mori-ear
powershell -ExecutionPolicy Bypass -File .\install-autostart.ps1

# 3. 馬上啟動(下次登入 scheduled task 會自動跑,所以這步只是當下立刻用)
Start-Process .\mori-ear.exe

# 4. 任何視窗按住 Ctrl+Alt+E 講話、放開 → 字會貼進焦點視窗
```

#### 路徑 B — From source

```powershell
# 0. 一次性裝
#    - Rust:https://rustup.rs/  → rustup-init.exe
#    - "Desktop development with C++" workload(rustup-init 會提示;MSVC linker 需要)
#    - Git for Windows

# 1. 設 Groq key(同上)
setx GROQ_API_KEY gsk_xxxxx

# 2. clone + build + install 到 ~/.cargo/bin
git clone https://github.com/yazelin/mori-ear
cd mori-ear
cargo install --path .

# 3. 註冊登入後自動啟動(會彈 UAC,點「是」)
powershell -ExecutionPolicy Bypass -File .\scripts\install-autostart.ps1

# 4. 啟動
Start-Process mori-ear
```

`install-autostart.ps1` / `remove-autostart.ps1` 會自己偵測非 admin 並 self-elevate(`Start-Process -Verb RunAs`)— 你只會看到一次 UAC 提示。task 註冊在 `\mori-ear`,`RunLevel=Limited`(autostart 跑時是普通使用者權限,沒 admin)。

### Linux

#### 路徑 A — Prebuilt

```sh
# 0. 設 Groq key 跟 paste-back deps
export GROQ_API_KEY=gsk_xxxxx     # 或寫進 ~/.mori/config.json
sudo apt install xclip xdotool    # X11 paste-back 必裝
sudo apt install wl-clipboard ydotool          # Wayland paste-back 必裝
sudo systemctl --user enable --now ydotool
sudo usermod -aG input "$USER"                 # 加完要重新登入

# 1. 從 https://github.com/yazelin/mori-ear/actions 下載最新 mori-ear-linux-x86_64
tar -xzf mori-ear-linux-x86_64.tar.gz
sudo install -m 755 mori-ear /usr/local/bin/mori-ear     # 或丟 ~/.local/bin/

# 2. 登入後自動啟動(XDG autostart entry)
git clone https://github.com/yazelin/mori-ear /tmp/mori-ear-src
bash /tmp/mori-ear-src/scripts/install-autostart.sh

# 3. 馬上跑(下次登入 XDG entry 會自動跑)
mori-ear &
```

#### 路徑 B — From source

```sh
# 0. 一次性裝
#    - Rust:https://rustup.rs/
#    - 系統依賴(以 Ubuntu / Debian 為例;其他發行版自行對應)
sudo apt install pkg-config libasound2-dev libx11-dev xclip xdotool
#      Wayland 另外要(X11 session 可略):
sudo apt install wl-clipboard ydotool
sudo systemctl --user enable --now ydotool     # paste-back 靠這個 daemon
sudo usermod -aG input "$USER"                 # ydotool 要 /dev/uinput,加完要重新登入

# 1. 設 Groq key
export GROQ_API_KEY=gsk_xxxxx

# 2. clone + build + install
git clone https://github.com/yazelin/mori-ear
cd mori-ear
cargo install --path .

# 3. 登入自動啟動
bash scripts/install-autostart.sh

# 4. 立即啟動 / 之後重啟用 scripts/restart.sh
mori-ear &
# 或:bash scripts/restart.sh --release
```

開發中 hot-reload 推薦 `bash scripts/restart.sh`(debug)/ `--release`(發版前驗):編 → kill → relaunch 一條鞭。

### macOS

僅理論支援(沒主要驗證環境)。Path B 走通,paste-back 走 enigo `text()` 逐字 fallback(長段中文會略卡)。第一次跑要授權 System Settings → Privacy & Security → Accessibility。

---

## Linux 便利:`ear` 一鍵 wrapper(選用)

`scripts/ear.sh` 把「開/關/狀態/一鍵裝/一鍵拆/綁 GNOME 快捷鍵」全包成單一 `ear` 指令。clone 完 repo 後:

```sh
# 1. symlink 進 PATH(repo 根目錄)
ln -s "$PWD/scripts/ear.sh" ~/.local/bin/ear

# 2. 一鍵全套裝 — 已裝的會跳過,idempotent
ear install
```

`ear install` 會做五件事(任一已裝就 skip):
1. **檢查 paste-back 依賴** — 依 X11 / Wayland 分別檢查,缺什麼直接印 `apt install` 指令
2. `cargo install --path .` 編譯 + 裝 binary
3. `bash scripts/install-autostart.sh` 寫 XDG autostart entry
4. `gsettings` 綁 Ctrl+Shift+Alt+E → `ear toggle`(GNOME)
5. nohup 啟動 process

依賴缺了**不會擋安裝** —— binary、熱鍵、stdout 都還是能用,只是字貼不進焦點視窗。
補完後跑 `ear deps` 重新確認。

日常用:

```sh
ear              # toggle(沒跑就開、跑就停)— 跟 Ctrl+Shift+Alt+E 同等
ear on / off     # 明確開 / 關
ear status       # 看 process + 各層安裝狀態 + paste-back 依賴
ear deps         # 只檢查 paste-back 依賴(自動分辨 X11 / Wayland)
ear log          # tail /tmp/mori-ear.err
ear keybind on|off    # 只動 GNOME 快捷鍵
ear autostart on|off  # 只動開機自動啟用
ear uninstall    # 反過來全套拆(會問你確認;`ear uninstall --yes` 跳過)
```

需要 `python3` + `gsettings` + `notify-send`(Ubuntu / Fedora / Arch 預設都有)。非 GNOME 桌面 keybind 段自動 skip,其他層照常 work。

paste-back 的外部依賴依 session 分兩組,`ear deps` 會自己判斷該檢查哪組:

| session | 需要 | 額外條件 |
|---|---|---|
| **X11** | `xclip` + `xdotool` | — |
| **Wayland** | `wl-clipboard` + `ydotool` | `ydotoold` 服務要在跑、使用者要在 `input` 群組(加完**必須重新登入**) |

預設快捷鍵 `<Ctrl><Shift><Alt>e`,要換改 `scripts/ear.sh` 頂端 `GS_BINDING` 後重跑 `ear keybind off && ear keybind on`。

---

## 解除安裝

### Windows

```powershell
# 1. 移除 scheduled task(會彈 UAC,點「是」)
powershell -ExecutionPolicy Bypass -File .\scripts\remove-autostart.ps1
# 或從 prebuilt 解壓資料夾:
powershell -ExecutionPolicy Bypass -File .\remove-autostart.ps1

# 2. 殺掉跑著的 process
Stop-Process -Name mori-ear -Force -ErrorAction SilentlyContinue

# 3. 刪 binary
#    cargo install 裝的:
Remove-Item "$env:USERPROFILE\.cargo\bin\mori-ear.exe"
#    prebuilt 解壓的:直接刪整個資料夾

# 4. (選)刪設定 / 環境變數
Remove-Item "$env:USERPROFILE\.mori\ear.json" -ErrorAction SilentlyContinue
# config.json 跟 mori-desktop 共用,不要亂刪
[Environment]::SetEnvironmentVariable("GROQ_API_KEY", $null, "User")
```

### Linux

裝過 `ear` wrapper 的話一句 `ear uninstall` 全套拆完(會問你確認)。手動版:

```sh
# 1. 移除 XDG autostart entry
bash scripts/install-autostart.sh --remove

# 2. 殺進程
pkill -9 -x mori-ear

# 3. 刪 binary
rm ~/.cargo/bin/mori-ear   # 或 sudo rm /usr/local/bin/mori-ear

# 4. (選)刪設定
rm -f ~/.mori/ear.json     # config.json 共用,別刪

# 5. (選,有裝 ear wrapper)
rm -f ~/.local/bin/ear
gsettings reset org.gnome.settings-daemon.plugins.media-keys custom-keybindings  # 注意這會清掉所有自訂快捷鍵
```

---

## 使用

```
按住 Ctrl+Alt+E       → 開麥克風錄音
放開                  → 停止錄音、編 WAV、送 Groq Whisper、cleanup LLM 校正繁中、貼進焦點視窗
```

預設**同時做兩件事**:(a) 印 stdout 給 pipe(`mori-ear | grep ...`)、(b) clipboard + Ctrl+V 進焦點視窗。Terminal 自動切 Ctrl+Shift+V(Windows Terminal / kitty / alacritty / mintty 等)。

太短 / 太安靜的錄音會 skip STT(避免 Whisper 對靜音幻覺出「謝謝」「請訂閱」)— 閾值在 `src/main.rs` 的 `MIN_DURATION`(0.25s)跟 `MIN_RMS_DB`(-45dB)。

---

## 設定

`~/.mori/ear.json`(可選,完整範例):

```json
{
  "hotkey": "Ctrl+Alt+E",
  "groq_api_key": "gsk_...",
  "backend": "auto",
  "language": "zh",
  "raw": false,
  "cleanup_prompt_file": "~/.mori/voice_input/USER-00.純文字輸入.md",
  "stt_initial_prompt_file": "~/.mori/mori-ear/stt-initial-prompt.md",
  "paste_back": true,
  "paste_key": "ctrl+v",
  "voice_input": {
    "trim_silence_enabled": true,
    "trim_silence_min_ms": 300,
    "trim_silence_threshold": 0.02
  }
}
```

- `hotkey`:`Ctrl+Alt+E` / `Ctrl+Shift+V` / `Ctrl+Alt+Y` 等。被別的程式佔走會 register 失敗 — 換一組。
- `groq_api_key`:留空時 fallback 去 `~/.mori/config.json` 的 `providers.groq.api_key`(跟 mori-desktop 共用),再 fallback 環境變數 `GROQ_API_KEY`。
- `backend`:STT 後端 `auto`(預設)/ `groq`(只雲端)/ `local`(只本機、音檔不離機)。`auto` 本機優先、本機不行才 Groq。**本機隨需自啟**:`local`/`auto` 要用本機 whisper-server 但它沒在跑時,mori-ear 會用 `~/.mori/bin/mori-whisper-serve --ensure` 把它叫醒(冪等)、等它 ready(≤15s)再用;裝了那支 supervisor 才有,沒有就 fallback Groq(`auto`)或報錯(`local`)。沒人用滿 10 分鐘 server 自己關。
- `language`:空 = 自動偵測。`zh` / `en` / 其他 ISO 639-1。
- `raw`:`true` = 跳過 cleanup LLM,直接送 raw Whisper 輸出(省 ~200ms 跟一輪 token,但會有錯字 / 簡體)。
- `cleanup_prompt_file`:cleanup LLM 的 system prompt 來源 `.md` / `.txt` 路徑(支援 `~/`)。空 / 檔不存在 → fallback 內建 prompt。**每次 cleanup live-read**,改 prompt 不必重啟 mori-ear。指向 `~/.mori/voice_input/USER-00.純文字輸入.md` 可跟 mori-desktop 共用同一份。
- `stt_initial_prompt_file`:Whisper/Groq STT initial prompt 來源 `.md` / `.txt` 路徑(支援 `~/`)。空時依序讀 `~/.mori/mori-ear/stt-initial-prompt.md`、`~/.mori/stt/initial-prompt.md`。這是**轉錄 decoder context**(專有名詞 / 繁中 / 台灣用語 bias),不是 cleanup LLM system prompt;每次轉錄前 live-read,改檔不用重啟 mori-ear。HTTP `/inference` 也可用 multipart 欄位 `prompt` 臨時覆寫。
- `paste_back`:`true`(預設)= 同時印 stdout + 貼進焦點視窗;`false` = 只印 stdout,不碰 clipboard、不按 Ctrl+V。pipe 用法 / headless / 不想干擾焦點視窗時設 `false`。
- `paste_key`:貼上時送的按鍵組合,預設 `ctrl+v`。**只有 Wayland 需要設**。X11 / Windows 會偵測焦點視窗的 process name 自動決定要不要加 Shift(terminal 用 `Ctrl+Shift+V`),Wayland 刻意不讓 client 查焦點視窗、偵測不了,所以主要在 terminal 打字的人要自己設 `"paste_key": "ctrl+shift+v"`。
- `voice_input`:送 STT 前的靜音剪裁(對齊 mori-desktop 的 `config.json` `voice_input.*`)。`trim_silence_enabled`(預設 `true`)剪掉**首尾靜音 + 中間連續停頓 ≥ `trim_silence_min_ms`**(預設 300ms,短停頓留著給 Whisper 斷句)。`trim_silence_threshold`(預設 0.02,線性振幅 ≈ -34 dBFS)是「多大才算有聲」。設 `trim_silence_enabled: false` → 整段原樣送(舊行為)。**剪裁不影響** duration/RMS 跳過守門(那個用剪裁前整段算)。

只想覆寫一個欄位也行 — ear.json 沒寫的欄位會從 config.json / 預設補回來(partial merge)。

### Batch / pipe 模式 `--input <file>`

不按熱鍵、直接餵音檔轉成文字到 stdout:

```sh
mori-ear --input recording.wav
# Whisper 接受的格式都吃:wav / mp3 / m4a / flac / webm / ogg
# stderr 走 log,stdout 純粹輸出 transcript
```

跟 daemon 模式互不衝突 — batch 跳過 single-instance lock、不裝熱鍵、不 paste-back,純 pipeline 工具。
適合場景:

```sh
mori-ear --input meeting.mp3 > meeting.txt          # 轉錄存檔
mori-ear --input clip.wav | translate -t en         # 後續處理
find ~/recordings -name '*.wav' -exec mori-ear --input {} \; > all.txt  # batch 全跑
```

---

## 平台支援

| 環境 | hotkey | paste-back | 備註 |
|---|---|---|---|
| **Windows** | ✓ | ✓ | 主要驗證環境;Win32 `SetClipboardData` + `SendInput Ctrl+V`;config 路徑 `%USERPROFILE%\.mori\` |
| Linux **X11** | ✓ | ✓ | 走 `xclip` + `xdotool ctrl+v`;Terminal 自動偵測切 `ctrl+shift+v` |
| Linux **Wayland** | ✓ | ✓ | 熱鍵走 `GlobalShortcuts` portal(首次啟動要按同意),paste-back 走 `wl-copy` + `ydotool`。需 `ydotoold` 在跑且使用者在 `input` 群組;terminal 要自己設 `paste_key`(見下) |
| macOS | ✓(理論) | ✓(理論) | 第一次跑要授權 Accessibility;paste-back 走 enigo `text()` 逐字 fallback |

### Linux Wayland 細節

熱鍵走 `org.freedesktop.portal.GlobalShortcuts`(GNOME 45+ / KDE Plasma 6+),不是 X11 的 `XGrabKey` —— 後者在 Wayland 下只有「焦點停在 X11 視窗」時才收得到按鍵,是這顆器官在 Wayland 上曾經完全不響的原因。

**首次啟動會跳授權對話框**(「mori-ear 想註冊 Ctrl+Alt+E」),按同意後綁定持久化,之後啟動都是靜默的。

幾個要知道的:

- **改 `hotkey` 不會自動生效**。portal 規範:使用者同意過之後,實際綁定由 compositor 保管。要改鍵去「設定 → 鍵盤 → 檢視及自訂快捷鍵」,或刪掉 `~/.local/share/xdg-desktop-portal/permissions` 讓 mori-ear 重新註冊。啟動時 log 會印 `actual=` 顯示**實際**綁到什麼,按了沒反應先看那行。
- **偵測不到 terminal**。Wayland 不讓 client 查焦點視窗,所以自動 Ctrl+V / Ctrl+Shift+V 切換在這裡失效 —— terminal 使用者請設 `paste_key`。
- **paste-back 需要 `ydotoold`**。`wl-copy` 寫 clipboard、`ydotool` 透過 `/dev/uinput` 造虛擬鍵盤注入按鍵(所以任何視窗都吃)。daemon 沒跑或使用者不在 `input` 群組就會失敗,失敗時自動退回 XWayland 那條(`xclip`+`xdotool`,只對 X11 視窗有效)。
- **portal 不可用時**(舊桌面環境 / 沒裝 `xdg-desktop-portal-gnome`)自動 fallback 回 X11 熱鍵,log 會 WARN 說明。

範例 pipe(轉錄寫到剪貼簿 + 自己 echo):

```sh
mori-ear | while read -r line; do echo "$line"; echo "$line" | xclip -selection clipboard; done
```

---

## 跟 mori-desktop 的關係

mori-ear 是 Mori 宇宙的「**第一個器官**」— 從 mori-desktop(身體)拆出來的獨立 process。設計守則:

- **獨立 lifecycle**:身體掛 / rebuild 時耳朵還在聽
- **獨立發版**:不同 cargo workspace、不同 repo
- **共用設定**:GROQ key 等讀 `~/.mori/config.json`(跟 mori-desktop 同源)
- **未來整合是 enhancement layer,不是 critical path**:IPC 把轉錄推 mori-desktop chat panel、ConfigTab 加 mori-ear sub-tab、啟動時 spawn mori-ear(像 D-1 annuli supervisor)

同樣的拆法之後輪到 `mori-eye`(視覺 / 截圖)、`mori-hand`(輸出 / OS automation)、`mori-lip`(TTS / 講話)。

---

## 不做(故意)

- 沒 GUI / tray icon
- 沒 voice profile / 校正詞庫
- 沒 VAD / noise gate(但**有**送 STT 前的靜音剪裁 + duration/RMS 守門員,見「設定」的 `voice_input`)
- 沒持久化錄音檔(audio 用完丟,只留 transcript)

這些在 mori-desktop 那邊。`mori-ear` 是極簡版,只負責「聽聲音 → 變字 → 貼進焦點視窗」。

---

## License

MIT OR Apache-2.0

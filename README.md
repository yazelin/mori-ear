# mori-ear

Mori 宇宙的「耳朵」器官 — 極簡 CLI:**全域熱鍵 → 錄音 → STT → 自動貼進焦點視窗**。

獨立於 [mori-desktop](https://github.com/yazelin/mori-desktop)(身體 / GUI)— 重啟 mori-desktop 時這顆耳朵還在工作，你還是能跟 Mori / Claude / 任何 CLI 講話。

預設熱鍵 `Ctrl+Alt+E`、**toggle 模式**(按一下開始、再按一下停，跟 mori-desktop 的語音按鈕一致)。
後端 `auto` 會先用本機 whisper-server，本機不可用才 fallback 到 Groq；paste-back 走 clipboard + Ctrl+V(瞬間貼上，長段中文也不卡)。

**講話期間就在轉譯**：每次你停頓，前一段就先送去 STT，所以停下來的時候多半只剩尾巴要等
(實測停止到貼回 0.9–1.4 秒 → 0.75 秒)。toggle 模式下每段轉完直接貼到游標處，講到一半字就出現了。

### 實測範圍

目前這個新的運作模式只在 Linux X11 實測。Windows 尚未實測；程式裡已有 Windows 的熱鍵與貼回路徑，但 toggle、停頓分段、邊講邊貼等新流程是否穩定仍未知，可能正常，也可能遇到問題。Linux Wayland 與 macOS 也不列入這次新模式的實測保證。

## 為什麼存在

mori-desktop 是 Mori 的身體，把 voice input、tray、sprite、chat panel、annuli 對接都包在一起。開發中改 Rust code 觸發 tauri 自動 rebuild，身體會重啟，語音就斷。

`mori-ear` 把「耳朵」從身體拆出來：獨立 process、獨立 lifecycle、極小依賴。即使身體不在，耳朵照樣聽。

同樣的哲學之後會拆出 `mori-eye`(視覺)、`mori-hand`(輸出)、`mori-lip`(說話)等器官。

---

## 安裝

兩種路徑，挑一個：

- **路徑 A — Prebuilt binary**(快，不用裝 Rust)：從 GitHub Actions 抓 zip / tar.gz，解壓即用。
- **路徑 B — From source**(改 code / 沒有 prebuilt 的平台):本機 `cargo install --path .`。

**Groq API key 是選用的**：`backend=auto` / `backend=local` 可以只靠本機 whisper-server；只有要走 Groq 雲端 STT 或 cleanup LLM 時才需要 key。要設定的話有兩條路，擇一即可：

- (i) 寫進 `~/.mori/config.json` 的 `providers.groq.api_key`(跟 mori-desktop 共用)
- (ii) 設環境變數 `GROQ_API_KEY`

沒設 key 時，`backend=auto` 仍可在本機 whisper-server 可用時工作；只有 `backend=groq` 才會在啟動時報錯。

### Windows

Windows 的安裝流程與 binary 已準備好，但新的 toggle、停頓分段與邊講邊貼流程尚未在 Windows 實測，使用時請預期可能需要額外修正。

#### 路徑 A — Prebuilt(推薦)

```powershell
# 0. (選用) 設 Groq key；只跑本機 backend 時可略過
setx GROQ_API_KEY gsk_xxxxx       # 或寫進 %USERPROFILE%\.mori\config.json

# 1. 從 GitHub Actions 最新的 main build 下載 mori-ear-windows-x86_64
#    https://github.com/yazelin/mori-ear/actions  → 找最新一筆 → Download artifact
#    把 mori-ear-windows-x86_64.zip 解壓到固定路徑,例 C:\Tools\mori-ear\

# 2. 註冊登入後自動啟動(會彈 UAC,點「是」)
cd C:\Tools\mori-ear
powershell -ExecutionPolicy Bypass -File .\install-autostart.ps1

# 3. 馬上啟動(下次登入 scheduled task 會自動跑,所以這步只是當下立刻用)
Start-Process .\mori-ear.exe

# 4. 任何視窗按一下 Ctrl+Alt+E 開始講話、再按一下停止 → 字會貼進焦點視窗
```

#### 路徑 B — From source

```powershell
# 0. 一次性裝
#    - Rust:https://rustup.rs/  → rustup-init.exe
#    - "Desktop development with C++" workload(rustup-init 會提示;MSVC linker 需要)
#    - Git for Windows

# 1. (選用) 設 Groq key；`backend=local` 不需要
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

`install-autostart.ps1` / `remove-autostart.ps1` 會自己偵測非 admin 並 self-elevate(`Start-Process -Verb RunAs`)— 你只會看到一次 UAC 提示。task 註冊在 `\mori-ear`,`RunLevel=Limited`(autostart 跑時是普通使用者權限，沒 admin)。

### Linux

#### 路徑 A — Prebuilt

tarball 內含 `mori-ear` binary + `ear.sh` + `install-autostart.sh`，不需要 clone repo。

```sh
# 0. (選用) 設 Groq key(或寫進 ~/.mori/config.json 跟 mori-desktop 共用)
export GROQ_API_KEY=gsk_xxxxx

#    paste-back 依賴 —— 依你的 session 裝其中一組(不確定就跑 ./ear.sh deps 問它)
#    另外選用:sudo apt install yad —— hold 模式的懸浮預覽 / 長句編輯視窗
#    (缺了只是沒有那個視窗,轉錄與貼回照常)
sudo apt install xclip xdotool                 # X11
sudo apt install wl-clipboard ydotool          # Wayland
sudo systemctl --user enable --now ydotool     # Wayland:paste-back 靠這個 daemon
sudo usermod -aG input "$USER"                 # Wayland:加完必須重新登入才生效

# 1. 從 https://github.com/yazelin/mori-ear/releases 下載最新 tar.gz
tar -xzf mori-ear-linux-x86_64.tar.gz
mkdir -p ~/.local/bin && install -m 755 mori-ear ~/.local/bin/    # 免 sudo
#   或系統層:sudo install -m 755 mori-ear /usr/local/bin/

# 2. 一鍵裝好其餘四層(autostart + GNOME 快捷鍵 + 啟動；另含依賴檢查)
./ear.sh install

# 3. (選)放進 PATH,之後就能直接打 `ear status` / `ear log`
#    兩支都放 —— ear.sh 會去同目錄找 install-autostart.sh,只放前者的話
#    日後 `ear autostart on/off` 會找不到腳本
install -m 755 ear.sh ~/.local/bin/ear
install -m 755 install-autostart.sh ~/.local/bin/
```

`ear.sh` 會自己找 binary(`MORI_EAR_BIN` → `~/.cargo/bin` → `PATH`),所以裝在哪都認得。

**Wayland 首次啟動會跳授權對話框**(「mori-ear 想註冊 Ctrl+Alt+E」)。要按同意，熱鍵才生效。

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

# 1. (選用) 設 Groq key；`backend=local` 不需要
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

`scripts/ear.sh` 把「開/關/狀態/一鍵裝/一鍵拆/綁 GNOME 快捷鍵」全包成單一 `ear` 指令。clone 完 repo 後：

```sh
# 1. symlink 進 PATH(repo 根目錄)
ln -s "$PWD/scripts/ear.sh" ~/.local/bin/ear

# 2. 一鍵全套裝 — 已裝的會跳過,idempotent
ear install
```

`ear install` 會做五件事(任一已裝就 skip):
1. **檢查 paste-back 依賴** — 依 X11 / Wayland 分別檢查，缺什麼直接印 `apt install` 指令
2. `cargo install --path .` 編譯 + 裝 binary
3. `bash scripts/install-autostart.sh` 寫 XDG autostart entry
4. `gsettings` 綁 Ctrl+Shift+Alt+E → `ear toggle`(GNOME)
5. nohup 啟動 process

依賴缺了**不會擋安裝**。binary、熱鍵、stdout 都還是能用，只是字貼不進焦點視窗。
補完後跑 `ear deps` 重新確認。

日常用：

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

paste-back 的外部依賴依 session 分兩組，`ear deps` 會自己判斷該檢查哪組：

| session | 需要 | 額外條件 |
|---|---|---|
| **X11** | `xclip` + `xdotool` | — |
| **Wayland** | `wl-clipboard` + `ydotool` | `ydotoold` 服務要在跑、使用者要在 `input` 群組(加完**必須重新登入**) |

預設快捷鍵 `<Ctrl><Shift><Alt>e`，要換改 `scripts/ear.sh` 頂端 `GS_BINDING` 後重跑 `ear keybind off && ear keybind on`。

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

裝過 `ear` wrapper 的話一句 `ear uninstall` 全套拆完(會問你確認)。手動版：

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

### toggle 模式(預設)

```
按一下 Ctrl+Alt+E   → 開始錄音
講話                 → 每次停頓,那一段就轉譯完直接貼到游標處
再按一下             → 停止,尾巴補上
```

字在你還在講的時候就一段一段出現在游標處，要改直接用鍵盤改。它就落在你原本要打字的地方。
忘記按第二下也不會一直錄下去，`toggle_max_secs`(預設 120 秒)會自己收尾。

### hold 模式(`voice_input.hotkey_mode: "hold"`)

```
按住 Ctrl+Alt+E     → 開始錄音
講話                 → 每次停頓,那一段就先轉譯掉(但先不貼)
放開                 → 只剩尾巴要等,然後整句一起貼
```

**hold 不能邊講邊貼**。原因是 X11 的 active keyboard grab：按住熱鍵時，
mori-ear 注入的 `Ctrl+V` 到不了焦點視窗。實測過，三段都回報「貼回完成」，只有放開之後那一段
真的落地。所以 hold 改用**懸浮視窗**顯示進度，長句再跳編輯視窗讓你先改再送。

### 兩種模式對照

| | toggle(預設) | hold |
|---|---|---|
| 開始 / 停止 | 按一下 / 再按一下 | 按住 / 放開 |
| 分段轉譯 | 支援 | 支援 |
| 邊講邊貼到游標處 | 支援 | 不支援(鍵盤 grab) |
| 懸浮預覽視窗 | 自動關(字已經在游標處) | 自動開(需 `yad`) |
| 長句(>`preview_confirm_chars`) | 不特別處理，直接在游標處改 | 跳多行編輯視窗，**滑鼠點送出、Enter 是換行** |
| 忘記結束 | `toggle_max_secs` 自動收尾 | 放開就結束，不會發生 |

`preview_enabled` 與 `live_paste` 不設的話會**跟著 `hotkey_mode` 自動配好**，不用自己組。

### 輸出

預設**同時做兩件事**：(a) 印 stdout 給 pipe(`mori-ear | grep ...`)、(b) clipboard + Ctrl+V 進焦點視窗。
Terminal 自動切 Ctrl+Shift+V(Windows Terminal / kitty / alacritty / mintty 等)。

### 什麼時候會跳過不轉譯

Whisper 對安靜的音訊會幻覺出「謝謝」「請訂閱」「祝你生日快樂」「字幕製作」這類訓練資料尾巴，
所以有三道守門(`src/main.rs`)：

| 守門 | 值 | 擋什麼 |
|---|---|---|
| `MIN_DURATION` | 0.25s | 熱鍵誤觸，根本沒講話 |
| `MIN_RMS_DB` | -45dB | 整段幾乎是背景噪音 |
| `MIN_SPEECH_SECS` | 0.35s | **剪掉靜音後剩下的人聲**不足。平均音量擋不住「1.5 秒裡只有 0.2 秒有聲音」，而分段模式下停頓久一點就會切出這種段落 |

---

## 設定

`~/.mori/ear.json`(可選，完整範例)：

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
  "transcribe_timeout_secs": 90,
  "service": {
    "enabled": true,
    "port": 0
  },
  "voice_input": {
    "hotkey_mode": "toggle",
    "toggle_max_secs": 120,

    "stream_chunks_enabled": true,
    "stream_pause_ms": 700,
    "stream_min_segment_ms": 1500,

    "trim_silence_enabled": true,
    "trim_silence_min_ms": 300,
    "trim_silence_threshold": 0.02
  }
}
```

`voice_input` 底下還有三個**不寫就跟著 `hotkey_mode` 自動配好**的欄位，列在下面的表裡：
`live_paste`、`preview_enabled`、`preview_confirm_chars`。

- `hotkey`:`Ctrl+Alt+E` / `Ctrl+Shift+V` / `Ctrl+Alt+Y` 等。被別的程式佔走會 register 失敗 — 換一組。
- `groq_api_key`：留空時 fallback 去 `~/.mori/config.json` 的 `providers.groq.api_key`(跟 mori-desktop 共用)，再 fallback 環境變數 `GROQ_API_KEY`。
- `backend`：STT 後端 `auto`(預設) / `groq`(只雲端) / `local`(只本機、音檔不離機)。`auto` 本機優先，本機不行才 Groq。**本機隨需自啟**：`local` / `auto` 要用本機 whisper-server 但它沒在跑時，mori-ear 會用 `~/.mori/bin/mori-whisper-serve --ensure` 把它叫醒(冪等)，等它 ready(≤15s)再用；裝了那支 supervisor 才有，沒有就 fallback Groq(`auto`)或報錯(`local`)。沒人用滿 10 分鐘，server 自己關。
- `language`:空 = 自動偵測。`zh` / `en` / 其他 ISO 639-1。
- `raw`：`true` = 跳過 cleanup LLM，直接送 raw Whisper 輸出(省 ~200ms 跟一輪 token，但會有錯字 / 簡體)。
- `cleanup_prompt_file`：cleanup LLM 的 system prompt 來源 `.md` / `.txt` 路徑(支援 `~/`)。空 / 檔不存在 → fallback 內建 prompt。**每次 cleanup live-read**，改 prompt 不必重啟 mori-ear。指向 `~/.mori/voice_input/USER-00.純文字輸入.md` 可跟 mori-desktop 共用同一份。
- `stt_initial_prompt_file`：Whisper/Groq STT initial prompt 來源 `.md` / `.txt` 路徑(支援 `~/`)。空時依序讀 `~/.mori/mori-ear/stt-initial-prompt.md`、`~/.mori/stt/initial-prompt.md`。這是**轉錄 decoder context**(專有名詞 / 繁中 / 台灣用語 bias)，不是 cleanup LLM system prompt；每次轉錄前 live-read，改檔不用重啟 mori-ear。HTTP `/inference` 也可用 multipart 欄位 `prompt` 臨時覆寫。
- `paste_back`：`true`(預設) = 同時印 stdout + 貼進焦點視窗；`false` = 只印 stdout，不碰 clipboard、不按 Ctrl+V。pipe 用法 / headless / 不想干擾焦點視窗時設 `false`。
- `transcribe_timeout_secs`：一次 STT + cleanup 的整體上限(預設 `90` 秒)。逾時只放棄該句，daemon 仍會繼續跑；長批次檔可調大。
- `service`：對外的 loopback HTTP 轉譯服務。`enabled` 預設 `true`；`port: 0` 讓 OS 配 ephemeral port，實際 port 會寫進 `~/.mori/mori-ear-server.json`。只想跑熱鍵時設 `"enabled": false`。
- `paste_key`：貼上時送的按鍵組合，預設 `ctrl+v`。**只有 Wayland 需要設**。X11 / Windows 會偵測焦點視窗的 process name 自動決定要不要加 Shift(terminal 用 `Ctrl+Shift+V`)，Wayland 刻意不讓 client 查焦點視窗、偵測不了，所以主要在 terminal 打字的人要自己設 `"paste_key": "ctrl+shift+v"`。
- `voice_input`：送 STT 前的靜音剪裁(對齊 mori-desktop 的 `config.json` `voice_input.*`)。`trim_silence_enabled`(預設 `true`)剪掉**首尾靜音 + 中間連續停頓 ≥ `trim_silence_min_ms`**(預設 300ms，短停頓留著給 Whisper 斷句)。`trim_silence_threshold`(預設 0.02，線性振幅 ≈ -34 dBFS)是「多大才算有聲」。設 `trim_silence_enabled: false` → 整段原樣送(舊行為)。**剪裁不影響** duration/RMS 跳過守門(那個用剪裁前整段算)。

### `voice_input` 完整欄位

**熱鍵行為**

| 欄位 | 預設 | 說明 |
|---|---|---|
| `hotkey_mode` | `"toggle"` | `toggle` 按一下開始、再按一下停；`hold` 按住講話、放開停。預設 toggle 是為了跟 mori-desktop 的語音按鈕一致 |
| `toggle_max_secs` | `120` | toggle 錄超過這麼久自動收尾(忘記按第二下的保險)。`0` = 不設限 |

**講到一半就先轉譯**

| 欄位 | 預設 | 說明 |
|---|---|---|
| `stream_chunks_enabled` | `true` | 講話期間偵測停頓，前段先送 STT。`false` = 舊行為(結束才整段送) |
| `stream_pause_ms` | `700` | 尾巴連續靜音這麼久算一個停頓、可以切段。換氣就被切的話往上調 |
| `stream_min_segment_ms` | `1500` | 一段至少要累積這麼久才准切，免得切出碎片害 Whisper 認不準 |

開了分段之後 cleanup 會變成**逐段做**，接縫的標點會比整句一起做差一點。換到的是等待變短。

**輸出與預覽**(這三個不寫就跟著 `hotkey_mode` 走)

| 欄位 | 不寫時 | 說明 |
|---|---|---|
| `live_paste` | toggle → `true`，hold → `false` | 每段轉完直接貼到游標處。**只有 toggle 能用**，hold 會被鍵盤 grab 擋掉(見「使用」) |
| `preview_enabled` | 邊講邊貼時 `false`,否則 `true` | 懸浮視窗顯示即時進度。需要 `yad`,沒裝就自動關掉、轉錄照跑 |
| `preview_confirm_chars` | `150` | 超過這個字數跳多行編輯視窗讓你先改再送。邊講邊貼時不生效(字都出去了)。設 `0` = 每一句都問 |

**靜音剪裁**

| 欄位 | 預設 | 說明 |
|---|---|---|
| `trim_silence_enabled` | `true` | 送 STT 前剪掉首尾靜音 + 中間連續停頓 |
| `trim_silence_min_ms` | `300` | 中間連續靜音 ≥ 此值才壓掉(短停頓留給 Whisper 斷句) |
| `trim_silence_threshold` | `0.02` | 線性振幅 ≈ -34 dBFS,「多大才算有聲」。分段的停頓判定也用這個值 |

剪裁**不影響** duration / RMS 那兩道跳過守門(它們用剪裁前的整段算)，但第三道 `MIN_SPEECH_SECS`
就是看剪裁後剩多少。見「使用」章節的守門表。

只想覆寫一個欄位也行 — ear.json 沒寫的欄位會從 config.json / 預設補回來(partial merge)。

### Batch / pipe 模式 `--input <file>`

不按熱鍵、直接餵音檔轉成文字到 stdout:

```sh
mori-ear --input recording.wav
# Whisper 接受的格式都吃:wav / mp3 / m4a / flac / webm / ogg
# stderr 走 log,stdout 純粹輸出 transcript
```

跟 daemon 模式互不衝突。batch 跳過 single-instance lock、不裝熱鍵、不 paste-back，純 pipeline 工具。
`backend=local` 的本機 whisper-server 只接受可解碼的 WAV；`backend=auto` 餵 mp3/m4a/flac 等格式時，會在本機路徑失敗後用 Groq fallback（所以需要 Groq key）。
適合場景：

```sh
mori-ear --input meeting.mp3 > meeting.txt          # 轉錄存檔
mori-ear --input clip.wav | translate -t en         # 後續處理
find ~/recordings -name '*.wav' -exec mori-ear --input {} \; > all.txt  # batch 全跑
```

### 純轉譯服務模式 `--serve`

daemon 預設會同時開 hotkey 與 loopback HTTP service；如果只需要讓 mori-desktop / AgentOS
按需拉起一個轉譯 provider，可以用：

```sh
mori-ear --serve
```

`--serve` 不註冊 hotkey，也不取得 hotkey 用的 single-instance lock。它會先檢查
`~/.mori/mori-ear-server.json` 是否已有在線服務；已有就直接讓位，沒有才綁
`127.0.0.1:<port>` 並寫入 descriptor。服務提供：

| 方法 | 路徑 | 行為 |
|---|---|---|
| `GET` | `/`、`/health` | 回 `mori-ear ok`，給 client 做 ready gate |
| `POST` | `/inference` | `multipart/form-data` 的 `file`(必填) → `{"text":"..."}` |

`POST /inference` 另接受 `language`、`prompt`、`backend`(`auto` / `groq` / `local`)
與 `cleanup`。`cleanup=false`、`0`、`raw` 或 `none` 會跳過繁中 cleanup；`backend=local`
可以要求這一請求只走本機 whisper-server。服務只綁 loopback，不是對外公開的 HTTP server。

如果要關掉 daemon 裡的 service，在 `ear.json` 設：

```json
{
  "service": { "enabled": false }
}
```

---

## 平台支援

| 環境 | hotkey | paste-back | 備註 |
|---|---|---|---|
| **Windows** | 尚未實測 | 尚未實測 | 已有 Win32 `SetClipboardData` + `SendInput Ctrl+V` 路徑，但新的 toggle、分段轉譯與邊講邊貼流程尚未驗證，可能正常，也可能遇到問題；config 路徑 `%USERPROFILE%\.mori\` |
| Linux **X11** | 已實測 | 已實測 | 目前新運作模式的主要驗證環境；走 `xclip` + `xdotool ctrl+v`，Terminal 自動偵測切 `ctrl+shift+v` |
| Linux **Wayland** | 未實測 | 未實測 | 程式已有 `GlobalShortcuts` portal、`wl-copy` + `ydotool` 路徑，但新的運作模式不在目前實測範圍；需 `ydotoold` 在跑且使用者在 `input` 群組 |
| macOS | 理論支援 | 理論支援 | 第一次跑要授權 Accessibility；paste-back 走 enigo `text()` 逐字 fallback |

### Linux Wayland 細節

熱鍵走 `org.freedesktop.portal.GlobalShortcuts`(GNOME 45+ / KDE Plasma 6+)，不是 X11 的 `XGrabKey`。後者在 Wayland 下只有「焦點停在 X11 視窗」時才收得到按鍵，是這顆器官在 Wayland 上曾經完全不響的原因。

**首次啟動會跳授權對話框**(「mori-ear 想註冊 Ctrl+Alt+E」)，按同意後綁定持久化，之後啟動都是靜默的。

幾個要知道的：

- **改 `hotkey` 不會自動生效**。portal 規範：使用者同意過之後，實際綁定由 compositor 保管。要改鍵去「設定 → 鍵盤 → 檢視及自訂快捷鍵」，或刪掉 `~/.local/share/xdg-desktop-portal/permissions` 讓 mori-ear 重新註冊。啟動時 log 會印 `actual=` 顯示**實際**綁到什麼，按了沒反應先看那行。
- **偵測不到 terminal**。Wayland 不讓 client 查焦點視窗，所以自動 Ctrl+V / Ctrl+Shift+V 切換在這裡失效。terminal 使用者請設 `paste_key`。
- **paste-back 需要 `ydotoold`**。`wl-copy` 寫 clipboard、`ydotool` 透過 `/dev/uinput` 造虛擬鍵盤注入按鍵(所以任何視窗都吃)。daemon 沒跑或使用者不在 `input` 群組就會失敗，失敗時自動退回 XWayland 那條(`xclip`+`xdotool`，只對 X11 視窗有效)。
- **portal 不可用時**(舊桌面環境 / 沒裝 `xdg-desktop-portal-gnome`)自動 fallback 回 X11 熱鍵，log 會 WARN 說明。

範例 pipe(轉錄寫到剪貼簿 + 自己 echo):

```sh
mori-ear | while read -r line; do echo "$line"; echo "$line" | xclip -selection clipboard; done
```

---

## 跟 mori-desktop 的關係

mori-ear 是 Mori 宇宙的「**第一個器官**」— 從 mori-desktop(身體)拆出來的獨立 process。設計守則：

- **獨立 lifecycle**:身體掛 / rebuild 時耳朵還在聽
- **獨立發版**:不同 cargo workspace、不同 repo
- **共用設定**:GROQ key 等讀 `~/.mori/config.json`(跟 mori-desktop 同源)
- **行為對齊**：熱鍵預設 toggle，跟 mori-desktop 的語音按鈕一致。同一雙手在兩個地方按，不該有兩套肌肉記憶
- **未來整合是 enhancement layer,不是 critical path**:IPC 把轉錄推 mori-desktop chat panel、ConfigTab 加 mori-ear sub-tab、啟動時 spawn mori-ear(像 D-1 annuli supervisor)

同樣的拆法之後輪到 `mori-eye`(視覺 / 截圖)、`mori-hand`(輸出 / OS automation)、`mori-lip`(TTS / 講話)。

---

## 不做(故意)

- 沒 GUI / tray icon。**一個刻意的例外**是 hold 模式的懸浮預覽與長句編輯視窗
  (`src/preview.rs`)。它 spawn `yad`，跟 paste-back spawn `xclip` / `xdotool` 同一個模式：
  沒有繪圖程式碼、沒有 toolkit 相依、沒有活過一句話的視窗。缺 `yad` 就整個功能關掉，
  轉錄不受影響。這項功能借助外部工具顯示一段暫時的文字。
- 沒 voice profile / 校正詞庫
- 沒 VAD / noise gate(但**有**送 STT 前的靜音剪裁 + duration/RMS 守門員，見「設定」的 `voice_input`)
- 沒持久化錄音檔(audio 用完丟，只留 transcript)

這些在 mori-desktop 那邊。`mori-ear` 是極簡版，只負責「聽聲音 → 變字 → 貼進焦點視窗」。

---

## License

MIT OR Apache-2.0

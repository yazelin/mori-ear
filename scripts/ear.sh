#!/usr/bin/env bash
# ear — 一鍵控制 mori-ear (Mori 的耳朵 / 語音輸入器官)
#
# 安裝:從 mori-ear repo 根目錄
#   ln -s "$PWD/scripts/ear.sh" ~/.local/bin/ear   # 推薦 symlink
#   ear install                                    # 一鍵全套裝
#
# 用法:
#   ear              # toggle (跑就停,沒跑就開) — 跟 GNOME 快捷鍵綁同支
#   ear on           # 啟動
#   ear off          # 停掉
#   ear toggle       # 同上
#   ear status       # 看在不在跑、binary 時間、各層安裝狀態、paste-back 依賴
#   ear deps         # 只檢查 paste-back 外部依賴(依 X11 / Wayland 分別檢查)
#   ear log          # tail 最近 log
#   ear install      # 一鍵全套裝(依賴檢查 + binary + autostart + GNOME 快捷鍵 + 啟動)
#   ear uninstall    # 一鍵反過來全套拆(會問你確認;--yes 跳過)
#   ear autostart on|off  # 只開/關 開機自動啟用(不動 binary 跟快捷鍵)
#   ear keybind on|off    # 只綁/解 GNOME Ctrl+Shift+Alt+E 快捷鍵
#   ear help         # 印這段
#
# 設計:四層彼此獨立,各層可單獨開關。`install` / `uninstall` 是便利包裝。
# 平台:Linux + GNOME 主測。非 GNOME 桌面 keybind 段會自動 skip(不影響其他層)。
#
# paste-back 依賴依 session 分兩組,`install` / `status` / `deps` 會自動判斷:
#   X11     — xclip + xdotool
#   Wayland — wl-clipboard + ydotool,且 ydotoold 要在跑、使用者要在 input 群組
# 熱鍵來源也跟著 session 走:X11 是 XGrabKey,Wayland 是 GlobalShortcuts portal
# (首次啟動要按同意授權)。

set -u

# Repo 路徑:script 自我定位(支援 symlink 跟直接執行兩種 invoke 方式)。
# 路徑長相 $REPO/scripts/ear.sh,所以 readlink -f 解 symlink 後往上一層就是 repo。
_SCRIPT_PATH=$(readlink -f "$0")
_SCRIPT_DIR=$(dirname "$_SCRIPT_PATH")
_REPO_CANDIDATE=$(cd "$_SCRIPT_DIR/.." && pwd)
if [[ -f "$_REPO_CANDIDATE/Cargo.toml" ]] && grep -q '^name = "mori-ear"' "$_REPO_CANDIDATE/Cargo.toml" 2>/dev/null; then
    REPO="$_REPO_CANDIDATE"
else
    # Fallback:script 被 copy 而非 symlink 過去時用預設位置
    REPO="$HOME/mori-universe/mori-ear"
fi

# binary 位置。三種安裝路徑會放在不同地方,所以要找而不是寫死:
#   - 路徑 B(from source):`cargo install` → ~/.cargo/bin/mori-ear
#   - 路徑 A(prebuilt):README 教的是 /usr/local/bin 或 ~/.local/bin
#   - 自訂:MORI_EAR_BIN 環境變數覆寫
# 寫死 ~/.cargo/bin 的話,prebuilt 使用者的 `ear status` 會回報「NOT INSTALLED」
# 而其實 binary 好好的在 PATH 裡 —— 純粹的假警報。
if [[ -n "${MORI_EAR_BIN:-}" ]]; then
    BIN="$MORI_EAR_BIN"
elif [[ -x "$HOME/.cargo/bin/mori-ear" ]]; then
    BIN="$HOME/.cargo/bin/mori-ear"
elif command -v mori-ear >/dev/null 2>&1; then
    BIN=$(command -v mori-ear)
else
    # 都找不到 → 用 cargo install 的目標位置當「該裝到哪」的預設
    BIN="$HOME/.cargo/bin/mori-ear"
fi

# install-autostart.sh 的位置。tarball 解壓後它跟 ear.sh 同層(平的),
# repo 裡則同在 scripts/ 底下 —— 兩種都是「跟我同一個目錄」,所以先找那裡,
# 找不到才退回 repo 路徑(symlink 進 PATH 的情境)。
if [[ -f "$_SCRIPT_DIR/install-autostart.sh" ]]; then
    AUTOSTART_SCRIPT="$_SCRIPT_DIR/install-autostart.sh"
else
    AUTOSTART_SCRIPT="$REPO/scripts/install-autostart.sh"
fi

LOG_OUT="/tmp/mori-ear.out"
LOG_ERR="/tmp/mori-ear.err"
AUTOSTART_DESKTOP="$HOME/.config/autostart/mori-ear.desktop"

# Wayland portal 產物。mori-ear 走 GlobalShortcuts portal 時會自己寫這個 desktop
# entry(GNOME 要有它才肯放行 app id 註冊),移除時要跟著清 —— 留著會變成選單裡
# 指向已刪 binary 的孤兒。APP_ID 必須跟 src/wayland_hotkey.rs 的常數一致。
PORTAL_APP_ID="ai.yazelin.mori-ear"
PORTAL_DESKTOP="$HOME/.local/share/applications/$PORTAL_APP_ID.desktop"

GS_SCHEMA="org.gnome.settings-daemon.plugins.media-keys"
GS_KEY_PATH="/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/mori-ear-toggle/"
GS_BINDING='<Ctrl><Shift><Alt>e'

notify() {
    if command -v notify-send >/dev/null 2>&1; then
        notify-send -i audio-input-microphone -t 2000 "mori-ear" "$1"
    fi
    echo "$1"
}

is_running() {
    pgrep -x mori-ear >/dev/null
}

cmd_on() {
    if is_running; then
        notify "已經在跑 (PID $(pgrep -x mori-ear | head -1))"
        return 0
    fi
    if [[ ! -x "$BIN" ]]; then
        notify "❌ binary 不在 $BIN — 先 ear install 或 cargo install --path ."
        return 1
    fi
    nohup "$BIN" >"$LOG_OUT" 2>"$LOG_ERR" </dev/null &
    disown
    sleep 1
    if is_running; then
        notify "✓ mori-ear 啟動 (PID $(pgrep -x mori-ear | head -1))"
    else
        notify "❌ 啟動失敗,看 $LOG_ERR:$(tail -1 "$LOG_ERR" 2>/dev/null)"
        return 1
    fi
}

cmd_off() {
    if ! is_running; then
        notify "本來就沒在跑"
        return 0
    fi
    pkill -9 -x mori-ear
    sleep 0.5
    if is_running; then
        notify "❌ 殺不掉,還在跑:$(pgrep -x mori-ear)"
        return 1
    fi
    notify "✓ mori-ear 停掉"
}

cmd_toggle() {
    if is_running; then cmd_off; else cmd_on; fi
}

binary_installed()    { [[ -x "$BIN" ]]; }
autostart_installed() { [[ -f "$AUTOSTART_DESKTOP" ]]; }
keybind_installed() {
    command -v gsettings >/dev/null 2>&1 || return 1
    [[ "$(gsettings get $GS_SCHEMA custom-keybindings 2>/dev/null)" == *"mori-ear-toggle"* ]]
}

is_wayland() {
    [[ "${XDG_SESSION_TYPE,,}" == wayland || -n "${WAYLAND_DISPLAY:-}" ]]
}

# paste-back 的外部依賴檢查。
#
# 為什麼需要:mori-ear 的 binary 裝好、autostart 綁好、熱鍵響了,paste-back 還是
# 可能整條啞掉 —— 它靠的是外部指令(X11 走 xclip+xdotool,Wayland 走
# wl-copy+ydotool),缺了只會在轉錄完那一刻才失敗。裝的時候就講清楚,
# 比讓使用者對著「log 說轉錄成功但字沒出現」瞎猜好。
#
# Wayland 那組特別容易漏:ydotool 光裝套件不夠,還要 ydotoold 這個 daemon 在跑、
# 使用者在 input 群組(才有 /dev/uinput 權限),而且**加群組要重新登入才生效**。
#
# 回傳:0 = 全齊,1 = 有缺(印出修法)。純檢查,不自己 sudo 裝東西。
check_deps() {
    local missing=() hints=() ok=1
    if is_wayland; then
        echo "  session:   Wayland(熱鍵走 GlobalShortcuts portal)"
        command -v wl-copy  >/dev/null 2>&1 || { missing+=("wl-clipboard"); ok=0; }
        command -v ydotool  >/dev/null 2>&1 || { missing+=("ydotool");      ok=0; }
        if command -v ydotool >/dev/null 2>&1; then
            if ! systemctl --user is-active --quiet ydotool 2>/dev/null; then
                hints+=("ydotoold 沒在跑:sudo systemctl --user enable --now ydotool")
                ok=0
            fi
            if ! id -nG | tr ' ' '\n' | grep -qx input; then
                hints+=("你不在 input 群組(ydotool 要 /dev/uinput):sudo usermod -aG input \"\$USER\" —— 加完必須重新登入")
                ok=0
            fi
        fi
        # XWayland fallback 用得到,缺了不致命
        command -v xdotool >/dev/null 2>&1 || \
            hints+=("(選用)xdotool 缺 — Wayland paste-back 失敗時的 XWayland 退路會一起沒有")
    else
        echo "  session:   X11(熱鍵走 XGrabKey)"
        command -v xclip   >/dev/null 2>&1 || { missing+=("xclip");   ok=0; }
        command -v xdotool >/dev/null 2>&1 || { missing+=("xdotool"); ok=0; }
    fi

    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "  依賴:     ✗ 缺 ${missing[*]}"
        echo "             sudo apt install ${missing[*]}"
    elif [[ $ok -eq 1 ]]; then
        echo "  依賴:     ✓ paste-back 依賴齊全"
    else
        echo "  依賴:     ⚠ 套件有裝但設定沒到位"
    fi
    local h
    for h in "${hints[@]}"; do echo "             $h"; done
    [[ $ok -eq 1 ]]
}

cmd_status() {
    if is_running; then
        local pid; pid=$(pgrep -x mori-ear | head -1)
        local since; since=$(ps -o lstart= -p "$pid" 2>/dev/null | xargs)
        echo "✓ running (PID $pid, since $since)"
    else
        echo "✗ stopped"
    fi
    if binary_installed; then
        echo "  binary:    $BIN ($(stat -c '%y' "$BIN" | cut -d. -f1))"
    else
        echo "  binary:    ✗ NOT INSTALLED"
    fi
    if autostart_installed; then
        echo "  autostart: ✓ on  ($AUTOSTART_DESKTOP)"
        # autostart entry 的 Exec= 是安裝當下寫死的路徑。之後換安裝方式
        # (prebuilt 裝 ~/.local/bin,後來又 cargo install 到 ~/.cargo/bin)
        # 兩顆 binary 會並存,而開機時跑的仍是舊那顆 —— 沒有任何錯誤訊息,
        # 只是「改了程式碼、重開機卻沒生效」。這裡主動比對,免得白找半天。
        local exec_path
        exec_path=$(sed -n 's/^Exec=//p' "$AUTOSTART_DESKTOP" 2>/dev/null | head -1)
        if [[ -n "$exec_path" && "$exec_path" != "$BIN" ]]; then
            echo "             ⚠ autostart 指向 $exec_path,跟現在用的 $BIN 不同"
            echo "               開機時會跑到前者。修:ear autostart on(重寫路徑)"
        fi
    else
        echo "  autostart: ✗ off"
    fi
    if keybind_installed; then
        echo "  keybind:   ✓ Ctrl+Shift+Alt+E"
    else
        echo "  keybind:   ✗ not bound"
    fi
    check_deps || true
}

cmd_log() {
    if [[ -f "$LOG_ERR" ]]; then tail -30 "$LOG_ERR"; else echo "(no log yet at $LOG_ERR)"; fi
}

cmd_autostart() {
    local script="$AUTOSTART_SCRIPT"
    if [[ ! -f "$script" ]]; then
        echo "❌ install-autostart.sh 找不到(試過 $_SCRIPT_DIR/ 跟 $REPO/scripts/)"
        return 1
    fi
    case "${1:-}" in
        on)  bash "$script" ;;
        off) bash "$script" --remove ;;
        *)   echo "用法:ear autostart on|off"; return 1 ;;
    esac
}

# gsettings custom-keybindings 是 list of dconf path,要安全 add/remove。
# 用 python3 ast.literal_eval 解析 — gsettings get 輸出格式跟 Python repr 相容。
_modify_keybindings_list() {
    local action="$1" target="$2"
    local current
    current=$(gsettings get $GS_SCHEMA custom-keybindings 2>/dev/null || echo "@as []")
    python3 - "$current" "$action" "$target" <<'PYEOF'
import ast, sys
current_str, action, target = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    items = ast.literal_eval(current_str)
    if not isinstance(items, list):
        items = []
except (ValueError, SyntaxError):
    items = []
if action == "add" and target not in items:
    items.append(target)
elif action == "remove" and target in items:
    items.remove(target)
print(repr(items))
PYEOF
}

cmd_keybind() {
    if ! command -v gsettings >/dev/null 2>&1; then
        echo "(gsettings 沒裝 — 不是 GNOME?跳過)"
        return 0
    fi
    case "${1:-}" in
        on)
            # 快捷鍵 command 偏好 invoke 時的絕對路徑(通常是 ~/.local/bin/ear symlink),
            # 不 resolve 到 repo 內 script — 這樣 user 之後移動 repo,symlink 換指向時
            # 快捷鍵還 work,不用重綁。$0 是相對才 fallback readlink resolve。
            local invoke_path
            if [[ "$0" = /* ]]; then
                invoke_path="$0"
            else
                invoke_path=$(readlink -f "$0")
            fi
            local new_list
            new_list=$(_modify_keybindings_list add "$GS_KEY_PATH") || return 1
            gsettings set $GS_SCHEMA custom-keybindings "$new_list"
            gsettings set ${GS_SCHEMA}.custom-keybinding:$GS_KEY_PATH name 'mori-ear toggle'
            gsettings set ${GS_SCHEMA}.custom-keybinding:$GS_KEY_PATH command "$invoke_path toggle"
            gsettings set ${GS_SCHEMA}.custom-keybinding:$GS_KEY_PATH binding "$GS_BINDING"
            echo "✓ GNOME 快捷鍵 Ctrl+Shift+Alt+E → $invoke_path toggle"
            ;;
        off)
            local new_list
            new_list=$(_modify_keybindings_list remove "$GS_KEY_PATH") || return 1
            gsettings set $GS_SCHEMA custom-keybindings "$new_list"
            gsettings reset-recursively ${GS_SCHEMA}.custom-keybinding:$GS_KEY_PATH 2>/dev/null || true
            dconf reset -f "$GS_KEY_PATH" 2>/dev/null || true
            echo "✓ GNOME 快捷鍵移除"
            ;;
        *)
            echo "用法:ear keybind on|off"; return 1 ;;
    esac
}

cmd_install() {
    echo "→ 安裝 mori-ear(5 層,已裝的會跳過)"
    echo

    # 先檢查再裝:缺依賴不擋安裝(binary / 熱鍵 / stdout 都還是能用),
    # 但要在使用者還盯著畫面時就說清楚,而不是等第一次講完話發現字沒貼進去。
    echo "  [1/5] → 檢查 paste-back 依賴"
    local deps_ok=1
    check_deps || deps_ok=0

    if binary_installed; then
        echo "  [2/5] ✓ binary 已在 $BIN(跳過 cargo install)"
    else
        if [[ ! -d "$REPO" ]]; then
            echo "  [2/5] ❌ 找不到 binary,也沒有 source repo($REPO)"
            echo "        從原始碼裝:git clone https://github.com/yazelin/mori-ear $REPO && ear install"
            echo "        用 prebuilt: 從 https://github.com/yazelin/mori-ear/releases 下載 tar.gz,"
            echo "                     解壓後 install -m 755 mori-ear ~/.local/bin/ 再跑 ./ear.sh install"
            return 1
        fi
        echo "  [2/5] → cargo install --path $REPO (1-2 分鐘)"
        (cd "$REPO" && cargo install --path . --force) || { echo "❌ cargo install 失敗"; return 1; }
    fi

    if autostart_installed; then
        echo "  [3/5] ✓ autostart 已裝(跳過)"
    else
        echo "  [3/5] → 裝開機自動啟動"
        bash "$AUTOSTART_SCRIPT" >/dev/null
        echo "        ✓ $AUTOSTART_DESKTOP"
    fi

    if keybind_installed; then
        echo "  [4/5] ✓ GNOME 快捷鍵已綁(跳過)"
    else
        echo "  [4/5] → 綁 Ctrl+Shift+Alt+E"
        cmd_keybind on >/dev/null
        echo "        ✓ 按 Ctrl+Shift+Alt+E 開/關"
    fi

    if is_running; then
        echo "  [5/5] ✓ process 已在跑(PID $(pgrep -x mori-ear | head -1))"
    else
        echo "  [5/5] → 啟動 mori-ear"
        cmd_on >/dev/null
    fi

    echo
    if [[ $deps_ok -eq 1 ]]; then
        echo "✓ 完整安裝完成"
    else
        echo "⚠ 安裝完成,但 paste-back 依賴沒齊 —— 轉錄會成功、字貼不進焦點視窗"
        echo '  (stdout 仍照印,ear log 看得到轉錄結果)'
        echo '  補完上面那幾條後跑 ear deps 重新確認'
    fi
    echo "  按住 Ctrl+Alt+E 講話、放開貼字"
    echo "  按 Ctrl+Shift+Alt+E toggle on/off"
    echo "  ear status — 看狀態"
    if is_wayland; then
        echo
        echo "  Wayland 提醒:第一次啟動會跳授權對話框(「mori-ear 想註冊 Ctrl+Alt+E」),"
        echo "  要按同意熱鍵才會生效。同意後綁定由 compositor 保管 —— 之後改 ear.json"
        echo "  的 hotkey 不會生效,要去「設定 → 鍵盤 → 檢視及自訂快捷鍵」改。"
        echo '  ear log 開頭那行 actual= 會顯示實際綁到哪組鍵。'
    fi
}

cmd_uninstall() {
    echo "這會移除:"
    [[ -n "$(pgrep -x mori-ear)" ]] && echo "  • 跑中的 mori-ear process"
    keybind_installed     && echo "  • GNOME 快捷鍵 Ctrl+Shift+Alt+E"
    autostart_installed   && echo "  • 開機自動啟動 $AUTOSTART_DESKTOP"
    binary_installed      && echo "  • binary $BIN"
    [[ -f "$PORTAL_DESKTOP" ]] && echo "  • Wayland portal desktop entry $PORTAL_DESKTOP"
    echo "  • ear wrapper symlink / copy:$(readlink -f "$0")"
    echo
    echo "不會動(共用 / 你的資料):"
    echo "  • ~/.mori/config.json(跟 mori-desktop 共用)"
    echo "  • ~/.mori/ear.json(你的設定,要拆自己 rm)"
    echo "  • source repo $REPO"
    echo

    if [[ "${1:-}" != "--yes" && "${1:-}" != "-y" ]]; then
        read -r -p "確定?(y/N) " confirm
        [[ "$confirm" == "y" || "$confirm" == "Y" ]] || { echo "abort"; return 1; }
    fi

    if is_running; then
        pkill -9 -x mori-ear 2>/dev/null || true
        sleep 0.5
        echo "✓ process stopped"
    fi

    if keybind_installed; then
        cmd_keybind off >/dev/null
        echo "✓ GNOME 快捷鍵移除"
    fi

    if autostart_installed; then
        rm -f "$AUTOSTART_DESKTOP"
        echo "✓ autostart entry 移除"
    fi

    if binary_installed; then
        if rm -f "$BIN" 2>/dev/null; then
            echo "✓ binary 移除"
        else
            echo "⚠ binary 刪不掉(權限?):$BIN"
            echo "  手動:sudo rm -f $BIN"
        fi
    fi

    if [[ -f "$PORTAL_DESKTOP" ]]; then
        rm -f "$PORTAL_DESKTOP"
        command -v update-desktop-database >/dev/null 2>&1 && \
            update-desktop-database "$(dirname "$PORTAL_DESKTOP")" 2>/dev/null || true
        echo "✓ Wayland portal desktop entry 移除"
    fi

    # 自我刪除:Linux unlink 不影響跑中的 process,kernel 開著 fd 跑完 script 沒問題。
    #
    # 判斷必須用 `-L`(真的是 symlink 嗎),不能用 `$0 != $(readlink -f $0)` ——
    # 後者在 $0 只是**相對路徑**時也成立,於是 `bash scripts/ear.sh uninstall`
    # 會走進「刪 symlink」分支、把 repo 裡的本體刪掉,還印訊息說 repo 內保留。
    # (這個 bug 真的發生過,靠 git checkout 才救回來。)
    local self_resolved; self_resolved=$(readlink -f "$0")
    if [[ -L "$0" ]]; then
        rm -f "$0"
        echo "✓ ear symlink 移除:$0(本體 $self_resolved 保留)"
    elif [[ -n "$REPO" && "$self_resolved" == "$REPO"/* ]]; then
        # 從 repo 內直接執行:那是版控中的原始檔,絕對不能刪
        echo "○ ear wrapper 是 repo 內原始檔,保留:$self_resolved"
    else
        rm -f "$self_resolved"
        echo "✓ ear wrapper 移除:$self_resolved"
    fi

    echo
    echo "✓ mori-ear 完整移除"
    echo "  徹底乾淨還要:rm -f ~/.mori/ear.json; rm -rf $REPO"
    if is_wayland; then
        echo
        echo "  注意:portal 的熱鍵授權由 compositor 保管,上面刪不掉 ——"
        echo "  留著的話重裝時不會再跳授權對話框(直接沿用舊綁定)。要真正回到"
        echo "  「沒裝過」的狀態,再刪授權紀錄:"
        echo "    rm -rf ~/.local/share/xdg-desktop-portal/permissions*"
        echo "  (那份是所有 app 的 portal 授權,刪掉別的 app 也要重新授權一次)"
    fi
}

cmd_help() {
    awk '/^#!/ {next} /^#/ {sub(/^# ?/, ""); print; next} {exit}' "$0"
}

case "${1:-toggle}" in
    on|start)         cmd_on ;;
    off|stop)         cmd_off ;;
    toggle|"")        cmd_toggle ;;
    status|st)        cmd_status ;;
    deps|dep)         check_deps ;;
    log|logs)         cmd_log ;;
    install)          cmd_install ;;
    uninstall|remove) cmd_uninstall "${2:-}" ;;
    autostart)        cmd_autostart "${2:-}" ;;
    keybind|key)      cmd_keybind "${2:-}" ;;
    help|-h|--help)   cmd_help ;;
    *)                echo "未知指令:$1"; cmd_help; exit 1 ;;
esac

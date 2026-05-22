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
#   ear status       # 看在不在跑、binary 時間、各層安裝狀態
#   ear log          # tail 最近 log
#   ear install      # 一鍵全套裝(binary + autostart + GNOME 快捷鍵 + 啟動)
#   ear uninstall    # 一鍵反過來全套拆(會問你確認;--yes 跳過)
#   ear autostart on|off  # 只開/關 開機自動啟用(不動 binary 跟快捷鍵)
#   ear keybind on|off    # 只綁/解 GNOME Ctrl+Shift+Alt+E 快捷鍵
#   ear help         # 印這段
#
# 設計:四層彼此獨立,各層可單獨開關。`install` / `uninstall` 是便利包裝。
# 平台:Linux + GNOME 主測。非 GNOME 桌面 keybind 段會自動 skip(不影響其他層)。

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

BIN="$HOME/.cargo/bin/mori-ear"
LOG_OUT="/tmp/mori-ear.out"
LOG_ERR="/tmp/mori-ear.err"
AUTOSTART_DESKTOP="$HOME/.config/autostart/mori-ear.desktop"

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
    else
        echo "  autostart: ✗ off"
    fi
    if keybind_installed; then
        echo "  keybind:   ✓ Ctrl+Shift+Alt+E"
    else
        echo "  keybind:   ✗ not bound"
    fi
}

cmd_log() {
    if [[ -f "$LOG_ERR" ]]; then tail -30 "$LOG_ERR"; else echo "(no log yet at $LOG_ERR)"; fi
}

cmd_autostart() {
    local script="$REPO/scripts/install-autostart.sh"
    if [[ ! -f "$script" ]]; then
        echo "❌ install-autostart.sh 不在 $script — source repo 不存在?"
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
    echo "→ 安裝 mori-ear(4 層,已裝的會跳過)"
    echo

    if binary_installed; then
        echo "  [1/4] ✓ binary 已在 $BIN(跳過 cargo install)"
    else
        if [[ ! -d "$REPO" ]]; then
            echo "  [1/4] ❌ source repo 不在 $REPO"
            echo "        先 clone:git clone https://github.com/yazelin/mori-ear $REPO"
            return 1
        fi
        echo "  [1/4] → cargo install --path $REPO (1-2 分鐘)"
        (cd "$REPO" && cargo install --path . --force) || { echo "❌ cargo install 失敗"; return 1; }
    fi

    if autostart_installed; then
        echo "  [2/4] ✓ autostart 已裝(跳過)"
    else
        echo "  [2/4] → 裝開機自動啟動"
        bash "$REPO/scripts/install-autostart.sh" >/dev/null
        echo "        ✓ $AUTOSTART_DESKTOP"
    fi

    if keybind_installed; then
        echo "  [3/4] ✓ GNOME 快捷鍵已綁(跳過)"
    else
        echo "  [3/4] → 綁 Ctrl+Shift+Alt+E"
        cmd_keybind on >/dev/null
        echo "        ✓ 按 Ctrl+Shift+Alt+E 開/關"
    fi

    if is_running; then
        echo "  [4/4] ✓ process 已在跑(PID $(pgrep -x mori-ear | head -1))"
    else
        echo "  [4/4] → 啟動 mori-ear"
        cmd_on >/dev/null
    fi

    echo
    echo "✓ 完整安裝完成"
    echo "  按住 Ctrl+Alt+E 講話、放開貼字"
    echo "  按 Ctrl+Shift+Alt+E toggle on/off"
    echo "  ear status — 看狀態"
}

cmd_uninstall() {
    echo "這會移除:"
    [[ -n "$(pgrep -x mori-ear)" ]] && echo "  • 跑中的 mori-ear process"
    keybind_installed     && echo "  • GNOME 快捷鍵 Ctrl+Shift+Alt+E"
    autostart_installed   && echo "  • 開機自動啟動 $AUTOSTART_DESKTOP"
    binary_installed      && echo "  • binary $BIN"
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
        rm -f "$BIN"
        echo "✓ binary 移除"
    fi

    # 自我刪除:Linux unlink 不影響跑中的 process,kernel 開著 fd 跑完 script 沒問題。
    # 注意 readlink -f 解到實體位置;如果是 symlink,實際刪到 repo 的 scripts/ear.sh —
    # 那不是 user 想要的,要改刪 symlink 本身($0,未 resolve)。
    local self_resolved; self_resolved=$(readlink -f "$0")
    if [[ "$0" != "$self_resolved" ]]; then
        # symlinked invoke:刪 symlink 本身,不刪 repo 內的本體
        rm -f "$0"
        echo "✓ ear symlink 移除:$0(repo 內 $self_resolved 保留)"
    else
        rm -f "$self_resolved"
        echo "✓ ear wrapper 移除:$self_resolved"
    fi

    echo
    echo "✓ mori-ear 完整移除"
    echo "  徹底乾淨還要:rm -f ~/.mori/ear.json; rm -rf $REPO"
}

cmd_help() {
    awk '/^#!/ {next} /^#/ {sub(/^# ?/, ""); print; next} {exit}' "$0"
}

case "${1:-toggle}" in
    on|start)         cmd_on ;;
    off|stop)         cmd_off ;;
    toggle|"")        cmd_toggle ;;
    status|st)        cmd_status ;;
    log|logs)         cmd_log ;;
    install)          cmd_install ;;
    uninstall|remove) cmd_uninstall "${2:-}" ;;
    autostart)        cmd_autostart "${2:-}" ;;
    keybind|key)      cmd_keybind "${2:-}" ;;
    help|-h|--help)   cmd_help ;;
    *)                echo "未知指令:$1"; cmd_help; exit 1 ;;
esac

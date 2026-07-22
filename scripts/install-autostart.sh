#!/usr/bin/env bash
# 在 Linux 登入時自動啟 mori-ear 的 XDG autostart entry。
#
# 跟 mori-desktop 的 autostart entry 共存 — 兩個器官各自獨立 lifecycle。
#
# 用法:
#   bash scripts/install-autostart.sh           # 安裝
#   bash scripts/install-autostart.sh --remove  # 移除

set -euo pipefail

AUTOSTART_DIR="$HOME/.config/autostart"
DESKTOP_FILE="$AUTOSTART_DIR/mori-ear.desktop"

# binary 位置要用找的,不能寫死 —— 這支腳本有三種被呼叫的情境:
#   repo 內(cargo install → ~/.cargo/bin)、release tarball 解壓後
#   (binary 可能在 ~/.local/bin 或 /usr/local/bin)、或使用者自訂位置。
# autostart entry 的 Exec= 指錯路徑,症狀是「登入後沒啟動」而且完全沒有錯誤訊息。
if [[ -n "${MORI_EAR_BIN:-}" ]]; then
    BINARY="$MORI_EAR_BIN"
elif [[ -x "$HOME/.cargo/bin/mori-ear" ]]; then
    BINARY="$HOME/.cargo/bin/mori-ear"
elif command -v mori-ear >/dev/null 2>&1; then
    BINARY=$(command -v mori-ear)
else
    BINARY="$HOME/.cargo/bin/mori-ear"
fi

if [[ "${1:-}" == "--remove" ]]; then
    if [[ -f "$DESKTOP_FILE" ]]; then
        rm "$DESKTOP_FILE"
        echo "✓ removed $DESKTOP_FILE"
    else
        echo "(沒安裝過,nothing to do)"
    fi
    exit 0
fi

if [[ ! -x "$BINARY" ]]; then
    echo "❌ 找不到 mori-ear binary(找過 MORI_EAR_BIN / ~/.cargo/bin / PATH)"
    echo "   從原始碼:cargo install --path ."
    echo "   用 prebuilt:install -m 755 mori-ear ~/.local/bin/"
    echo "   或指定位置:MORI_EAR_BIN=/path/to/mori-ear bash $0"
    exit 1
fi

mkdir -p "$AUTOSTART_DIR"

cat > "$DESKTOP_FILE" <<EOF
[Desktop Entry]
Type=Application
Name=mori-ear
Comment=Mori 的耳朵 — 全域熱鍵語音輸入(autostart at login)
Exec=$BINARY
Icon=audio-input-microphone
StartupNotify=false
Terminal=false
Categories=Utility;
X-GNOME-Autostart-enabled=true
X-GNOME-Autostart-Delay=5
EOF

echo "✓ installed $DESKTOP_FILE"
echo "  Exec=$BINARY"
echo
echo "下次登入 mori-ear 會自動啟動。立即啟動:ear on(或直接跑 $BINARY &)"
echo
echo "移除:bash scripts/install-autostart.sh --remove"

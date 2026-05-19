#!/usr/bin/env bash
# 在 Linux 登入時自動啟 mori-ear 的 XDG autostart entry。
#
# 跟 mori-desktop 的 autostart entry 共存 — 兩個器官各自獨立 lifecycle。
#
# 用法:
#   bash scripts/install-autostart.sh           # 安裝
#   bash scripts/install-autostart.sh --remove  # 移除

set -euo pipefail
cd "$(dirname "$0")/.."

AUTOSTART_DIR="$HOME/.config/autostart"
DESKTOP_FILE="$AUTOSTART_DIR/mori-ear.desktop"
BINARY="$HOME/.cargo/bin/mori-ear"

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
    echo "❌ mori-ear binary 不在 $BINARY"
    echo "   先跑 \`cargo install --path .\` 把 mori-ear 裝進 ~/.cargo/bin/"
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
echo "下次登入 mori-ear 會自動啟動。立即測試:bash scripts/restart.sh --release"
echo
echo "移除:bash scripts/install-autostart.sh --remove"

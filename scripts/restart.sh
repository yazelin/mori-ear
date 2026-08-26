#!/usr/bin/env bash
# 重新編譯 + 替換 ~/.cargo/bin/mori-ear + kill 舊 + 背景跑新版。
#
# 用法:
#   bash scripts/restart.sh           # cargo build (debug) + 跑 debug binary
#   bash scripts/restart.sh --release # cargo install --path .(release)+ 跑 ~/.cargo/bin/mori-ear
#   bash scripts/restart.sh --stop    # 只 kill,不重啟
#
# 流程:
#   1. compile(失敗就 abort,不殺 running instance)
#   2. pkill mori-ear,等 single-instance lock 釋放
#   3. nohup ... & disown 起新版,log 寫到 /tmp/mori-ear.{out,err}

set -euo pipefail
cd "$(dirname "$0")/.."

MODE="${1:-debug}"

# mori-ear 被 SIGKILL 時沒機會跑 Drop,它開的 yad 預覽視窗會活下來,
# 還繼承著 single-instance 的 abstract socket → 下一個 mori-ear 起不來。
kill_stray_yad() {
    pkill -9 -f 'yad .*--title=mori-ear' 2>/dev/null || true
}

if [[ "$MODE" == "--stop" ]]; then
    pkill mori-ear 2>/dev/null || true
    kill_stray_yad
    sleep 1
    if pgrep -x mori-ear > /dev/null; then
        echo "❌ 殺不掉 mori-ear,還在跑:$(pgrep -x mori-ear)"
        exit 1
    fi
    echo "✓ mori-ear stopped"
    exit 0
fi

if [[ "$MODE" == "--release" ]]; then
    echo "→ cargo install --path . (release)"
    cargo install --path . --offline 2>&1 | tail -3
    BINARY="$HOME/.cargo/bin/mori-ear"
else
    echo "→ cargo build (debug)"
    cargo build 2>&1 | tail -3
    BINARY="$PWD/target/debug/mori-ear"
fi

echo "→ pkill -9 mori-ear(force kill 確保 single-instance abstract socket 立即釋放)"
pkill -9 -x mori-ear 2>/dev/null || true
kill_stray_yad   # SIGKILL 過的 mori-ear 沒機會收掉自己開的預覽視窗
# poll 直到 pgrep 真的空,最多 5 秒
for i in 1 2 3 4 5; do
    if ! pgrep -x mori-ear > /dev/null; then
        break
    fi
    sleep 1
done
if pgrep -x mori-ear > /dev/null; then
    echo "❌ 舊 instance 5 秒內殺不掉,abort"
    exit 1
fi
# 再多等 0.5 秒讓 kernel 釋放 abstract socket / X11 GrabKey
sleep 0.5

echo "→ launching $BINARY"
nohup "$BINARY" > /tmp/mori-ear.out 2> /tmp/mori-ear.err < /dev/null &
disown
sleep 1

PID=$(pgrep -x mori-ear | head -1)
if [[ -z "$PID" ]]; then
    echo "❌ 新 instance 沒起來,看 /tmp/mori-ear.err:"
    tail -5 /tmp/mori-ear.err
    exit 1
fi

echo "✓ mori-ear running (PID $PID)"
echo "  stdout → /tmp/mori-ear.out"
echo "  stderr → /tmp/mori-ear.err  (tail -f 看 cleanup log)"
echo
head -3 /tmp/mori-ear.err

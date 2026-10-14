#!/usr/bin/env bash
# 验收 production-identity 二进制或 AppImage，所有操作均限制在独立桌面中。
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
binary=${1:-target/x86_64-unknown-linux-gnu/release/KwikPaste}
output=${2:-target/linux-acceptance}
export XDG_SESSION_TYPE=x11
export KWIKPASTE_X11_TEST=1
unset WAYLAND_DISPLAY
# 测试会话使用独立 runtime 目录，不触及当前桌面的 D-Bus/Keyring socket。
runtime=$(mktemp -d /tmp/kwikpaste-x11-runtime.XXXXXX)
trap 'rm -rf -- "$runtime"' EXIT
export XDG_RUNTIME_DIR=$runtime
xvfb-run -a -s '-screen 0 1440x1000x24' dbus-run-session -- \
  python3 scripts/linux/test-app.py "$binary" "$output"

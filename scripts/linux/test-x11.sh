#!/usr/bin/env bash
# 使用独立 Xvfb/D-Bus 会话验收，不读取或修改当前桌面的剪贴板。
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
export XDG_SESSION_TYPE=x11
unset WAYLAND_DISPLAY
xvfb-run -a dbus-run-session -- cargo test --locked -p kwikpaste-os --test linux_x11 -- --ignored

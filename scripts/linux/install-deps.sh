#!/usr/bin/env bash
# 安装 Ubuntu/Debian 上原生 GPUI 构建、AppImage 打包及 X11 自测所需依赖。
set -euo pipefail
if [[ $(id -u) -ne 0 ]]; then
  echo "请运行 sudo bash scripts/linux/install-deps.sh" >&2
  exit 1
fi
command -v apt-get >/dev/null || { echo "此脚本仅用于 Debian/Ubuntu。" >&2; exit 1; }
apt-get update
apt-get install -y build-essential pkg-config patchelf file curl \
  libxkbcommon-dev libxkbcommon-x11-dev libfontconfig1-dev libssl-dev \
  libgtk-3-dev libglib2.0-dev libayatana-appindicator3-dev libfuse2 \
  libwayland-dev libasound2-dev xvfb xdotool xclip
pkg-config --modversion glib-2.0 gtk+-3.0 xkbcommon fontconfig openssl

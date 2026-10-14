#!/usr/bin/env bash
# 构建原生 2.0 AppImage；系统依赖见 install-deps.sh，不编译 1.x WebView 应用。
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
if [[ $(uname -m) != x86_64 ]]; then
  echo "当前本地 AppImage 构建入口仅验证了 x86_64。" >&2
  exit 1
fi
pkg-config --exists gtk+-3.0 xkbcommon xkbcommon-x11 fontconfig openssl
command -v patchelf >/dev/null
cargo build --locked --release -p kwikpaste-app --target x86_64-unknown-linux-gnu --features production-identity
APPIMAGE_EXTRACT_AND_RUN=1 node scripts/release/package-native.mjs \
  --target x86_64-unknown-linux-gnu --out release-local/linux --features production-identity --no-sign

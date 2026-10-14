# Linux 本地构建（原生 2.0）

Linux 支持针对根 workspace 的 GPUI 应用（`crates/`）。正式发布流水线仍只发布 macOS/Windows；Linux AppImage 从源码在本地构建，不使用官方自动更新清单。

当前构建入口面向 x86_64，构建基线为 Ubuntu 22.04。GPUI 使用 wgpu，运行需要可用的 Vulkan 驱动和桌面字体。其他发行版与 CPU 架构需要另行验证。

## 构建

```bash
sudo bash scripts/linux/install-deps.sh
pnpm install --frozen-lockfile
bash scripts/linux/build-appimage.sh
```

Rust 版本取自 `rust-toolchain.toml`，Tauri CLI 使用项目锁定版本，仅用于打包。构建脚本编译 `kwikpaste-app` 并启用 `production-identity`，不编译 1.x 的 WebView 应用。产物位于 `release-local/linux/KwikPaste_<版本>_amd64.AppImage`。

```bash
chmod +x release-local/linux/*.AppImage
./release-local/linux/KwikPaste_2.0.3_amd64.AppImage
```

若系统不允许 FUSE 挂载，可使用 AppImage 的解包运行模式：

```bash
APPIMAGE_EXTRACT_AND_RUN=1 ./release-local/linux/KwikPaste_2.0.3_amd64.AppImage
```

默认数据保存在 `${XDG_DATA_HOME:-$HOME/.local/share}/com.fastthree.kwikpaste/prod`。自启动入口记录原始 AppImage 路径，移动镜像后需要重新开关自启动。开发构建使用独立的 `native-dev` 身份与数据目录。

## 平台能力

| 能力 | X11 | Wayland 会话 |
| --- | --- | --- |
| 面板、搜索、预览、偏好设置 | 原生 X11 窗口 | 需要 XWayland；没有 `DISPLAY` 时明确报错 |
| 文本、HTML、PNG、文件剪贴板 | clipboard-rs 的 X11 后端 | 取决于合成器的 XWayland 剪贴板桥接 |
| 全局快捷键、自动粘贴 | global-hotkey / XTest | 禁用，使用托盘打开面板，复制后手动粘贴 |
| 来源应用与窗口定位 | EWMH / RandR | 只保证 XWayland 范围，原生 Wayland 窗口信息不可用 |
| 托盘 | AppIndicator；桌面需支持托盘扩展 | 同左 |
| 自启动 | XDG Desktop Entry | 同左 |

Linux 暂不提供原生拖出、全局鼠标按键唤起、按全屏/应用暂停触发、Mica/Acrylic、原生 OCR、系统应用/文件图标提取与 GPU 设备丢失注入自测。对应代码有明确降级或错误返回，文件卡片使用内置图标。OCR helper 的协议探测返回 `Unsupported`。Linux 不下载 Windows 扩展包，不自动替换 AppImage。

历史库以文本标签 `linux` 记录来源平台，保留已有 `macos`/`windows` 标签，不修改已发布 SQL migration。跨端同步需要双方版本都能识别 `linux` 标签；尚未包含此次移植的旧版 macOS/Windows 客户端不具备这一兼容性。

## 验证

```bash
cargo fmt --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build --locked -p kwikpaste-ext-ocr
cargo test --locked --workspace
bash scripts/linux/test-x11.sh
```

X11 集成测试在单独的 Xvfb/D-Bus 会话中创建目标窗口和面板，验证映射、几何、输入焦点、Ctrl+V 与修饰键释放，不接触当前桌面的剪贴板。

运行应用自测时必须同时设置 `KWIKPASTE_SELFTEST=1` 与 `--selftest-smoke`。Linux 冒烟测试在面板未显示或未出帧时返回非零状态。

完整交互验收（需要 `python3-tk`、ImageMagick、`xclip`、`xdotool`）可对二进制或 AppImage 执行：

```bash
bash scripts/linux/test-app.sh target/x86_64-unknown-linux-gnu/release/KwikPaste
bash scripts/linux/test-app.sh release-local/linux/KwikPaste_2.0.3_amd64.AppImage
```

此脚本在隔离桌面中验证真实热键、文本/PNG/文件捕获、原窗口粘贴、失焦隐藏、单实例与偏好窗，并将 JSON 结果、应用日志和面板截图保存到 `target/linux-acceptance/run-*`。

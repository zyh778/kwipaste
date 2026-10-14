//! Linux 平台集成；X11 提供窗口与输入能力，Wayland 会话经 XWayland 降级运行。
pub mod apps;
pub mod keystroke;
pub mod monitor;
pub mod single_instance;
pub mod trigger_pause;
pub mod x11;

/// Wayland 会话不能通过 XTest 向原生 Wayland 客户端注入全局粘贴。
pub fn is_wayland() -> bool {
    std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value == "wayland")
        || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty())
        || WAYLAND_SESSION.load(std::sync::atomic::Ordering::Relaxed)
}

static WAYLAND_SESSION: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 在启动任何线程之前选择 XWayland；原生 Wayland 窗口尚无对应面板定位协议。
///
/// # Safety
/// 调用时进程必须仍为单线程，因为这里会更新 GPUI 读取的环境变量。
pub unsafe fn prepare_display() {
    WAYLAND_SESSION.store(is_wayland(), std::sync::atomic::Ordering::Relaxed);
    if std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty()) {
        // SAFETY: 入口在日志、单实例监听及 GPU 工作线程之前调用。
        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
        }
    }
}

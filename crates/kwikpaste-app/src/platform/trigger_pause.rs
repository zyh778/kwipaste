//! 前台应用让出全局触发（设置 `shortcuts.pauseInFullscreen` / `shortcuts.pauseAppIds`）：前台是全屏应用
//! 或列表里的应用时，原生侧（`kwikpaste_os::{win,mac}::trigger_pause`）让鼠标唤起和 Win+V 钩子放行，
//! 这里跟着注销全局热键，前台换走后按最新设置重新注册。

use gpui::{App, AsyncApp};
use kwikpaste_core::settings::Shortcuts;

#[cfg(target_os = "linux")]
use kwikpaste_os::linux::trigger_pause as native;
#[cfg(target_os = "macos")]
use kwikpaste_os::mac::trigger_pause as native;
#[cfg(target_os = "windows")]
use kwikpaste_os::win::trigger_pause as native;

use super::hotkey;

/// 接上原生暂停状态的通知并按当前设置起步。通知可能来自钩子线程，只转发不阻塞；
/// 主线程收到后读当时的状态，先后到达的通知不会把状态排错。
pub fn serve(cx: &mut App) {
    let (sender, receiver) = async_channel::unbounded::<()>();
    if let Err(err) = native::set_sink(move || {
        let _ = sender.try_send(());
    }) {
        log::error!("trigger pause notifications are unavailable: {err}");
    }
    cx.spawn(async move |cx: &mut AsyncApp| {
        while receiver.recv().await.is_ok() {
            cx.update(|cx| hotkey::set_paused(native::is_paused(), cx));
        }
    })
    .detach();

    if let Some(core) = crate::core_host::core(cx) {
        apply(&core.settings().shortcuts);
    }
}

/// 跟随设置起停前台监听。
pub fn apply(shortcuts: &Shortcuts) {
    if let Err(err) = native::configure(
        shortcuts.pause_in_fullscreen,
        shortcuts.pause_app_ids.clone(),
    ) {
        log::error!("trigger pause could not be configured: {err}");
    }
}

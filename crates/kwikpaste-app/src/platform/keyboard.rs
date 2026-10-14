//! 钩子按键 → GPUI：Windows 面板非编辑态时，键盘钩子截下的键以普通按键派发给面板窗口。
//!
//! - 按下、自动重复：`window.dispatch_keystroke`，按 UI 的 key binding 匹配 action；
//! - 空格松开：`KeyUp`（按住预览、松开关闭）；
//! - Ctrl 按下、松开：`ModifiersChanged`（快捷键提示）。
//!
//! 钩子线程只把事件送进 channel，派发在主线程的任务里进行。macOS 的面板成为 key 窗口后
//! 直接收键盘，不需要钩子。

use gpui::App;

/// 接上键盘钩子的出口（Windows）。在面板创建之后调用一次。
#[cfg(target_os = "windows")]
pub fn serve(cx: &mut App) {
    use kwikpaste_os::win::keyboard;

    use super::panel::Panel;

    let (sender, receiver) = async_channel::unbounded();
    if let Err(err) = keyboard::set_sink(move |event| {
        let _ = sender.try_send(event);
    }) {
        log::error!("keyboard hook events cannot be delivered: {err}");
        return;
    }

    let Some(window) = cx.global::<Panel>().window() else {
        log::error!("keyboard hook events have no panel window to go to");
        return;
    };
    cx.spawn(async move |cx| {
        while let Ok(event) = receiver.recv().await {
            cx.update(|cx| dispatch(window, event, cx));
        }
    })
    .detach();
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn serve(_cx: &mut App) {}

#[cfg(target_os = "windows")]
fn dispatch(
    window: gpui::AnyWindowHandle,
    event: kwikpaste_os::win::keyboard::HookEvent,
    cx: &mut App,
) {
    use gpui::{Capslock, KeyUpEvent, Keystroke, Modifiers, ModifiersChangedEvent, PlatformInput};
    use kwikpaste_os::win::keyboard::{HookEvent, KeyPhase};

    use super::probe;

    let dispatched = window.update(cx, |_, window, cx| match event {
        HookEvent::Key {
            key,
            phase,
            ctrl,
            shift,
        } => {
            let keystroke = Keystroke {
                modifiers: Modifiers {
                    control: ctrl,
                    shift,
                    ..Modifiers::default()
                },
                key: key.to_owned(),
                key_char: None,
            };
            match phase {
                KeyPhase::Down | KeyPhase::Repeat => window.dispatch_keystroke(keystroke, cx),
                KeyPhase::Up => {
                    let result =
                        window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
                    !result.propagate
                }
            }
        }
        HookEvent::Control { down } => {
            window.dispatch_event(
                PlatformInput::ModifiersChanged(ModifiersChangedEvent {
                    modifiers: Modifiers {
                        control: down,
                        ..Modifiers::default()
                    },
                    capslock: Capslock::default(),
                }),
                cx,
            );
            false
        }
    });

    match dispatched {
        Ok(handled) => probe::hook_key(&event, handled),
        Err(err) => log::warn!("hook key could not reach the panel: {err:#}"),
    }
}

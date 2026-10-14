//! 拖动窗口的空白区域。

use gpui::{InteractiveElement, WindowControlArea};

/// 把元素标成拖动窗口的区域。Windows 上 GPUI 按 `WindowControlArea::Drag` 做命中测试（当标题栏）；
/// macOS 的 GPUI 不做这项命中测试，按下时自己交给 AppKit 拖动窗口；
/// Linux 的面板是 override-redirect 窗口、窗口管理器不管它，按下时自己开轮询手势移动。
///
/// 只用在不含可点控件的空白处：macOS/Linux 上按下就开始拖，子元素收不到这次点击。
pub trait WindowDragArea: InteractiveElement {
    fn window_drag_area(self) -> Self {
        let element = self.window_control_area(WindowControlArea::Drag);
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let element = element.on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
            #[cfg(target_os = "macos")]
            window.start_window_move();
            #[cfg(target_os = "linux")]
            if let Some(xid) = super::native::window_xid(window) {
                kwikpaste_os::linux::x11::begin_drag(xid);
            }
        });
        element
    }
}

impl<E: InteractiveElement> WindowDragArea for E {}

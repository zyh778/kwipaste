//! 在隔离 Xvfb 中运行的真实 X11 验收，默认单元测试不触碰用户桌面。
#![cfg(target_os = "linux")]

use kwikpaste_os::{
    geometry::Rect,
    linux::{keystroke, x11},
};
use std::time::{Duration, Instant};
use x11rb::{
    COPY_DEPTH_FROM_PARENT,
    connection::Connection,
    protocol::{
        Event,
        xproto::{ConnectionExt, CreateWindowAux, EventMask, KeyButMask, WindowClass},
    },
};

/// 创建接收键盘事件的测试窗口；Drop 始终清理，避免测试失败后留下窗口。
struct TestWindow(u32);
impl Drop for TestWindow {
    fn drop(&mut self) {
        if let Ok(display) = x11::connection() {
            let _ = display.conn.destroy_window(self.0);
            let _ = display.conn.flush();
        }
    }
}

/// 真正验证 X11 映射、几何、焦点恢复和 Ctrl+V，不把编译成功当成功能可用。
#[test]
#[ignore = "requires an isolated X11 display (run under xvfb-run)"]
fn window_and_paste_roundtrip() {
    assert!(!kwikpaste_os::linux::is_wayland());
    let display = x11::connection().unwrap();
    let target = TestWindow(display.conn.generate_id().unwrap());
    let panel = TestWindow(display.conn.generate_id().unwrap());
    for window in [&target, &panel] {
        display
            .conn
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                window.0,
                display.root,
                10,
                20,
                300,
                200,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new()
                    .override_redirect(1)
                    .event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
            )
            .unwrap()
            .check()
            .unwrap();
    }
    let rect = Rect {
        left: 40,
        top: 50,
        right: 400,
        bottom: 650,
    };
    x11::place(panel.0, rect).unwrap();
    assert_eq!(x11::geometry(panel.0).unwrap(), rect);
    x11::show(target.0, true).unwrap();
    assert_eq!(x11::focused_window(), Some(target.0));
    x11::show(panel.0, true).unwrap();
    assert!(x11::is_visible(panel.0));
    assert_eq!(x11::focused_window(), Some(panel.0));
    x11::focus(target.0).unwrap();
    x11::hide(panel.0).unwrap();
    assert!(!x11::is_visible(panel.0));
    keystroke::simulate_paste().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut pasted = false;
    while Instant::now() < deadline {
        if let Some(Event::KeyPress(event)) = display.conn.poll_for_event().unwrap()
            && event.event == target.0
            && u16::from(event.state) & u16::from(KeyButMask::CONTROL) != 0
        {
            pasted = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(pasted, "Ctrl+V did not reach the previous target window");
    assert!(
        !keystroke::modifiers_pressed(),
        "synthetic modifier remained pressed"
    );
}

/// 连续写回不应为每次操作遗留两个 X11 服务窗口和一个常驻线程。
#[test]
#[ignore = "requires an isolated X11 display (run under xvfb-run)"]
fn repeated_clipboard_access_keeps_selection_and_bounded_connections() {
    use kwikpaste_core::clipboard::{ClipboardBackend, SystemClipboard};
    let display = x11::connection().unwrap();
    let count = || {
        display
            .conn
            .query_tree(display.root)
            .unwrap()
            .reply()
            .unwrap()
            .children
            .len()
    };
    let before = count();
    for index in 0..30 {
        let clipboard = SystemClipboard::new().unwrap();
        clipboard
            .set_text(format!("Linux clipboard {index}"))
            .unwrap();
    }
    assert_eq!(
        SystemClipboard::new().unwrap().get_text().unwrap(),
        "Linux clipboard 29"
    );
    // 允许同一测试程序并发运行窗口测试；无缓存时这里会增长约 60 个窗口。
    assert!(
        count() <= before + 8,
        "clipboard contexts leaked X11 windows"
    );
}

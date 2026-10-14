//! X11 XTest 粘贴；按当前键盘映射查找按键，不假设美式键盘固定键码。
use super::x11;
use std::io;
use x11rb::{
    CURRENT_TIME,
    connection::Connection,
    protocol::{
        xproto::{ConnectionExt, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, KeyButMask},
        xtest::ConnectionExt as _,
    },
};

/// Wayland 原生客户端拒绝 XTest 输入，明确返回不支持而不伪报成功。
pub fn ensure_accessibility_trusted() -> io::Result<()> {
    if super::is_wayland() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Automatic paste is unavailable in a Wayland session; copy the item and paste manually",
        ));
    }
    Ok(())
}

/// X11 不需要 Windows 的菜单键屏蔽标记。
pub fn mask_modifier_release() -> io::Result<()> {
    Ok(())
}

/// 查询当前修饰键状态，用于等用户释放唤起快捷键。
pub fn modifiers_pressed() -> bool {
    let Ok(display) = x11::connection() else {
        return false;
    };
    display
        .conn
        .query_pointer(display.root)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|reply| {
            u16::from(reply.mask)
                & u16::from(
                    KeyButMask::SHIFT | KeyButMask::CONTROL | KeyButMask::MOD1 | KeyButMask::MOD4,
                )
                != 0
        })
}

/// 从服务端键盘映射查找 keysym，兼容重映射后的键码。
fn keycode(display: &x11::Display, keysym: u32) -> io::Result<u8> {
    let setup = display.conn.setup();
    let min = setup.min_keycode;
    let mapping = display
        .conn
        .get_keyboard_mapping(min, setup.max_keycode - min + 1)
        .map_err(io::Error::other)?
        .reply()
        .map_err(io::Error::other)?;
    let width = usize::from(mapping.keysyms_per_keycode);
    if width == 0 {
        return Err(io::Error::other("empty X11 keyboard mapping"));
    }
    mapping
        .keysyms
        .chunks(width)
        .position(|keys| keys.contains(&keysym))
        .and_then(|index| u8::try_from(usize::from(min) + index).ok())
        .ok_or_else(|| io::Error::other(format!("keysym {keysym:#x} is unavailable")))
}

/// 注入 Ctrl+V；无论中间是否报错都尝试释放合成按键。
pub fn simulate_paste() -> io::Result<()> {
    ensure_accessibility_trusted()?;
    let display = x11::connection()?;
    let control = keycode(&display, 0xffe3)?;
    let paste = keycode(&display, u32::from(b'v'))?;
    let send = |kind, code| -> io::Result<()> {
        display
            .conn
            .xtest_fake_input(kind, code, CURRENT_TIME, display.root, 0, 0, 0)
            .map_err(io::Error::other)?
            .check()
            .map_err(io::Error::other)
    };
    send(KEY_PRESS_EVENT, control)?;
    let pressed = send(KEY_PRESS_EVENT, paste);
    let released = send(KEY_RELEASE_EVENT, paste);
    let control_released = send(KEY_RELEASE_EVENT, control);
    display.conn.flush().map_err(io::Error::other)?;
    pressed.and(released).and(control_released)
}

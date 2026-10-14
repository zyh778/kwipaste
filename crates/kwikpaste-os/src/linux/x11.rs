//! X11 连接及窗口操作；每线程复用连接，避免在剪贴板回调中反复握手。
use crate::geometry::{Point, Rect};
use std::{cell::RefCell, io, rc::Rc, thread, time::Duration};
use x11rb::{
    CURRENT_TIME,
    connection::Connection,
    protocol::xproto::{
        self, AtomEnum, ConfigureWindowAux, ConnectionExt, InputFocus, KeyButMask, MapState,
        StackMode,
    },
    rust_connection::RustConnection,
};

pub struct Display {
    pub conn: RustConnection,
    pub screen: usize,
    pub root: u32,
}

thread_local! {
    static DISPLAY: RefCell<Option<Rc<Display>>> = const { RefCell::new(None) };
}

/// 获取当前线程的 X11 连接；无显示服务时返回错误，调用方决定如何降级。
pub fn connection() -> io::Result<Rc<Display>> {
    DISPLAY.with(|slot| {
        if let Some(display) = slot.borrow().as_ref() {
            return Ok(display.clone());
        }
        let (conn, screen) = RustConnection::connect(None).map_err(io::Error::other)?;
        let root = conn.setup().roots[screen].root;
        let display = Rc::new(Display { conn, screen, root });
        *slot.borrow_mut() = Some(display.clone());
        Ok(display)
    })
}

/// 查询扩展属性名对应的原子。
pub fn atom(display: &Display, name: &str) -> io::Result<u32> {
    Ok(display
        .conn
        .intern_atom(false, name.as_bytes())
        .map_err(io::Error::other)?
        .reply()
        .map_err(io::Error::other)?
        .atom)
}

/// 读取窗口属性，限制为 64 KiB，避免不可信客户端发布无限长属性。
pub fn property(window: u32, name: &str) -> io::Result<xproto::GetPropertyReply> {
    let display = connection()?;
    display
        .conn
        .get_property(
            false,
            window,
            atom(&display, name)?,
            AtomEnum::ANY,
            0,
            16384,
        )
        .map_err(io::Error::other)?
        .reply()
        .map_err(io::Error::other)
}

/// EWMH 前台窗口仍指向原应用，可用于来源归因。
pub fn active_window() -> Option<u32> {
    let display = connection().ok()?;
    property(display.root, "_NET_ACTIVE_WINDOW")
        .ok()?
        .value32()?
        .next()
        .filter(|id| *id > 1)
}

/// 实际键盘焦点可能是应用的子窗口，粘贴恢复时保留此句柄。
pub fn focused_window() -> Option<u32> {
    let display = connection().ok()?;
    let focus = display.conn.get_input_focus().ok()?.reply().ok()?.focus;
    (focus > 1).then_some(focus)
}

/// 获取根窗口物理像素坐标中的鼠标位置。
pub fn pointer() -> io::Result<Point> {
    let display = connection()?;
    let reply = display
        .conn
        .query_pointer(display.root)
        .map_err(io::Error::other)?
        .reply()
        .map_err(io::Error::other)?;
    Ok(Point {
        x: reply.root_x.into(),
        y: reply.root_y.into(),
    })
}

/// 只有已映射窗口才能接收输入；服务端错误向上传递。
pub fn focus(window: u32) -> io::Result<()> {
    if window <= 1 {
        return Ok(());
    }
    let display = connection()?;
    display
        .conn
        .set_input_focus(InputFocus::PARENT, window, CURRENT_TIME)
        .map_err(io::Error::other)?
        .check()
        .map_err(io::Error::other)?;
    display.conn.flush().map_err(io::Error::other)
}

/// 移动并调整原生窗口，尺寸至少为一个物理像素。
pub fn place(window: u32, rect: Rect) -> io::Result<()> {
    let display = connection()?;
    let values = ConfigureWindowAux::new()
        .x(rect.left)
        .y(rect.top)
        .width(rect.width().max(1) as u32)
        .height(rect.height().max(1) as u32)
        .stack_mode(StackMode::ABOVE);
    display
        .conn
        .configure_window(window, &values)
        .map_err(io::Error::other)?
        .check()
        .map_err(io::Error::other)?;
    display.conn.flush().map_err(io::Error::other)
}

/// 映射窗口；预览不取焦点，面板需要显式接收 GPUI 键盘事件。
pub fn show(window: u32, take_focus: bool) -> io::Result<()> {
    let display = connection()?;
    display
        .conn
        .map_window(window)
        .map_err(io::Error::other)?
        .check()
        .map_err(io::Error::other)?;
    display
        .conn
        .configure_window(
            window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        )
        .map_err(io::Error::other)?
        .check()
        .map_err(io::Error::other)?;
    display.conn.flush().map_err(io::Error::other)?;
    if take_focus {
        focus(window)?;
    }
    Ok(())
}

/// 隐藏窗口，不影响其它应用的焦点选择。
pub fn hide(window: u32) -> io::Result<()> {
    let display = connection()?;
    display
        .conn
        .unmap_window(window)
        .map_err(io::Error::other)?
        .check()
        .map_err(io::Error::other)?;
    display.conn.flush().map_err(io::Error::other)
}

/// 将窗口客户区坐标转换到根窗口，供记忆位置与预览定位复用。
pub fn geometry(window: u32) -> io::Result<Rect> {
    let display = connection()?;
    let geometry = display
        .conn
        .get_geometry(window)
        .map_err(io::Error::other)?
        .reply()
        .map_err(io::Error::other)?;
    let origin = display
        .conn
        .translate_coordinates(window, display.root, 0, 0)
        .map_err(io::Error::other)?
        .reply()
        .map_err(io::Error::other)?;
    Ok(Rect {
        left: origin.dst_x.into(),
        top: origin.dst_y.into(),
        right: i32::from(origin.dst_x) + i32::from(geometry.width),
        bottom: i32::from(origin.dst_y) + i32::from(geometry.height),
    })
}

/// 查询真实映射状态，不依赖本地缓存。
pub fn is_visible(window: u32) -> bool {
    let Ok(display) = connection() else {
        return false;
    };
    display
        .conn
        .get_window_attributes(window)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|reply| reply.map_state == MapState::VIEWABLE)
}

/// 手势要改的边；语义与 GPUI 的 `ResizeEdge` 一致，`None` 是拖动。
enum Gesture {
    Move,
    Resize {
        left: bool,
        right: bool,
        top: bool,
        bottom: bool,
        min: (i32, i32),
    },
}

/// 按住左键期间轮询指针位置并直接配置窗口几何。面板是 override-redirect 窗口，
/// 窗口管理器不认它的 `_NET_WM_MOVERESIZE`，拖动缩放只能自己做；松开左键即结束。
/// 轮询线程用自己的 X 连接，移动产生的 ConfigureNotify 由 GPUI 的连接正常收到。
fn begin_gesture(window: u32, gesture: Gesture) {
    thread::spawn(move || {
        let Ok(display) = connection() else {
            return;
        };
        let (Ok(start), Ok(pointer_start)) = (geometry(window), pointer()) else {
            return;
        };
        loop {
            thread::sleep(Duration::from_millis(16));
            let Ok(reply) = display
                .conn
                .query_pointer(display.root)
                .map_err(io::Error::other)
                .and_then(|cookie| cookie.reply().map_err(io::Error::other))
            else {
                break;
            };
            if !reply.mask.contains(KeyButMask::BUTTON1) {
                break;
            }
            let dx = i32::from(reply.root_x) - pointer_start.x;
            let dy = i32::from(reply.root_y) - pointer_start.y;
            let values = match gesture {
                Gesture::Move => ConfigureWindowAux::new()
                    .x(start.left + dx)
                    .y(start.top + dy),
                Gesture::Resize {
                    left,
                    right,
                    top,
                    bottom,
                    min,
                } => {
                    let mut rect = start;
                    if left {
                        rect.left = start.left + dx;
                    }
                    if right {
                        rect.right = start.right + dx;
                    }
                    if top {
                        rect.top = start.top + dy;
                    }
                    if bottom {
                        rect.bottom = start.bottom + dy;
                    }
                    if rect.width() < min.0 {
                        if left {
                            rect.left = rect.right - min.0;
                        } else {
                            rect.right = rect.left + min.0;
                        }
                    }
                    if rect.height() < min.1 {
                        if top {
                            rect.top = rect.bottom - min.1;
                        } else {
                            rect.bottom = rect.top + min.1;
                        }
                    }
                    ConfigureWindowAux::new()
                        .x(rect.left)
                        .y(rect.top)
                        .width(rect.width().max(1) as u32)
                        .height(rect.height().max(1) as u32)
                }
            };
            if display
                .conn
                .configure_window(window, &values)
                .is_err()
                || display.conn.flush().is_err()
            {
                break;
            }
        }
    });
}

/// 按住左键拖动窗口（物理像素；见 [`begin_gesture`] 的说明）。
pub fn begin_drag(window: u32) {
    begin_gesture(window, Gesture::Move);
}

/// 窗口缩放的八条边。
#[derive(Debug, Clone, Copy)]
pub enum ResizeEdge {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// 按住左键沿一条边缩放窗口，宽高不小于 `min`（物理像素）。
pub fn begin_resize(window: u32, edge: ResizeEdge, min: (i32, i32)) {
    let (left, right, top, bottom) = match edge {
        ResizeEdge::Left => (true, false, false, false),
        ResizeEdge::Right => (false, true, false, false),
        ResizeEdge::Top => (false, false, true, false),
        ResizeEdge::Bottom => (false, false, false, true),
        ResizeEdge::TopLeft => (true, false, true, false),
        ResizeEdge::TopRight => (false, true, true, false),
        ResizeEdge::BottomLeft => (true, false, false, true),
        ResizeEdge::BottomRight => (false, true, false, true),
    };
    begin_gesture(
        window,
        Gesture::Resize {
            left,
            right,
            top,
            bottom,
            min,
        },
    );
}

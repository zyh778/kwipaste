//! X11 显示器与工作区；坐标为物理像素，缩放采用桌面的 Xft.dpi。
use super::x11;
use crate::geometry::{Point, Rect};
use std::io;
use x11rb::{connection::Connection, protocol::randr::ConnectionExt};

pub const BASE_DPI: u32 = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorInfo {
    pub cursor: Point,
    pub monitor: Rect,
    pub work_area: Rect,
    pub dpi: u32,
}

impl MonitorInfo {
    /// 使用桌面逻辑缩放，避免按屏幕物理尺寸把高分屏放大两次。
    pub fn scale(&self) -> f64 {
        f64::from(self.dpi) / f64::from(BASE_DPI)
    }
}

/// EWMH 工作区包含 dock/panel 保留空间，按当前虚拟桌面读取。
fn work_area(root: u32) -> Option<Rect> {
    let desktop = x11::property(root, "_NET_CURRENT_DESKTOP")
        .ok()
        .and_then(|reply| reply.value32()?.next())
        .unwrap_or(0) as usize;
    let reply = x11::property(root, "_NET_WORKAREA").ok()?;
    let values: Vec<_> = reply.value32()?.collect();
    let offset = desktop.checked_mul(4)?;
    let values = values.get(offset..offset.checked_add(4)?)?;
    Some(Rect {
        left: values[0] as i32,
        top: values[1] as i32,
        right: (values[0] as i32).saturating_add(values[2] as i32),
        bottom: (values[1] as i32).saturating_add(values[3] as i32),
    })
}

/// 缩放跟随 Xft.dpi，未设置时使用 96 DPI。
fn dpi(root: u32) -> u32 {
    x11::property(root, "RESOURCE_MANAGER")
        .ok()
        .and_then(|reply| String::from_utf8(reply.value).ok())
        .and_then(|resources| {
            resources.lines().find_map(|line| {
                let (key, value) = line.split_once(':')?;
                (key.trim() == "Xft.dpi")
                    .then(|| value.trim().parse::<f64>().ok())
                    .flatten()
            })
        })
        .filter(|value| value.is_finite() && *value > 0.0)
        .map_or(BASE_DPI, |value| value.round().clamp(96.0, 384.0) as u32)
}

/// 枚举 RandR 显示器；老 X Server 不支持 GetMonitors 时回退到根窗口尺寸。
pub fn all() -> Vec<MonitorInfo> {
    let Ok(display) = x11::connection() else {
        return Vec::new();
    };
    let screen = &display.conn.setup().roots[display.screen];
    let mut monitors: Vec<Rect> = display
        .conn
        .randr_get_monitors(display.root, true)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|mut reply| {
            reply.monitors.sort_by_key(|monitor| !monitor.primary);
            reply
                .monitors
                .into_iter()
                .map(|monitor| Rect {
                    left: monitor.x.into(),
                    top: monitor.y.into(),
                    right: i32::from(monitor.x) + i32::from(monitor.width),
                    bottom: i32::from(monitor.y) + i32::from(monitor.height),
                })
                .collect()
        })
        .unwrap_or_default();
    if monitors.is_empty() {
        monitors.push(Rect {
            left: 0,
            top: 0,
            right: screen.width_in_pixels.into(),
            bottom: screen.height_in_pixels.into(),
        });
    }
    let work = work_area(display.root);
    let dpi = dpi(display.root);
    monitors
        .into_iter()
        .map(|monitor| {
            let intersection = work
                .map(|work| Rect {
                    left: monitor.left.max(work.left),
                    top: monitor.top.max(work.top),
                    right: monitor.right.min(work.right),
                    bottom: monitor.bottom.min(work.bottom),
                })
                .filter(|rect| rect.width() > 0 && rect.height() > 0)
                .unwrap_or(monitor);
            MonitorInfo {
                monitor,
                work_area: intersection,
                dpi,
                cursor: Point {
                    x: monitor.left + monitor.width() / 2,
                    y: monitor.top + monitor.height() / 2,
                },
            }
        })
        .collect()
}

/// 主显示器优先；没有 RandR 信息时仍能使用根窗口。
pub fn primary() -> io::Result<MonitorInfo> {
    all()
        .into_iter()
        .next()
        .ok_or_else(|| io::Error::other("no X11 monitor is available"))
}

/// 鼠标所在显示器，用于跟随光标定位。
pub fn at_cursor() -> io::Result<MonitorInfo> {
    let cursor = x11::pointer()?;
    let monitors = all();
    let info = monitors
        .iter()
        .find(|info| {
            cursor.x >= info.monitor.left
                && cursor.x < info.monitor.right
                && cursor.y >= info.monitor.top
                && cursor.y < info.monitor.bottom
        })
        .or(monitors.first())
        .ok_or_else(|| io::Error::other("no X11 monitor is available"))?;
    Ok(MonitorInfo { cursor, ..*info })
}

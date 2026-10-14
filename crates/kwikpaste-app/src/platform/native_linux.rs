//! Linux 面板胶水：GPUI 接收键盘与输入法，X11 负责定位和前台恢复。
use super::{
    editing::{EditReport, EditTrigger},
    panel::{PANEL_SIZE, PanelCommand},
    window_state::PanelLayout,
};
use anyhow::{anyhow, bail};
use async_channel::Sender;
use gpui::Window;
use kwikpaste_core::{settings::WindowPosition, window_state::WindowGeometry};
use kwikpaste_os::{
    geometry::{Point, Rect, center_in, follow_cursor, scale_size},
    linux::{monitor, x11},
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::cell::Cell;

pub struct Placement {
    pub rect: Rect,
}

pub struct NativePanel {
    xid: u32,
    previous: Cell<Option<u32>>,
    editing: Cell<bool>,
    text_scale: Cell<f64>,
    scale: Cell<f64>,
    last: Cell<Option<WindowGeometry>>,
}

/// 提取 X11 原生句柄；纯 Wayland 无绝对定位和焦点恢复权限。
fn xid(window: &Window) -> anyhow::Result<u32> {
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|err| anyhow!("panel window handle: {err:?}"))?;
    match handle.as_raw() {
        RawWindowHandle::Xcb(handle) => Ok(handle.window.get()),
        RawWindowHandle::Xlib(handle) => Ok(u32::try_from(handle.window)?),
        _ => bail!("the Linux panel requires X11 or XWayland"),
    }
}

/// 任意 GPUI 窗口的 X11 句柄；拖动/缩放手势按窗口取，拿不到就退化为不可拖。
pub fn window_xid(window: &Window) -> Option<u32> {
    xid(window).ok()
}

impl NativePanel {
    /// 接管 GPUI 创建的弹出窗口，保留逻辑尺寸与还原焦点所需信息。
    pub fn attach(window: &Window, text_scale: f64) -> anyhow::Result<Self> {
        Ok(Self {
            xid: xid(window)?,
            previous: Cell::new(None),
            editing: Cell::new(false),
            text_scale: Cell::new(text_scale),
            scale: Cell::new(f64::from(window.scale_factor())),
            last: Cell::new(None),
        })
    }

    /// Linux 的失焦通知由 GPUI observer 发送，不安装全局键盘钩子。
    pub fn set_command_sender(&self, _sender: Sender<PanelCommand>) {}

    /// 初始窗口必须隐藏，避免 GPUI 后端忽略 show=false 时闪出空面板。
    pub fn install(&self) -> anyhow::Result<()> {
        Ok(x11::hide(self.xid)?)
    }

    /// 读取服务端映射状态，避免框架缓存与原生窗口不同步。
    pub fn is_visible(&self) -> bool {
        x11::is_visible(self.xid)
    }
    /// 自测日志使用原生 XID。
    pub fn raw_handle(&self) -> isize {
        self.xid as isize
    }
    /// 字体缩放在下一次显示时应用于最小尺寸。
    pub fn set_text_scale(&self, scale: f64) {
        self.text_scale.set(scale);
    }

    /// 复用既有几何函数，按工作区裁剪跟随、居中与记忆位置。
    pub fn place(&self, layout: &PanelLayout) -> anyhow::Result<Placement> {
        let saved = self.last.get().or(layout.saved);
        let cursor_monitor = monitor::at_cursor().or_else(|_| monitor::primary())?;
        let remembered = saved
            .filter(|_| matches!(layout.position, WindowPosition::Remember))
            .and_then(|geometry| {
                let origin = Point {
                    x: (geometry.x * geometry.scale).round() as i32,
                    y: (geometry.y * geometry.scale).round() as i32,
                };
                monitor::all()
                    .into_iter()
                    .find(|info| {
                        origin.x >= info.monitor.left
                            && origin.x < info.monitor.right
                            && origin.y >= info.monitor.top
                            && origin.y < info.monitor.bottom
                    })
                    .map(|info| (info, origin))
            });
        let info = remembered.map_or(cursor_monitor, |(info, _)| info);
        self.scale.set(info.scale());
        let default = (
            PANEL_SIZE.0 * self.text_scale.get(),
            PANEL_SIZE.1 * self.text_scale.get(),
        );
        let logical = saved.map_or(default, |geometry| {
            (
                geometry.width.max(default.0),
                geometry.height.max(default.1),
            )
        });
        let size = scale_size(logical, info.scale());
        let rect = match (layout.position, remembered) {
            (WindowPosition::FollowCursor, _) => follow_cursor(info.cursor, info.work_area, size),
            (WindowPosition::Remember, Some((_, origin))) => {
                follow_cursor(origin, info.work_area, size)
            }
            _ => center_in(info.work_area, size),
        };
        x11::place(self.xid, rect)?;
        Ok(Placement { rect })
    }

    /// 显示前保存原焦点；弹出窗口获得 X11 输入焦点后才能接收 GPUI 键盘事件。
    pub fn show(&self) {
        let current = x11::focused_window().or_else(x11::active_window);
        if current != Some(self.xid) {
            self.previous.set(current);
        }
        if let Err(err) = x11::show(self.xid, true) {
            log::warn!("panel show failed: {err}");
        }
    }

    /// GPUI FocusOut 驱动失焦隐藏，不注册原生全局钩子。
    pub fn start_hooks(&self) {}
    /// 与 start_hooks 对称，Linux 无需清理全局钩子。
    pub fn stop_hooks(&self) {}
    /// 固定面板让出焦点后，再次快捷键需要恢复键盘输入。
    pub fn should_recapture_on_toggle(&self, visible: bool, _summon: bool) -> bool {
        visible && x11::focused_window() != Some(self.xid)
    }
    /// 固定面板粘贴时临时让出输入，避免 Ctrl+V 落到自己的搜索框。
    pub fn set_input_capture(&self, captured: bool) {
        if captured {
            self.raise();
        } else {
            self.restore_focus();
        }
    }
    /// 验证放置请求，异常仅记日志，不在渲染路径 panic。
    pub fn verify(&self, target: &Placement) {
        if x11::geometry(self.xid).is_ok_and(|rect| rect != target.rect) {
            log::debug!("X11 panel geometry differs from requested placement");
        }
    }
    /// 提升已有面板时不覆盖原粘贴目标。
    pub fn raise(&self) {
        if let Err(err) = x11::show(self.xid, true) {
            log::warn!("panel raise failed: {err}");
        }
    }
    /// 用户已经切换到别的应用时保留其选择，只归还本面板仍持有的焦点。
    fn restore_focus(&self) {
        if x11::focused_window() == Some(self.xid)
            && let Some(previous) = self.previous.get()
            && let Err(err) = x11::focus(previous)
        {
            log::debug!("previous X11 focus is unavailable: {err}");
        }
    }
    /// 保存逻辑几何并隐藏，自动粘贴之前恢复原应用焦点。
    pub fn hide(&self) -> Option<WindowGeometry> {
        let scale = self.scale.get();
        let geometry = x11::geometry(self.xid).ok().map(|rect| WindowGeometry {
            x: f64::from(rect.left) / scale,
            y: f64::from(rect.top) / scale,
            width: f64::from(rect.width()) / scale,
            height: f64::from(rect.height()) / scale,
            scale,
        });
        self.restore_focus();
        if let Err(err) = x11::hide(self.xid) {
            log::warn!("panel hide failed: {err}");
        }
        self.last.set(geometry);
        self.editing.set(false);
        geometry
    }
    /// Linux 的输入框编辑态仍由 GPUI 管理。
    pub fn is_editing(&self) -> bool {
        self.editing.get()
    }
    /// 面板显示时已持有输入焦点，只切换编辑状态。
    pub fn begin_editing(&self, _trigger: EditTrigger) -> anyhow::Result<EditReport> {
        x11::focus(self.xid)?;
        self.editing.set(true);
        Ok(EditReport {
            previous_foreground: self.previous.get().unwrap_or(0) as isize,
            ..Default::default()
        })
    }
    /// 退出编辑态不抢回已由用户切换走的前台。
    pub fn end_editing(&self, restore_foreground: bool) {
        self.editing.set(false);
        if restore_foreground {
            self.restore_focus();
        }
    }
    /// 探针输出 XID 与焦点状态，便于运行验收定位故障。
    pub fn probe_fields(&self, _placement: Option<&Placement>) -> String {
        format!(
            ",\"xid\":{},\"visible\":{},\"editing\":{},\"foreground\":{}",
            self.xid,
            self.is_visible(),
            self.is_editing(),
            x11::focused_window().unwrap_or(0)
        )
    }
}

//! 面板几何存档：`Core::window_state()` 里的 `clipboard` 项（与 1.x 剪贴板窗口同一个 label）。
//!
//! - 隐藏时保存：外框左上角（逻辑像素，与 1.x 的 `outer_position` 同义）、内容区尺寸（逻辑像素）和
//!   所在显示器的缩放。
//! - 显示时按设置 `clipboard.window.position` 摆放：跟随光标、居中，或「记住位置」时回到存档位置
//!   （存档位置已不在任何显示器上时居中到光标所在显示器，与 1.x 相同）。尺寸取存档，但不小于
//!   当前文本缩放下的默认尺寸。
//! - 首次启动时把 1.x 的 `window-state.json`（物理像素）按当前显示器列表换算成原生存档。

use gpui::App;
use kwikpaste_core::Core;
use kwikpaste_core::settings::WindowPosition;
use kwikpaste_core::window_state::{LegacyMonitor, WindowGeometry, legacy_to_logical};

use crate::core_host;

/// 面板在窗口存档里的 label。
pub const PANEL_LABEL: &str = "clipboard";

/// 一次显示要用的摆放方式与存档几何。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanelLayout {
    pub position: WindowPosition,
    pub saved: Option<WindowGeometry>,
}

impl Default for PanelLayout {
    fn default() -> Self {
        Self {
            position: WindowPosition::FollowCursor,
            saved: None,
        }
    }
}

/// 当前设置下的摆放方式与面板的存档几何。
pub fn layout(cx: &App) -> PanelLayout {
    let Some(core) = core_host::core(cx) else {
        return PanelLayout::default();
    };

    PanelLayout {
        position: core.settings().clipboard.window.position,
        saved: core.window_state().get(PANEL_LABEL),
    }
}

/// 在后台保存面板的几何，失败只记日志。
pub fn save(cx: &App, geometry: WindowGeometry) {
    let Some(core) = core_host::core(cx).cloned() else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            if let Err(err) = core.window_state().save(PANEL_LABEL, geometry) {
                log::warn!("the panel geometry could not be saved: {err}");
            }
        })
        .detach();
}

/// 没有原生存档、只有 1.x 存档时换算一次（在主线程上调用：macOS 读显示器要主线程）。
pub fn migrate_legacy(core: &Core) {
    let store = core.window_state();
    if !store.has_pending_legacy() {
        return;
    }
    let monitors = legacy_monitors();
    match store.migrate_legacy(|_, state| legacy_to_logical(state, &monitors)) {
        Ok(migrated) => log::info!(
            "converted {migrated} window state(s) from 1.x with {} monitor(s)",
            monitors.len()
        ),
        Err(err) => log::warn!("the 1.x window state could not be converted: {err}"),
    }
}

/// 1.x（tao）坐标系里的显示器：Windows 是虚拟桌面的物理像素；macOS 是 point 乘该屏的
/// `backingScaleFactor`。
fn legacy_monitors() -> Vec<LegacyMonitor> {
    // Linux 未发布 1.x，本机没有需要迁移的旧窗口坐标。
    #[cfg(target_os = "linux")]
    {
        Vec::new()
    }
    #[cfg(target_os = "windows")]
    {
        kwikpaste_os::win::monitor::all()
            .into_iter()
            .map(|info| LegacyMonitor {
                x: info.monitor.left,
                y: info.monitor.top,
                width: info.monitor.width().max(0) as u32,
                height: info.monitor.height().max(0) as u32,
                scale: info.scale(),
            })
            .collect()
    }
    #[cfg(target_os = "macos")]
    {
        kwikpaste_os::mac::monitor::screens()
            .into_iter()
            .map(|screen| LegacyMonitor {
                x: (screen.x * screen.scale).round() as i32,
                y: (screen.y * screen.scale).round() as i32,
                width: (screen.width * screen.scale).round() as u32,
                height: (screen.height * screen.scale).round() as u32,
                scale: screen.scale,
            })
            .collect()
    }
}

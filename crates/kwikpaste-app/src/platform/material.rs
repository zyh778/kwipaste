//! 窗口材质：设置 `appearance.material`（default / mica / acrylic），与 1.x 的
//! `src-tauri/src/window/material.rs` 相同的契约和门槛，DWM 细节在 `kwikpaste_os::win::material`。
//!
//! 设置值先收敛成「这台机器上真正生效的材质」（[`WindowMaterial::effective`]）：
//! - 系统不支持（Mica 要 Windows 11，Acrylic 要 Windows 10 1809）→ default，与 1.x 相同；
//! - 系统关了「透明效果」、开了高对比度 → default（1.x 前端的 `prefers-reduced-transparency`）；
//! - DirectComposition 被关掉（崩溃后的降级模式，或 `GPUI_DISABLE_DIRECT_COMPOSITION`）→ default：
//!   那时交换链没有 alpha。
//!
//! 生效的材质套到面板上：default 用 GPUI `Opaque`（拿回 ClearType）加圆角；mica / acrylic 用
//! `Transparent`，清掉 GPUI 的 accent 再设 DWM 背板（Windows 11 22H2 之前的 acrylic 用 GPUI
//! `Blurred`）。窗口深浅色跟随 `appearance.theme`（auto 跟系统），与 1.x `setTheme` 相同。
//! DWM 对非活动窗口只画背板的纯色回退，面板、预览这类从不激活的浮层因此把框架状态留在活动
//! （`kwikpaste_os::win::material::keep_popup_frame_active`）；偏好设置等普通窗口照系统惯例，
//! 失焦时回退。
//! 设置变化、系统设置变化（透明效果、深浅色、高对比度）时重新套。这里是全应用唯一调用
//! `set_background_appearance` 的地方：别处调用会重设 accent，打掉 DWM 背板。
//!
//! # UI 怎么接
//! 窗口外层 gpui-component `Root` 的不透明底色在材质下已经去掉（`kwikpaste_ui::set_root_translucent`），
//! 根元素的底色按 [`current`]`(cx).effective` 选（[`panel_surface`]、[`shell_surface`]、
//! [`chrome_surface`]），订阅 `cx.observe_global::<WindowMaterial>()`：
//! - `Default`：不透明的 token 底色，窗口本身不透明；
//! - `Mica`：不铺底色，直接露出系统 Mica（Mica 本身不透明，色调来自桌面壁纸）；
//! - `Acrylic`：面板色按主题色板的 `materials` 比例半透明，压住背后模糊内容的花色、保证文字可读。
//!
//! （以上是 Windows；macOS 的系统材质自带色调但不压花色，两种材质都铺底色，见色板的 `MATERIALS`。）
//!
//! 每个窗口只铺一层材质底色：子视图再铺一层会叠成接近不透明，材质就看不出来了。
//! 材质下不要用不透明的浮层盖住滚动内容：不透明底色在材质上是一块实色，模糊也盖不住下面滚过的行；
//! 需要一直可见的东西（置顶行、表头）放在滚动容器外面。
//!
//! macOS：default 用不透明窗口，mica / acrylic 用 `Transparent` 加系统 `NSVisualEffectView`
//! （`kwikpaste_os::mac::material`，所有窗口都一样，面板另加圆角）；系统关闭透明效果或高对比度时
//! 回退 default。

use gpui::{AnyWindowHandle, App, Global, Hsla, Window, WindowBackgroundAppearance};
use kwikpaste_core::settings::{Material, Theme};
use kwikpaste_ui::theme;

use super::panel::Panel;
use super::system::SystemSignals;
use crate::core_host;

/// 当前已经创建的应用窗口；材质变化时所有窗口都要同步重套原生背板和 Root。
#[derive(Default)]
pub struct MaterialWindows {
    handles: Vec<AnyWindowHandle>,
}

impl Global for MaterialWindows {}

/// 当前窗口材质。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowMaterial {
    /// 设置值。
    pub requested: Material,
    /// 收敛后真正生效的材质：UI 按它选底色。
    pub effective: Material,
    /// 窗口是否用深色外观（Mica / Acrylic 的底色深浅）。
    pub dark: bool,
}

impl Global for WindowMaterial {}

impl WindowMaterial {
    /// 窗口是半透明的（有系统背板），根元素要用半透明底色。
    pub fn is_translucent(&self) -> bool {
        self.effective != Material::Default
    }
}

/// 当前窗口材质；平台层启动之前为 default。
pub fn current(cx: &App) -> WindowMaterial {
    cx.try_global::<WindowMaterial>()
        .copied()
        .unwrap_or(WindowMaterial {
            requested: Material::Default,
            effective: Material::Default,
            dark: false,
        })
}

/// 返回面板内容层的不透明度；默认材质返回 `None`，由调用方使用不透明 token。
#[allow(dead_code, reason = "保留供平台材质探针读取统一 alpha")]
pub fn surface_alpha(cx: &App) -> Option<f32> {
    let materials = theme::semantic(cx).materials;
    match current(cx).effective {
        Material::Mica => Some(materials.panel_mica_alpha),
        Material::Acrylic => Some(materials.panel_acrylic_alpha),
        Material::Default => None,
    }
}

fn theme_kind(material: Material) -> theme::MaterialKind {
    match material {
        Material::Default => theme::MaterialKind::None,
        Material::Mica => theme::MaterialKind::Mica,
        Material::Acrylic => theme::MaterialKind::Acrylic,
    }
}

/// 窗口“框架”区（偏好设置的侧栏、标题栏）底色：默认材质与窗口底同色（`bg_layout`），
/// 靠细分隔线区分；材质下照 1.x 叠一层很淡的 `bg_container`。
pub fn chrome_surface(cx: &App) -> Hsla {
    let tokens = theme::semantic(cx);
    theme::palette::material_chrome_surface(tokens, theme_kind(current(cx).effective))
}

/// 剪贴板面板的底色：默认材质是 `bg_container`，材质下是半透明的内容层。条目是扁平的行，
/// 不再有自己的卡片底。
pub fn panel_surface(cx: &App) -> Hsla {
    let tokens = theme::semantic(cx);
    theme::palette::material_surface(tokens, theme_kind(current(cx).effective))
}

/// 窗口外壳底色（1.x `.kp-material-surface`）：默认材质用窗口自己的 token；材质下统一是
/// `bg_container` 按面板同样的比例半透明——暗色的 `bg_layout` 是纯黑，不能拿来透底。
pub fn shell_surface(cx: &App, default: Hsla) -> Hsla {
    if current(cx).effective == Material::Default {
        default
    } else {
        theme::palette::material_surface(theme::semantic(cx), theme_kind(current(cx).effective))
    }
}

/// 注册一个窗口并立即套用当前材质。窗口创建后 Root 已经存在，因此这里同时处理 Root 底色。
pub fn register_window(handle: AnyWindowHandle, cx: &mut App) {
    let handles = cx
        .try_global::<MaterialWindows>()
        .map_or_else(Vec::new, |windows| windows.handles.clone());
    if !handles.contains(&handle) {
        let mut next = handles;
        next.push(handle);
        cx.set_global(MaterialWindows { handles: next });
    }
    let material = current(cx);
    cx.defer(move |cx| {
        apply_one(handle, cx, &material);
    });
}

/// 按当前设置和系统设置重新计算材质，并套到所有已创建窗口。设置或系统设置变化时调用。
pub fn apply(cx: &mut App) {
    let Some(core) = core_host::core(cx) else {
        return;
    };
    let appearance = core.settings().appearance;
    let signals = cx.try_global::<SystemSignals>().copied();
    let dark = match appearance.theme {
        Theme::Light => false,
        Theme::Dark => true,
        Theme::Auto => signals.is_some_and(|signals| signals.dark),
    };
    let effective = resolve(appearance.material, signals.as_ref());
    let material = WindowMaterial {
        requested: appearance.material,
        effective,
        dark,
    };
    let changed = cx.try_global::<WindowMaterial>() != Some(&material);
    cx.set_global(material);
    if changed {
        log::info!("window material: {material:?}");
        super::probe::material(&material);
    }

    let handles = cx
        .try_global::<MaterialWindows>()
        .map_or_else(Vec::new, |windows| windows.handles.clone());
    let mut open_handles = Vec::with_capacity(handles.len());
    for handle in handles {
        if apply_one(handle, cx, &material) {
            open_handles.push(handle);
        }
    }
    cx.set_global(MaterialWindows {
        handles: open_handles,
    });
    if let Some(panel) = cx.try_global::<Panel>() {
        panel.request(super::panel::PanelCommand::SetMaterial(material));
    }
}

fn apply_one(handle: AnyWindowHandle, cx: &mut App, material: &WindowMaterial) -> bool {
    match handle.update(cx, |_, window, cx| {
        apply_to_window(window, material);
        kwikpaste_ui::set_root_translucent(window, material.is_translucent(), cx);
    }) {
        Ok(()) => true,
        Err(err) => {
            log::debug!("window material not applied now: {err:#}");
            false
        }
    }
}

/// 设置值收敛成这台机器上能生效的材质。
fn resolve(requested: Material, signals: Option<&SystemSignals>) -> Material {
    if requested == Material::Default {
        return Material::Default;
    }
    if signals.is_some_and(|signals| !signals.transparency || signals.high_contrast)
        || std::env::var_os("GPUI_DISABLE_DIRECT_COMPOSITION").is_some()
        || crate::health::degraded()
    {
        return Material::Default;
    }
    if !supported(requested) {
        return Material::Default;
    }

    requested
}

#[cfg(target_os = "windows")]
fn supported(material: Material) -> bool {
    let support = kwikpaste_os::win::material::support(build());
    match material {
        Material::Default => true,
        Material::Mica => support.mica,
        Material::Acrylic => support.acrylic,
    }
}

#[cfg(target_os = "macos")]
fn supported(material: Material) -> bool {
    matches!(
        material,
        Material::Default | Material::Mica | Material::Acrylic
    )
}

#[cfg(target_os = "windows")]
fn build() -> u32 {
    static BUILD: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *BUILD.get_or_init(kwikpaste_os::win::material::build)
}

#[cfg(target_os = "windows")]
pub(super) fn apply_to_window(window: &mut Window, material: &WindowMaterial) {
    use kwikpaste_os::win::material::{
        Backdrop, acrylic_uses_backdrop, keep_popup_frame_active, set_backdrop, set_dark_mode,
        set_round_corners,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let hwnd = match HasWindowHandle::window_handle(window).map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get(),
        _ => return,
    };
    let build = build();
    let (appearance, backdrop) = match material.effective {
        Material::Default => (WindowBackgroundAppearance::Opaque, Backdrop::None),
        Material::Mica => (WindowBackgroundAppearance::Transparent, Backdrop::Mica),
        Material::Acrylic if acrylic_uses_backdrop(build) => {
            (WindowBackgroundAppearance::Transparent, Backdrop::Acrylic)
        }
        Material::Acrylic => (WindowBackgroundAppearance::Blurred, Backdrop::None),
    };

    // 顺序要紧：GPUI 先设 accent，再由我们清 accent、设深浅色、设背板。
    window.set_background_appearance(appearance);
    set_dark_mode(hwnd, material.dark);
    set_backdrop(hwnd, backdrop, build);
    set_round_corners(hwnd);
    keep_popup_frame_active(hwnd);
    window.refresh();
}

#[cfg(target_os = "macos")]
pub(super) fn apply_to_window(window: &mut Window, material: &WindowMaterial) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let appearance = if material.effective == Material::Default {
        WindowBackgroundAppearance::Opaque
    } else {
        WindowBackgroundAppearance::Transparent
    };
    // 顺序要紧：GPUI 先拿掉它自己的无色模糊视图，再放系统材质层。
    window.set_background_appearance(appearance);
    let ns_view = match HasWindowHandle::window_handle(window).map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::AppKit(handle)) => handle.ns_view,
        _ => return,
    };
    // SAFETY: 在 GPUI 的窗口更新里调用，窗口视图存活且在主线程。
    if let Err(err) = unsafe { kwikpaste_os::mac::material::apply(ns_view, material.effective) } {
        log::warn!("macOS window material not applied: {err}");
    }
    window.refresh();
}

/// Linux 暂无跨桌面统一的 Mica/Acrylic 材质，回退到不透明主题。
#[cfg(target_os = "linux")]
fn supported(material: Material) -> bool {
    material == Material::Default
}

/// 保持 Root 与原生窗口都不透明，避免合成器背景透出。
#[cfg(target_os = "linux")]
pub(super) fn apply_to_window(window: &mut Window, _material: &WindowMaterial) {
    window.set_background_appearance(WindowBackgroundAppearance::Opaque);
    window.refresh();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signals(transparency: bool, high_contrast: bool) -> SystemSignals {
        SystemSignals {
            text_scale: 1.0,
            high_contrast,
            reduce_motion: false,
            transparency,
            dark: false,
        }
    }

    #[test]
    fn reduced_transparency_and_high_contrast_fall_back_to_default() {
        assert_eq!(
            resolve(Material::Mica, Some(&signals(false, false))),
            Material::Default
        );
        assert_eq!(
            resolve(Material::Acrylic, Some(&signals(true, true))),
            Material::Default
        );
        assert_eq!(
            resolve(Material::Default, Some(&signals(true, false))),
            Material::Default
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn supported_materials_pass_through_on_this_machine() {
        // 本机是 Windows 11（build ≥ 22000）。
        if build() >= 22000 {
            assert_eq!(
                resolve(Material::Mica, Some(&signals(true, false))),
                Material::Mica
            );
            assert_eq!(
                resolve(Material::Acrylic, Some(&signals(true, false))),
                Material::Acrylic
            );
        }
    }
}

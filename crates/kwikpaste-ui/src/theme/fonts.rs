//! 字体栈：与 1.x（antd `fontFamily` 加 Chromium 回退）看起来一致。
//!
//! Windows 不能用 `.SystemUIFont`：GPUI 把它解析成图标标题字体，中文系统上是
//! “Microsoft YaHei UI”，西文字形会变。所以显式写 Segoe UI，中文、Emoji、符号各自回退；
//! 显式回退列表还能防止日文区域的系统用 Yu Gothic 字形显示中文。

use gpui::FontFallbacks;

/// 正文字体（antd `fontFamily` 里实际命中的那一个）。
#[cfg(target_os = "windows")]
pub const UI_FAMILY: &str = "Segoe UI";
/// 正文字体回退：中文、Emoji、符号。
#[cfg(target_os = "windows")]
pub const UI_FALLBACKS: &[&str] = &["Microsoft YaHei UI", "Segoe UI Emoji", "Segoe UI Symbol"];
/// 等宽字体（Kbd、KeyHint、颜色值、预览文本）。
#[cfg(target_os = "windows")]
pub const MONO_FAMILY: &str = "Consolas";
/// 等宽字体回退：中文。
#[cfg(target_os = "windows")]
pub const MONO_FALLBACKS: &[&str] = &["Microsoft YaHei UI"];

/// 正文字体：系统字体（`.AppleSystemUIFont`）。
#[cfg(target_os = "macos")]
pub const UI_FAMILY: &str = ".SystemUIFont";
/// 正文字体回退：中文、Emoji。
#[cfg(target_os = "macos")]
pub const UI_FALLBACKS: &[&str] = &["PingFang SC", "Apple Color Emoji"];
/// 等宽字体：Menlo（SF Mono 能否按名字解析待 macOS CI 确认，产品拍板项 D-15）。
#[cfg(target_os = "macos")]
pub const MONO_FAMILY: &str = "Menlo";
/// 等宽字体回退：中文。
#[cfg(target_os = "macos")]
pub const MONO_FALLBACKS: &[&str] = &["PingFang SC"];

/// GPUI 的正文回退列表。
pub fn ui_fallbacks() -> FontFallbacks {
    fallbacks(UI_FALLBACKS)
}

/// GPUI 的等宽回退列表。
pub fn mono_fallbacks() -> FontFallbacks {
    fallbacks(MONO_FALLBACKS)
}

fn fallbacks(families: &[&str]) -> FontFallbacks {
    FontFallbacks::from_fonts(families.iter().map(|family| family.to_string()).collect())
}

/// Linux 使用常见系统字体，并显式覆盖中文和 Emoji。
#[cfg(target_os = "linux")]
pub const UI_FAMILY: &str = "Noto Sans";
#[cfg(target_os = "linux")]
pub const UI_FALLBACKS: &[&str] = &["Noto Sans CJK SC", "DejaVu Sans", "Noto Color Emoji"];
#[cfg(target_os = "linux")]
pub const MONO_FAMILY: &str = "DejaVu Sans Mono";
#[cfg(target_os = "linux")]
pub const MONO_FALLBACKS: &[&str] = &["Noto Sans CJK SC", "Noto Color Emoji"];

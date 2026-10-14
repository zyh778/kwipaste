//! 系统界面语言，只在首次启动和恢复默认设置时用来决定应用语言。

/// 系统语言标签（BCP 47，如 `zh-CN`、`en-US`、macOS 上可能是 `zh-Hans-CN`）；取不到时为 `None`。
#[cfg(target_os = "windows")]
pub fn system_locale() -> Option<String> {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;

    let mut buffer = [0u16; 85];
    let length = unsafe { GetUserDefaultLocaleName(&mut buffer) };
    // 返回值含结尾的 NUL。
    let length = usize::try_from(length).ok()?.checked_sub(1)?;
    let tag = String::from_utf16(buffer.get(..length)?).ok()?;
    (!tag.is_empty()).then_some(tag)
}

#[cfg(target_os = "macos")]
pub fn system_locale() -> Option<String> {
    use objc2_foundation::NSLocale;

    let languages = NSLocale::preferredLanguages();
    let first = languages.iter().next()?;
    Some(first.to_string())
}

/// 按 POSIX 优先级读取语言，去掉编码与修饰符并转换成 BCP 47。
#[cfg(target_os = "linux")]
pub fn system_locale() -> Option<String> {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|key| std::env::var(key).ok().filter(|value| !value.is_empty()))?;
    let tag = locale.split(['.', '@']).next()?.replace('_', "-");
    (!matches!(tag.as_str(), "C" | "POSIX") && !tag.is_empty()).then_some(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_locale_looks_like_a_language_tag() {
        if let Some(tag) = system_locale() {
            assert!(tag.len() >= 2, "{tag}");
            assert!(
                tag.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            );
        }
    }
}

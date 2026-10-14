//! 本机系统版本：清单的 `requires` 与公告的筛选条件都按它判断。

/// Windows 只关心 build 号（`10.0.26100` 的第三段），macOS 是三段版本号。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsVersion {
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    WindowsBuild(u64),
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Macos((u64, u64, u64)),
}

impl OsVersion {
    /// 公告接口查询参数 `os` 的取值：Windows 填 build 号，macOS 填 `14.6.1`。
    pub fn query_value(self) -> String {
        match self {
            Self::WindowsBuild(build) => build.to_string(),
            Self::Macos((major, minor, patch)) => format!("{major}.{minor}.{patch}"),
        }
    }
}

/// 读本机系统版本；读不出来返回 `None`，带系统要求的更新和公告按不满足处理。
pub fn current() -> Option<OsVersion> {
    // Linux 本地 AppImage 不参与官方更新，不套用 Windows/macOS 系统要求。
    #[cfg(target_os = "linux")]
    {
        None
    }
    #[cfg(target_os = "windows")]
    {
        windows_build().map(OsVersion::WindowsBuild)
    }
    #[cfg(target_os = "macos")]
    {
        macos_version().map(OsVersion::Macos)
    }
}

/// `CurrentBuildNumber` 不受兼容性清单影响，Windows 10 起总是真实的 build 号。
#[cfg(target_os = "windows")]
fn windows_build() -> Option<u64> {
    windows_registry::LOCAL_MACHINE
        .open(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
        .and_then(|key| key.get_string("CurrentBuildNumber"))
        .map_err(|err| log::warn!("read Windows build number failed: {err}"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// `sw_vers -productVersion`，如 `14.6.1`。
#[cfg(target_os = "macos")]
fn macos_version() -> Option<(u64, u64, u64)> {
    let output = std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    parse_macos_version(String::from_utf8(output.stdout).ok()?.trim())
}

/// `10.15`、`14.6.1` 这类点分版本，缺的段按 0 补齐；多于三段或不是数字时返回 `None`。
pub fn parse_macos_version(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |part| part.parse().ok())?;
    let patch = parts.next().map_or(Some(0), |part| part.parse().ok())?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dotted_macos_versions() {
        assert_eq!(parse_macos_version("15.0"), Some((15, 0, 0)));
        assert_eq!(parse_macos_version("10.15.7"), Some((10, 15, 7)));
        assert_eq!(parse_macos_version("11"), Some((11, 0, 0)));
        assert_eq!(parse_macos_version("10.15.7.1"), None);
        assert_eq!(parse_macos_version("ten"), None);
        assert_eq!(OsVersion::Macos((10, 15, 7)).query_value(), "10.15.7");
        assert_eq!(OsVersion::WindowsBuild(26100).query_value(), "26100");
    }

    /// 只读系统信息，不改任何东西。
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn reads_the_running_system() {
        let version = current().expect("system version should be readable");
        if cfg!(target_os = "windows") {
            assert!(matches!(version, OsVersion::WindowsBuild(build) if build >= 10240));
        } else {
            assert!(matches!(version, OsVersion::Macos((major, _, _)) if major >= 10));
        }
    }
}

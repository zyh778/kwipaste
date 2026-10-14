//! 开机自启，与 1.x（`src-tauri/src/autostart`）相同：`auto-launch` crate，启动项名由调用方给
//! （正式版 `KwikPaste`，便携版 `KwikPaste Portable`），参数固定 `--auto-launch`。
//!
//! - Windows：`HKCU\…\Run` 的值 `"<exe>" --auto-launch`，同时把任务管理器的
//!   `StartupApproved\Run` 标成启用。HKLM 里有同名值（老版本或管理员装的）时先尝试删掉；删不掉
//!   （标准用户）就保留它作为唯一的启动项，不再写 HKCU。关闭时只在 HKLM 真有这个值时才去删，
//!   标准用户关闭自启不会因为没有 HKLM 写权限而报错（1.x 会）。
//! - macOS：`~/Library/LaunchAgents/<name>.plist`（`auto-launch` 的 LaunchAgent 模式）。
//!
//! 开发版、自测必须传和正式版不同的名字（由 identifier 派生），否则会改掉本机已安装应用的启动项。

use std::io;
use std::path::Path;

use auto_launch::{AutoLaunch, AutoLaunchBuilder};

/// 自启时带的参数：第二实例带它时静默退出（单实例回调里识别）。
pub const AUTO_LAUNCH_ARG: &str = "--auto-launch";

/// 一个启动项。
pub struct Autostart {
    inner: AutoLaunch,
    name: String,
}

impl Autostart {
    /// `name` 是启动项名（注册表值名 / plist 名），`exe` 是要启动的可执行文件。
    pub fn new(name: &str, exe: &Path) -> io::Result<Self> {
        // AppImage 挂载目录随退出消失，自启必须记录原始 AppImage 路径。
        #[cfg(target_os = "linux")]
        let appimage = std::env::var_os("APPIMAGE").map(std::path::PathBuf::from);
        #[cfg(target_os = "linux")]
        let exe = appimage.as_deref().unwrap_or(exe);
        let exe = exe.to_string_lossy();
        #[cfg(target_os = "windows")]
        let app_path = crate::win::args::quote_arg(&exe);
        #[cfg(target_os = "macos")]
        let app_path = exe.into_owned();
        #[cfg(target_os = "linux")]
        let app_path = desktop_exec_path(&exe)?;

        let mut builder = AutoLaunchBuilder::new();
        builder
            .set_app_name(name)
            .set_app_path(&app_path)
            .set_args(&[AUTO_LAUNCH_ARG]);
        #[cfg(target_os = "windows")]
        builder.set_windows_enable_mode(auto_launch::WindowsEnableMode::CurrentUser);
        let inner = builder.build().map_err(io::Error::other)?;

        Ok(Self {
            inner,
            name: name.to_owned(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// 系统里是否注册了这个启动项（并且没在任务管理器里被禁用）。
    pub fn is_enabled(&self) -> io::Result<bool> {
        self.inner.is_enabled().map_err(io::Error::other)
    }

    /// 打开或关闭。
    pub fn set_enabled(&self, enabled: bool) -> io::Result<()> {
        #[cfg(target_os = "windows")]
        {
            if enabled {
                if crate::win::autostart::keep_system_entry(&self.name)? {
                    return Ok(());
                }
                return self.inner.enable().map_err(io::Error::other);
            }
            crate::win::autostart::remove_entries(&self.name)
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            if enabled {
                self.inner.enable().map_err(io::Error::other)
            } else {
                self.inner.disable().map_err(io::Error::other)
            }
        }
    }
}

/// Desktop Entry 的 Exec 有独立转义规则；双引号保护空格，百分号必须成对。
#[cfg(target_os = "linux")]
fn desktop_exec_path(path: &str) -> io::Result<String> {
    if path.contains(['\n', '\r', '\0']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "autostart path contains a control character",
        ));
    }
    let mut quoted = String::from("\"");
    for ch in path.chars() {
        match ch {
            '%' => quoted.push_str("%%"),
            '\\' => quoted.push_str("\\\\\\\\"),
            '"' | '$' | '`' => {
                quoted.push_str("\\\\");
                quoted.push(ch);
            }
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    Ok(quoted)
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    /// AppImage 路径可以含空格和百分号，不能变成 Desktop Entry 字段或新行。
    #[test]
    fn quotes_desktop_exec_paths() {
        assert_eq!(
            desktop_exec_path("/tmp/My App 10%.AppImage").unwrap(),
            "\"/tmp/My App 10%%.AppImage\""
        );
        assert!(desktop_exec_path("/tmp/app\nExec=bad").is_err());
    }
}

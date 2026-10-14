//! core 要用、只能由平台层提供的能力（[`PlatformServices`]）：前台应用识别、运行中应用枚举、
//! 应用包元信息读取、复制提示音。
//!
//! 从 1.x 的 `src-tauri/src/clipboard/{source,apps_registry,sound}.rs` 移植。应用 id
//! （macOS bundle id / Windows exe 绝对路径）、显示名与报错文案都与 1.x 一致：2.0 打开的是 1.x 的数据库，
//! `clipboard_apps` 里的 id 必须对得上。

use std::path::Path;

use kwikpaste_core::platform::{FrontmostApp, PlatformServices, ScannedApp};
use kwikpaste_core::{AppError, Result};

#[cfg(target_os = "linux")]
use crate::linux::apps as native;
#[cfg(target_os = "macos")]
use crate::mac::apps as native;
#[cfg(target_os = "windows")]
use crate::win::apps as native;

/// [`PlatformServices`] 的本机实现。宿主在 `Core::start` 之后经 `Core::set_platform_services` 接上。
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeServices;

impl PlatformServices for NativeServices {
    fn frontmost_app(&self) -> Option<FrontmostApp> {
        native::frontmost_app()
    }

    fn clipboard_owner(&self) -> Option<std::path::PathBuf> {
        native::clipboard_owner()
    }

    fn process_running(&self, exe_name: &str) -> bool {
        native::process_running(exe_name)
    }

    fn running_apps(&self) -> Vec<ScannedApp> {
        native::running_apps()
    }

    fn app_from_path(&self, path: &Path) -> Result<ScannedApp> {
        if !path.exists() {
            return Err(AppError::Clipboard("app path does not exist".to_owned()));
        }

        native::app_from_path(path)
    }

    fn app_from_id(&self, id: &str) -> Option<ScannedApp> {
        native::app_from_id(id)
    }

    fn play_copy_sound(&self, volume_percent: u8) {
        crate::sound::play_copy(volume_percent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_app_path_is_refused_before_any_platform_check() {
        let missing = std::env::temp_dir().join("kwikpaste-os-missing-app.exe");

        let err = NativeServices.app_from_path(&missing).unwrap_err();

        assert_eq!(err.to_string(), "app path does not exist");
    }
}

//! X11 前台应用归因；稳定标识采用 WM_CLASS，程序路径从 /proc 读取。
use super::x11;
use kwikpaste_core::{
    AppError, Result,
    db::models::Platform,
    platform::{FrontmostApp, ScannedApp},
};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use x11rb::protocol::xproto::ConnectionExt;

/// 从窗口 PID 属性读取可执行文件路径，不执行桌面文件中的 Exec。
fn executable(window: u32) -> Option<PathBuf> {
    let pid = x11::property(window, "_NET_WM_PID")
        .ok()?
        .value32()?
        .next()?;
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

/// 从顶层窗口构造应用记录；窗口标题不作为稳定 ID。
fn describe(window: u32) -> Option<ScannedApp> {
    let reply = x11::property(window, "WM_CLASS").ok()?;
    let mut fields = reply.value.split(|byte| *byte == 0);
    let instance = fields.next().unwrap_or_default();
    let class = fields
        .next()
        .filter(|field| !field.is_empty())
        .unwrap_or(instance);
    let id = String::from_utf8_lossy(class).into_owned();
    if id.is_empty() {
        return None;
    }
    let desktop = app_from_id(&id);
    Some(ScannedApp {
        name: desktop
            .as_ref()
            .map_or_else(|| id.clone(), |app| app.name.clone()),
        path: desktop
            .and_then(|app| app.path)
            .or_else(|| executable(window)),
        id,
        platform: Platform::Linux,
    })
}

/// 剪贴板回调同步捕获前台应用，以免后续后台处理拿到另一个窗口。
pub fn frontmost_app() -> Option<FrontmostApp> {
    let app = describe(x11::active_window()?)?;
    Some(FrontmostApp {
        id: app.id,
        name: app.name,
        platform: app.platform,
        icon_source: app.path,
    })
}

/// X11 selection owner 带 PID 属性时可定位进程；未提供时返回 None。
pub fn clipboard_owner() -> Option<PathBuf> {
    let display = x11::connection().ok()?;
    let selection = x11::atom(&display, "CLIPBOARD").ok()?;
    let owner = display
        .conn
        .get_selection_owner(selection)
        .ok()?
        .reply()
        .ok()?
        .owner;
    executable(owner)
}

/// 扫描 /proc 的进程名，忽略消失或无权读取的条目。
pub fn process_running(exe_name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry.file_name().to_string_lossy().parse::<u32>().is_ok()
            && std::fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|name| name.trim() == exe_name)
    })
}

/// 只枚举 EWMH 客户端窗口所属应用，避免把系统后台服务放进选择列表。
pub fn running_apps() -> Vec<ScannedApp> {
    let Some(reply) = x11::connection()
        .ok()
        .and_then(|display| x11::property(display.root, "_NET_CLIENT_LIST").ok())
    else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    reply
        .value32()
        .into_iter()
        .flatten()
        .filter_map(describe)
        .filter(|app| seen.insert(app.id.clone()))
        .collect()
}

/// 解析 Desktop Entry 主节中的字段；本地化名称后续交由系统图标与语言策略处理。
pub fn app_from_path(path: &Path) -> Result<ScannedApp> {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| AppError::Clipboard("invalid application path".into()))?;
    let mut name = stem.to_owned();
    let mut id = stem.to_owned();
    if path.extension().is_some_and(|ext| ext == "desktop") {
        let text =
            std::fs::read_to_string(path).map_err(|err| AppError::Clipboard(err.to_string()))?;
        let mut main = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                main = line == "[Desktop Entry]";
                continue;
            }
            if !main {
                continue;
            }
            if let Some(value) = line.strip_prefix("Name=") {
                name = value.to_owned();
            }
            if let Some(value) = line
                .strip_prefix("StartupWMClass=")
                .filter(|value| !value.is_empty())
            {
                id = value.to_owned();
            }
        }
    }
    Ok(ScannedApp {
        id,
        name,
        path: Some(path.to_owned()),
        platform: Platform::Linux,
    })
}

/// 按桌面文件名寻找元信息；ID 只能是名称，不能借路径分隔符访问任意文件。
pub fn app_from_id(id: &str) -> Option<ScannedApp> {
    if id.contains(['/', '\\']) || id == ".." || id.is_empty() {
        return None;
    }
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
    {
        dirs.push(home);
    }
    dirs.extend(std::env::split_paths(
        &std::env::var_os("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into()),
    ));
    for dir in dirs {
        for name in [id.to_owned(), id.to_lowercase()] {
            if let Ok(app) =
                app_from_path(&dir.join("applications").join(format!("{name}.desktop")))
            {
                return Some(app);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Desktop Entry 主节才定义应用身份，动作分节不能覆盖来源名称。
    #[test]
    fn desktop_entry_uses_main_section_and_wm_class() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("example.desktop");
        std::fs::write(&path, "[Desktop Entry]\nName=示例编辑器\nStartupWMClass=ExampleEditor\nExec=ignored\n[Desktop Action New]\nName=wrong\n").unwrap();
        let app = app_from_path(&path).unwrap();
        assert_eq!(app.id, "ExampleEditor");
        assert_eq!(app.name, "示例编辑器");
        assert_eq!(app.platform, Platform::Linux);
        assert!(app_from_id("../example").is_none());
    }
}

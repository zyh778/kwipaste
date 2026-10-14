//! Linux 暂不订阅前台全屏状态；保留接口并明确标记此功能降级。
use std::io;

/// TODO: 接入 EWMH 前台变化后向宿主通知暂停状态。
pub fn set_sink(_sink: impl Fn() + Send + Sync + 'static) -> io::Result<()> {
    Ok(())
}

/// TODO: 实现按应用和全屏状态暂停触发；当前不阻止快捷键。
pub fn configure(_fullscreen: bool, _app_ids: Vec<String>) -> io::Result<()> {
    Ok(())
}

/// 未安装暂停观察器时始终允许触发。
pub fn is_paused() -> bool {
    false
}

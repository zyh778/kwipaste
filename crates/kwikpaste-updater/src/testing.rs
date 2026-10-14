//! 测试用的 core：临时目录、自带 runtime、内存剪贴板，不碰本机数据和剪贴板。
//! 用开发环境：统计和公告默认不发，测试不会请求线上地址。

use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use kwikpaste_core::clipboard::MemoryClipboard;
use kwikpaste_core::{AppEnv, AppInfo, Core, CoreOptions, CorePaths, CoreRuntime, NoopSink};

pub(crate) struct TestCore {
    temp: tempfile::TempDir,
    runtime: CoreRuntime,
    pub core: Core,
}

impl TestCore {
    pub(crate) fn start(version: &str) -> Self {
        // 扩展已整体下线（kwikpaste_core::extensions::AVAILABLE = false），
        // 但这里的测试验证的正是装/卸/换版机制，强制打开。
        kwikpaste_core::extensions::FORCE_AVAILABLE.store(true, std::sync::atomic::Ordering::Relaxed);
        let temp = tempfile::tempdir().unwrap();
        let local = temp.path().join("local");
        let paths = CorePaths::new(AppEnv::Dev, local.clone(), local.join("logs"), None);
        let runtime = CoreRuntime::new().unwrap();
        let info = AppInfo {
            name: kwikpaste_core::APP_NAME,
            identifier: kwikpaste_core::APP_IDENTIFIER,
            version: semver::Version::parse(version).unwrap(),
            env: AppEnv::Dev,
        };
        let core = runtime
            .handle()
            .block_on(Core::start(
                info,
                paths,
                CoreOptions::default(),
                Arc::new(NoopSink),
                runtime.handle(),
            ))
            .unwrap();
        core.set_clipboard_provider(Arc::new(MemoryClipboard::new()));

        Self {
            temp,
            runtime,
            core,
        }
    }

    pub(crate) fn root(&self) -> &Path {
        self.temp.path()
    }

    /// 在 core runtime 上跑完一个 future。
    pub(crate) fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.runtime.handle().block_on(future)
    }
}

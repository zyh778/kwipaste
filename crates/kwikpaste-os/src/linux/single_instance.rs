//! Linux 单实例：用户私有 Unix socket 配合文件锁，避免双启动竞争删除活跃 socket。
use crate::single_instance::{Claim, Invocation, current_invocation, decode_nul, encode_nul};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

type Sink = Box<dyn Fn(Invocation) + Send>;

pub struct PrimaryInstance {
    socket: PathBuf,
    _lock: File,
}

impl Drop for PrimaryInstance {
    /// 持锁时清理 socket，随后释放文件锁让下一实例接管。
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// 创建仅当前用户可访问的实例目录，拒绝符号链接和宽松权限。
fn directory() -> io::Result<PathBuf> {
    // SAFETY: geteuid 无参数，不读取调用方内存。
    let uid = unsafe { libc::geteuid() };
    let dir = std::env::temp_dir().join(format!("kwikpaste-{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => (),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => (),
        Err(err) => return Err(err),
    }
    let meta = std::fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe single-instance directory",
        ));
    }
    Ok(dir)
}

/// 独占锁决定主实例，第二实例只发送参数；主实例启动期间短暂重试连接。
pub(crate) fn claim(identifier: &str, sink: Sink) -> io::Result<Claim> {
    let name: String = identifier
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let dir = directory()?;
    let socket = dir.join(format!("{name}.sock"));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(format!("{name}.lock")))?;
    // SAFETY: 锁文件描述符在 guard 生命周期内保持有效。
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::WouldBlock {
            return Err(err);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match forward(&socket) {
                Ok(()) => return Ok(Claim::Forwarded),
                Err(err) if Instant::now() >= deadline => return Err(err),
                Err(_) => thread::sleep(Duration::from_millis(25)),
            }
        }
    }
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    thread::Builder::new()
        .name("single-instance".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut text = String::new();
                if stream.take(1024 * 1024).read_to_string(&mut text).is_ok() {
                    sink(decode_nul(&text));
                }
            }
        })?;
    Ok(Claim::Primary(PrimaryInstance {
        socket,
        _lock: lock,
    }))
}

/// 把启动参数沿用 NUL 协议交给主实例。
fn forward(socket: &Path) -> io::Result<()> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(&encode_nul(&current_invocation()))
}

/// 崩溃重启等待父进程退出，超时仍按普通单实例流程处理。
pub(crate) fn take_over(identifier: &str, sink: Sink, timeout: Duration) -> io::Result<Claim> {
    let parent = std::os::unix::process::parent_id();
    let deadline = Instant::now() + timeout;
    while parent != 1 && std::os::unix::process::parent_id() == parent && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(50));
    }
    claim(identifier, sink)
}

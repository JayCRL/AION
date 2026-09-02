//! Namespace 适配器：Linux namespace 隔离（unshare / setns）。

use std::path::Path;

use async_trait::async_trait;

use crate::{AdapterError, AdapterResult};

// clone/unshare 标志位（与 linux/sched.h 一致，避免依赖平台差异）。
#[cfg(target_os = "linux")]
pub const CLONE_NEWNS: i32 = 0x0002_0000; // mount namespace
#[cfg(target_os = "linux")]
pub const CLONE_NEWCGROUP: i32 = 0x0200_0000;
#[cfg(target_os = "linux")]
pub const CLONE_NEWUTS: i32 = 0x0400_0000;
#[cfg(target_os = "linux")]
pub const CLONE_NEWIPC: i32 = 0x0800_0000;
#[cfg(target_os = "linux")]
pub const CLONE_NEWUSER: i32 = 0x1000_0000;
#[cfg(target_os = "linux")]
pub const CLONE_NEWPID: i32 = 0x2000_0000;
#[cfg(target_os = "linux")]
pub const CLONE_NEWNET: i32 = 0x4000_0000;

/// 需要隔离的 namespace 集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NamespaceSet {
    pub mount: bool,
    pub pid: bool,
    pub net: bool,
    pub ipc: bool,
    pub uts: bool,
    pub user: bool,
    pub cgroup: bool,
}

impl NamespaceSet {
    /// Agent 沙箱默认集合：mount / pid / net / ipc / uts。
    ///
    /// user/cgroup namespace 影响面较大，默认关闭（可通过配置打开）。
    pub fn sandbox_default() -> Self {
        NamespaceSet {
            mount: true,
            pid: true,
            net: true,
            ipc: true,
            uts: true,
            user: false,
            cgroup: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        !(self.mount || self.pid || self.net || self.ipc || self.uts || self.user || self.cgroup)
    }

    pub fn names(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.mount {
            v.push("mnt");
        }
        if self.pid {
            v.push("pid");
        }
        if self.net {
            v.push("net");
        }
        if self.ipc {
            v.push("ipc");
        }
        if self.uts {
            v.push("uts");
        }
        if self.user {
            v.push("user");
        }
        if self.cgroup {
            v.push("cgroup");
        }
        v
    }

    #[cfg(target_os = "linux")]
    pub fn flags(&self) -> i32 {
        let mut flags = 0;
        if self.mount {
            flags |= CLONE_NEWNS;
        }
        if self.pid {
            flags |= CLONE_NEWPID;
        }
        if self.net {
            flags |= CLONE_NEWNET;
        }
        if self.ipc {
            flags |= CLONE_NEWIPC;
        }
        if self.uts {
            flags |= CLONE_NEWUTS;
        }
        if self.user {
            flags |= CLONE_NEWUSER;
        }
        if self.cgroup {
            flags |= CLONE_NEWCGROUP;
        }
        flags
    }
}

/// Namespace 适配器 trait。
#[async_trait]
pub trait NamespaceAdapter: Send + Sync {
    /// 当前平台是否支持 namespace 隔离。
    fn supported(&self) -> bool;

    /// 对当前线程/进程执行 unshare（仅应在 fork 后、exec 前调用）。
    async fn unshare_current(&self, set: NamespaceSet) -> AdapterResult<()>;

    /// 加入已有 namespace（`/proc/<pid>/ns/<type>`），`nstype` 为 CLONE_NEW* 常量。
    async fn setns_current(&self, path: &Path, nstype: i32) -> AdapterResult<()>;
}

/// 平台原生实现。
pub struct NativeNamespaceAdapter;

#[async_trait]
impl NamespaceAdapter for NativeNamespaceAdapter {
    fn supported(&self) -> bool {
        cfg!(target_os = "linux")
    }

    async fn unshare_current(&self, set: NamespaceSet) -> AdapterResult<()> {
        #[cfg(target_os = "linux")]
        {
            let flags = set.flags();
            if flags == 0 {
                return Ok(());
            }
            unshare_raw(flags)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = set;
            Err(AdapterError::Unsupported(
                "namespace isolation requires Linux".into(),
            ))
        }
    }

    async fn setns_current(&self, path: &Path, nstype: i32) -> AdapterResult<()> {
        #[cfg(target_os = "linux")]
        {
            setns_raw(path, nstype)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (path, nstype);
            Err(AdapterError::Unsupported(
                "setns requires Linux".into(),
            ))
        }
    }
}

/// 同步版 unshare，供 `pre_exec`（fork 后、exec 前）调用。
#[cfg(target_os = "linux")]
pub fn unshare_raw(flags: i32) -> AdapterResult<()> {
    if flags == 0 {
        return Ok(());
    }
    // SAFETY: unshare 是标准系统调用；flags 合法性由内核校验。
    let rc = unsafe { libc::unshare(flags) };
    if rc != 0 {
        return Err(AdapterError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

/// 同步版 setns。
#[cfg(target_os = "linux")]
pub fn setns_raw(path: &Path, nstype: i32) -> AdapterResult<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| AdapterError::Other("namespace path contains NUL".into()))?;
    // SAFETY: 路径来自调用方，fd 由本函数负责关闭。
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(AdapterError::Io(std::io::Error::last_os_error()));
    }
    let rc = unsafe { libc::setns(fd, nstype) };
    let errno = std::io::Error::last_os_error();
    unsafe { libc::close(fd) };
    if rc != 0 {
        return Err(AdapterError::Io(errno));
    }
    Ok(())
}

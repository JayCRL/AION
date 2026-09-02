//! # AION 系统适配层 — Linux Adapter
//!
//! 封装 Linux 系统调用，向上层（AION System Services）提供统一接口：
//!
//! | 适配器 | 职责 | Linux 实现 | 非 Linux 平台 |
//! |--------|------|-----------|---------------|
//! | [`process`]  | clone / execve / wait | tokio + `pre_exec`（unshare/cgroup/seccomp/caps） | tokio（沙箱不强制） |
//! | [`fs`]       | open / read / write / mount | tokio::fs + `mount(2)` | tokio::fs |
//! | [`net`]      | socket / connect / bind | tokio::net | tokio::net |
//! | [`cgroup`]   | cgroup v2 资源控制 | 直接读写 cgroupfs | 内存模拟 |
//! | [`namespace`]| namespace 隔离 | `unshare(2)` / `setns(2)` | 返回 Unsupported |
//! | [`seccomp`]  | seccomp 系统调用过滤 | BPF + `prctl(PR_SET_SECCOMP)` | 返回 Unsupported |
//! | [`device`]   | 设备权限 / 访问 | /dev 扫描 + 权限位 | 目录扫描模拟 |
//! | [`capability`]| 权限 / Capability | `prctl(PR_CAPBSET_DROP)` | 仅策略计算 |
//!
//! 通过 [`AdapterKit::native`] 取得平台默认组合。

pub mod capability;
pub mod cgroup;
pub mod device;
pub mod fs;
pub mod namespace;
pub mod net;
pub mod process;
pub mod sandbox;
pub mod seccomp;

// 常用类型在 crate 根重导出，方便上层使用
pub use capability::CapabilitySet;
pub use cgroup::{CgroupHandle, CgroupLimits};
pub use fs::{DirEntryInfo, FileMeta};
pub use namespace::NamespaceSet;
pub use process::{ProcessSpec, SpawnedProcess, StreamMode};
pub use sandbox::{SandboxProfile, SandboxSupport};
pub use seccomp::{SeccompDefault, SeccompPolicy};

use std::path::PathBuf;
use std::sync::Arc;

/// 适配层错误。
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("not supported on this platform: {0}")]
    Unsupported(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub type AdapterResult<T> = Result<T, AdapterError>;

/// 装箱 Future 别名。
pub type BoxFut<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>;

/// 平台原生适配器组合。
///
/// - Linux：cgroup/namespace/seccomp/capability 均为真实实现；
/// - 其他平台：cgroup 走内存模拟，namespace/seccomp/capability 报告不支持，
///   进程沙箱不被强制执行（由上层通过 `SpawnedProcess::sandboxed` 得知）。
#[derive(Clone)]
pub struct AdapterKit {
    pub process: Arc<dyn process::ProcessAdapter>,
    pub fs: Arc<dyn fs::FsAdapter>,
    pub net: Arc<dyn net::NetAdapter>,
    pub cgroup: Arc<dyn cgroup::CgroupAdapter>,
    pub namespace: Arc<dyn namespace::NamespaceAdapter>,
    pub seccomp: Arc<dyn seccomp::SeccompAdapter>,
    pub device: Arc<dyn device::DeviceAdapter>,
    pub capability: Arc<dyn capability::CapabilityAdapter>,
}

impl std::fmt::Debug for AdapterKit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdapterKit")
            .field("platform", &std::env::consts::OS)
            .field(
                "sandbox_capable",
                &(
                    cfg!(target_os = "linux"),
                    !self.cgroup.is_emulated(),
                    self.seccomp.supported(),
                    self.namespace.supported(),
                ),
            )
            .finish()
    }
}

impl AdapterKit {
    /// 构建平台原生适配器组合。
    ///
    /// `cgroup_root`：cgroup v2 挂载点（Linux 上通常为 `/sys/fs/cgroup`）。
    pub fn native(cgroup_root: PathBuf) -> Self {
        AdapterKit {
            process: Arc::new(process::NativeProcessAdapter),
            fs: Arc::new(fs::NativeFsAdapter),
            net: Arc::new(net::NativeNetAdapter),
            cgroup: if cfg!(target_os = "linux") {
                Arc::new(cgroup::NativeCgroupAdapter::new(cgroup_root))
            } else {
                Arc::new(cgroup::EmulatedCgroupAdapter::default())
            },
            namespace: Arc::new(namespace::NativeNamespaceAdapter),
            seccomp: Arc::new(seccomp::NativeSeccompAdapter),
            device: Arc::new(device::NativeDeviceAdapter),
            capability: Arc::new(capability::NativeCapabilityAdapter),
        }
    }

    /// 当前平台的沙箱能力报告。
    pub fn sandbox_support(&self) -> sandbox::SandboxSupport {
        sandbox::SandboxSupport {
            namespaces: self.namespace.supported(),
            cgroup_enforced: !self.cgroup.is_emulated(),
            seccomp: self.seccomp.supported(),
            capabilities: self.capability.supported(),
        }
    }
}

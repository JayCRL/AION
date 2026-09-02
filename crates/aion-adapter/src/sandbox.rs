//! 沙箱配置：namespace + cgroup + seccomp + capability 的组合描述。

use crate::capability::CapabilitySet;
use crate::cgroup::CgroupLimits;
use crate::namespace::NamespaceSet;
use crate::seccomp::SeccompPolicy;

/// 沙箱档案：描述一个 Agent 进程应处的隔离环境。
#[derive(Debug, Clone, Default)]
pub struct SandboxProfile {
    pub namespaces: NamespaceSet,
    pub cgroup: Option<CgroupLimits>,
    pub seccomp: Option<SeccompPolicy>,
    /// 进程保留的 capability（其余全部从 bounding set 移除）。
    pub capabilities: CapabilitySet,
    /// 设置 PR_SET_NO_NEW_PRIVS。
    pub no_new_privs: bool,
}

impl SandboxProfile {
    /// 严格沙箱：mount/pid/net/ipc/uts namespace + 资源限制 +
    /// seccomp 白名单 + 最小 capability。
    pub fn strict() -> Self {
        SandboxProfile {
            namespaces: NamespaceSet::sandbox_default(),
            cgroup: Some(
                CgroupLimits::new()
                    .memory_max(512 * 1024 * 1024)
                    .cpu_max(100_000, 100_000)
                    .pids_max(128),
            ),
            seccomp: Some(SeccompPolicy::default_allowlist()),
            capabilities: CapabilitySet::minimal(),
            no_new_privs: true,
        }
    }

    /// 宽松沙箱：只做资源限制与 no_new_privs，不做 namespace/seccomp。
    pub fn relaxed() -> Self {
        SandboxProfile {
            namespaces: NamespaceSet::default(),
            cgroup: Some(CgroupLimits::new().memory_max(1024 * 1024 * 1024)),
            seccomp: None,
            capabilities: CapabilitySet::all(),
            no_new_privs: true,
        }
    }
}

/// 平台沙箱能力报告（可观测性）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SandboxSupport {
    pub namespaces: bool,
    pub cgroup_enforced: bool,
    pub seccomp: bool,
    pub capabilities: bool,
}

impl SandboxSupport {
    /// 是否能完整执行沙箱档案。
    pub fn full_enforcement(&self) -> bool {
        self.namespaces && self.cgroup_enforced && self.seccomp && self.capabilities
    }

    /// 人可读的能力摘要（用于展示）。
    pub fn summary(&self) -> String {
        let flag = |b: bool| if b { "✓" } else { "✗" };
        format!(
            "namespace {} · cgroup {} · seccomp {} · capability {}",
            flag(self.namespaces),
            flag(self.cgroup_enforced),
            flag(self.seccomp),
            flag(self.capabilities)
        )
    }
}

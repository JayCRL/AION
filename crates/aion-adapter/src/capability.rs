//! Capability 适配器：Linux capability 权限管理（最小权限原则）。

use async_trait::async_trait;

use crate::{AdapterError, AdapterResult};

/// Linux capability 名称表（索引即 capability 位，0..=40）。
pub const CAP_NAMES: [&str; 41] = [
    "CHOWN", // 0
    "DAC_OVERRIDE",
    "DAC_READ_SEARCH",
    "FOWNER",
    "FSETID",
    "KILL",
    "SETGID",
    "SETUID",
    "SETPCAP",
    "LINUX_IMMUTABLE", // 9
    "NET_BIND_SERVICE",
    "NET_BROADCAST",
    "NET_ADMIN",
    "NET_RAW",
    "IPC_LOCK",
    "IPC_OWNER",
    "SYS_MODULE",
    "SYS_RAWIO",
    "SYS_CHROOT",
    "SYS_PTRACE", // 19
    "SYS_PACCT",
    "SYS_ADMIN",
    "SYS_BOOT",
    "SYS_NICE",
    "SYS_RESOURCE",
    "SYS_TIME",
    "SYS_TTY_CONFIG",
    "MKNOD",
    "LEASE",
    "AUDIT_WRITE", // 29
    "AUDIT_CONTROL",
    "SETFCAP",
    "MAC_OVERRIDE",
    "MAC_ADMIN",
    "SYSLOG",
    "WAKE_ALARM",
    "BLOCK_SUSPEND",
    "AUDIT_READ",
    "PERFMON", // 38
    "BPF",
    "CHECKPOINT_RESTORE", // 40
];

/// Capability 位集合。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CapabilitySet(pub u64);

impl CapabilitySet {
    pub fn none() -> Self {
        CapabilitySet(0)
    }

    pub fn all() -> Self {
        CapabilitySet(u64::MAX)
    }

    pub fn has(&self, bit: u8) -> bool {
        bit < 64 && (self.0 >> bit) & 1 == 1
    }

    pub fn add(&mut self, bit: u8) {
        if bit < 64 {
            self.0 |= 1u64 << bit;
        }
    }

    pub fn union(&self, other: CapabilitySet) -> CapabilitySet {
        CapabilitySet(self.0 | other.0)
    }

    /// 按 capability 名构造；未知名称报错。
    pub fn from_names(names: &[&str]) -> AdapterResult<Self> {
        let mut set = CapabilitySet::none();
        for name in names {
            let bit = bit_for_name(name).ok_or_else(|| {
                AdapterError::Other(format!("unknown capability `{name}`"))
            })?;
            set.add(bit);
        }
        Ok(set)
    }

    /// 展开为 capability 名列表。
    pub fn to_names(&self) -> Vec<&'static str> {
        CAP_NAMES
            .iter()
            .enumerate()
            .filter(|(i, _)| self.has(*i as u8))
            .map(|(_, name)| *name)
            .collect()
    }

    /// 沙箱内进程的最小 capability 集（最小权限原则）。
    pub fn minimal() -> Self {
        // CHOWN/DAC_OVERRIDE: 常规文件操作兜底；SETUID/SETGID: 降权运行；
        // NET_BIND_SERVICE: 绑定低位端口；MKNOD: 建立 /dev 节点。
        Self::from_names(&["CHOWN", "DAC_OVERRIDE", "SETGID", "SETUID", "NET_BIND_SERVICE", "MKNOD"])
            .unwrap_or_default()
    }
}

/// 按名称查 capability 位。
pub fn bit_for_name(name: &str) -> Option<u8> {
    CAP_NAMES
        .iter()
        .position(|n| n.eq_ignore_ascii_case(name))
        .map(|i| i as u8)
}

/// Capability 适配器 trait。
#[async_trait]
pub trait CapabilityAdapter: Send + Sync {
    /// 当前平台是否支持运行时收缩权限。
    fn supported(&self) -> bool;

    /// 将当前进程的 capability bounding set 收缩为 `keep`（仅 Linux）。
    async fn restrict_current(&self, keep: CapabilitySet) -> AdapterResult<()>;
}

/// 平台原生实现。
pub struct NativeCapabilityAdapter;

#[async_trait]
impl CapabilityAdapter for NativeCapabilityAdapter {
    fn supported(&self) -> bool {
        cfg!(target_os = "linux")
    }

    async fn restrict_current(&self, keep: CapabilitySet) -> AdapterResult<()> {
        #[cfg(target_os = "linux")]
        {
            restrict_bounding(keep)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = keep;
            Err(AdapterError::Unsupported(
                "capability restriction requires Linux".into(),
            ))
        }
    }
}

/// 同步收缩 bounding set，供 `pre_exec` 调用。
#[cfg(target_os = "linux")]
pub fn restrict_bounding(keep: CapabilitySet) -> AdapterResult<()> {
    const PR_CAPBSET_DROP: i32 = 24;
    for bit in 0u8..CAP_NAMES.len() as u8 {
        if keep.has(bit) {
            continue;
        }
        // SAFETY: prctl(PR_CAPBSET_DROP, bit) 是合法的权限收缩操作。
        let rc = unsafe { libc::prctl(PR_CAPBSET_DROP, bit as libc::c_ulong, 0, 0, 0) };
        if rc != 0 {
            // 某些 capability 在旧内核上不存在，ENOSCR/EINVAL 时跳过
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINVAL) {
                continue;
            }
            return Err(AdapterError::Io(err));
        }
    }
    Ok(())
}

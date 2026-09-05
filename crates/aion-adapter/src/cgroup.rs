//! Cgroup 适配器：cgroup v2 资源控制（内存 / CPU / 进程数）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;

use crate::{AdapterError, AdapterResult};

/// cgroup 资源限制。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CgroupLimits {
    /// memory.max（字节）。
    pub memory_max_bytes: Option<u64>,
    /// cpu.max 配额（微秒 / 周期微秒），quota 为 None 表示不限。
    pub cpu_quota_us: Option<u64>,
    pub cpu_period_us: Option<u64>,
    /// pids.max。
    pub pids_max: Option<i64>,
}

impl CgroupLimits {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn memory_max(mut self, bytes: u64) -> Self {
        self.memory_max_bytes = Some(bytes);
        self
    }

    pub fn cpu_max(mut self, quota_us: u64, period_us: u64) -> Self {
        self.cpu_quota_us = Some(quota_us);
        self.cpu_period_us = Some(period_us.max(1000));
        self
    }

    pub fn pids_max(mut self, n: i64) -> Self {
        self.pids_max = Some(n);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.memory_max_bytes.is_none()
            && self.cpu_quota_us.is_none()
            && self.pids_max.is_none()
    }
}

/// 已创建的 cgroup 句柄。
#[derive(Debug, Clone)]
pub struct CgroupHandle {
    pub name: String,
    /// cgroup 目录（Linux 上为 cgroupfs 路径；模拟模式无实际意义）。
    pub path: PathBuf,
    /// 是否为模拟句柄（非 Linux 平台）。
    pub emulated: bool,
}

/// Cgroup 适配器 trait。
#[async_trait]
pub trait CgroupAdapter: Send + Sync {
    /// 是否为模拟实现（不实际限制资源）。
    fn is_emulated(&self) -> bool;

    /// 创建 cgroup 并写入限制。
    async fn create(&self, name: &str, limits: &CgroupLimits) -> AdapterResult<CgroupHandle>;

    /// 将进程附加到 cgroup。
    async fn attach(&self, cg: &CgroupHandle, pid: u32) -> AdapterResult<()>;

    /// 销毁 cgroup。
    async fn destroy(&self, cg: &CgroupHandle) -> AdapterResult<()>;

    /// 读取 cgroup 统计（memory.current / pids.current / cpu.stat 摘要）。
    async fn stats(&self, cg: &CgroupHandle) -> AdapterResult<BTreeMap<String, u64>>;
}

// ---------------------------------------------------------------------------
// Linux 原生实现
// ---------------------------------------------------------------------------

/// cgroup v2 原生适配器。`root` 通常为 `/sys/fs/cgroup`。
pub struct NativeCgroupAdapter {
    root: PathBuf,
}

impl NativeCgroupAdapter {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        NativeCgroupAdapter { root: root.into() }
    }

    /// cgroup v2 是否对本进程**真实可用**（能建组实施资源限制）。
    ///
    /// 判据：`root` 是目录、带 cgroup v2 特征文件 `cgroup.controllers`（借此排除
    /// cgroup v1 挂载点与不存在的情形），并且能在 `root` 下**实际建/删一个探针子目录**
    /// ——验证当前用户对该树有写权限（root 运行、systemd 式 delegate，或挂载点放权）。
    ///
    /// 不满足的典型场景：容器（/sys/fs/cgroup 只读或未 delegate）、无特权普通用户桌面
    /// （root 树归 root 不可写）、cgroup v1 机器。上层应据此把资源限制降级为
    /// [`EmulatedCgroupAdapter`]（记录限制但**不假装强制**），而不是每次 spawn 都尝试
    /// 失败告警。只读探测 + 一次建删，不残留。
    #[cfg(target_os = "linux")]
    pub fn usable(root: &Path) -> bool {
        if !root.is_dir() {
            return false;
        }
        // cgroup v1 的挂载根（/sys/fs/cgroup）没有这个特征文件；有它才是 v2。
        if !root.join("cgroup.controllers").is_file() {
            return false;
        }
        // 真实建组要求 root 树可写。建一个唯一探针目录再删掉：能建 = 可用。
        // pid 参与命名，避免与本机其它 AION 实例的探针互相打架。
        let probe = root.join(format!(".aion-cg-probe-{}", std::process::id()));
        match std::fs::create_dir(&probe) {
            Ok(()) => {
                let _ = std::fs::remove_dir(&probe);
                true
            }
            Err(_) => false,
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn usable(_root: &Path) -> bool {
        false
    }

    #[allow(dead_code)]
    fn group_path(&self, name: &str) -> PathBuf {
        self.root.join(format!("aion.{name}"))
    }

    #[allow(dead_code)]
    async fn write_file(&self, path: &PathBuf, content: &str) -> AdapterResult<()> {
        tokio::fs::write(path, content.as_bytes())
            .await
            .map_err(|e| AdapterError::Other(format!("write {}: {e}", path.display())))
    }

    #[cfg(target_os = "linux")]
    async fn create_linux(&self, name: &str, limits: &CgroupLimits) -> AdapterResult<CgroupHandle> {
        let path = self.group_path(name);
        tokio::fs::create_dir_all(&path).await?;
        // 在父层开启要下放的控制器（失败不致命：可能是纯用户态挂载点）
        if !limits.is_empty() {
            let _ = tokio::fs::write(
                self.root.join("cgroup.subtree_control"),
                b"+memory +cpu +pids",
            )
            .await;
        }
        if let Some(bytes) = limits.memory_max_bytes {
            self.write_file(&path.join("memory.max"), &format!("{bytes}")).await?;
        }
        if limits.cpu_quota_us.is_some() || limits.cpu_period_us.is_some() {
            let quota = limits.cpu_quota_us.map(|q| q.to_string()).unwrap_or_else(|| "max".into());
            let period = limits.cpu_period_us.unwrap_or(100_000);
            self.write_file(&path.join("cpu.max"), &format!("{quota} {period}")).await?;
        }
        if let Some(pids) = limits.pids_max {
            self.write_file(&path.join("pids.max"), &format!("{pids}")).await?;
        }
        Ok(CgroupHandle {
            name: name.to_string(),
            path,
            emulated: false,
        })
    }

    #[cfg(target_os = "linux")]
    async fn stats_linux(&self, cg: &CgroupHandle) -> AdapterResult<BTreeMap<String, u64>> {
        let mut out = BTreeMap::new();
        for key in ["memory.current", "pids.current", "memory.peak"] {
            if let Ok(text) = tokio::fs::read_to_string(cg.path.join(key)).await {
                if let Ok(v) = text.trim().parse::<u64>() {
                    out.insert(key.to_string(), v);
                }
            }
        }
        if let Ok(stat) = tokio::fs::read_to_string(cg.path.join("cpu.stat")).await {
            for line in stat.lines() {
                let mut parts = line.split_whitespace();
                if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                    if let Ok(v) = v.parse::<u64>() {
                        out.insert(format!("cpu.{k}"), v);
                    }
                }
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl CgroupAdapter for NativeCgroupAdapter {
    fn is_emulated(&self) -> bool {
        !cfg!(target_os = "linux")
    }

    async fn create(&self, name: &str, limits: &CgroupLimits) -> AdapterResult<CgroupHandle> {
        #[cfg(target_os = "linux")]
        {
            self.create_linux(name, limits).await
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (name, limits);
            Err(AdapterError::Unsupported(
                "cgroup v2 requires Linux".into(),
            ))
        }
    }

    async fn attach(&self, cg: &CgroupHandle, pid: u32) -> AdapterResult<()> {
        #[cfg(target_os = "linux")]
        {
            if cg.emulated {
                return Ok(());
            }
            self.write_file(&cg.path.join("cgroup.procs"), &format!("{pid}"))
                .await
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (cg, pid);
            Err(AdapterError::Unsupported(
                "cgroup v2 requires Linux".into(),
            ))
        }
    }

    async fn destroy(&self, cg: &CgroupHandle) -> AdapterResult<()> {
        #[cfg(target_os = "linux")]
        {
            if cg.emulated {
                return Ok(());
            }
            match tokio::fs::remove_dir(&cg.path).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(AdapterError::Other(format!(
                    "destroy cgroup {}: {e}（可能仍有进程存活）",
                    cg.path.display()
                ))),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = cg;
            Err(AdapterError::Unsupported(
                "cgroup v2 requires Linux".into(),
            ))
        }
    }

    async fn stats(&self, cg: &CgroupHandle) -> AdapterResult<BTreeMap<String, u64>> {
        #[cfg(target_os = "linux")]
        {
            if cg.emulated {
                return Ok(BTreeMap::new());
            }
            self.stats_linux(cg).await
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = cg;
            Err(AdapterError::Unsupported(
                "cgroup v2 requires Linux".into(),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// 模拟实现（非 Linux 平台 / 无根权限环境）
// ---------------------------------------------------------------------------

/// 内存模拟适配器：记录句柄但不实际限制资源。
///
/// 用于非 Linux 平台，以及 Linux 上 `NativeCgroupAdapter::usable` 判定 cgroup v2
/// 不可写时的降级（容器 / 无 delegate 的普通用户 / cgroup v1）——资源限制尽力而为，
/// 由 `SandboxSupport` 如实上报 cgroup 未强制。
#[derive(Default)]
pub struct EmulatedCgroupAdapter {
    groups: Mutex<std::collections::BTreeMap<String, CgroupLimits>>,
}

impl EmulatedCgroupAdapter {
    pub fn limits(&self, name: &str) -> Option<CgroupLimits> {
        self.groups
            .lock()
            .expect("cgroup map poisoned")
            .get(name)
            .cloned()
    }
}

#[async_trait]
impl CgroupAdapter for EmulatedCgroupAdapter {
    fn is_emulated(&self) -> bool {
        true
    }

    async fn create(&self, name: &str, limits: &CgroupLimits) -> AdapterResult<CgroupHandle> {
        self.groups
            .lock()
            .expect("cgroup map poisoned")
            .insert(name.to_string(), limits.clone());
        Ok(CgroupHandle {
            name: name.to_string(),
            path: PathBuf::from(format!("/sys/fs/cgroup-emulated/aion.{name}")),
            emulated: true,
        })
    }

    async fn attach(&self, _cg: &CgroupHandle, _pid: u32) -> AdapterResult<()> {
        Ok(()) // 模拟模式下不实施限制
    }

    async fn destroy(&self, cg: &CgroupHandle) -> AdapterResult<()> {
        self.groups
            .lock()
            .expect("cgroup map poisoned")
            .remove(&cg.name);
        Ok(())
    }

    async fn stats(&self, _cg: &CgroupHandle) -> AdapterResult<BTreeMap<String, u64>> {
        Ok(BTreeMap::new())
    }
}

//! 安全上下文：Agent 的权限 & Capability 检查模型。
//!
//! 落实「最小权限」原则：每个 Agent 携带自己的 [`SecurityContext`]，
//! 服务在调用 Linux Adapter 之前先检查 capability / 路径 / 网络白名单，
//! 形成 Agent → Context → Service → Kernel 的多级隔离。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::{AionError, AionResult};

/// 一次（Agent 视角的）操作授权上下文。
#[derive(Debug, Clone, Default)]
pub struct SecurityContext {
    pub agent: String,
    /// 授予的 capability（`*` 表示全部，仅用于开发/演示）。
    pub caps: BTreeSet<String>,
    /// 允许读写的文件系统根目录。
    pub fs_roots: Vec<PathBuf>,
    /// 允许访问的网络目标（`*` / `host` / `host:port`）。
    pub net_allow: Vec<String>,
    /// 最大并发进程数（0 = 不限）。
    pub max_processes: u32,
    /// 是否强制要求沙箱执行。
    pub require_sandbox: bool,
}

impl SecurityContext {
    pub fn new(agent: impl Into<String>) -> Self {
        SecurityContext {
            agent: agent.into(),
            ..Default::default()
        }
    }

    /// 授予一个 capability。
    pub fn allow(mut self, cap: impl Into<String>) -> Self {
        self.caps.insert(cap.into());
        self
    }

    /// 授予全部 capability（仅限开发/演示环境）。
    pub fn allow_all(mut self) -> Self {
        self.caps.insert("*".into());
        self
    }

    /// 批量授予。
    pub fn allow_list(mut self, caps: &[&str]) -> Self {
        for c in caps {
            self.caps.insert(c.to_string());
        }
        self
    }

    /// 追加允许的文件系统根目录。
    pub fn root(mut self, path: impl Into<PathBuf>) -> Self {
        self.fs_roots.push(path.into());
        self
    }

    /// 追加允许的网络目标。
    pub fn net(mut self, target: impl Into<String>) -> Self {
        self.net_allow.push(target.into());
        self
    }

    pub fn max_processes(mut self, n: u32) -> Self {
        self.max_processes = n;
        self
    }

    pub fn require_sandbox(mut self, required: bool) -> Self {
        self.require_sandbox = required;
        self
    }

    /// 检查是否持有 capability。
    pub fn check_cap(&self, cap: &str) -> AionResult<()> {
        if self.caps.contains("*") || self.caps.contains(cap) {
            Ok(())
        } else {
            Err(AionError::PermissionDenied(cap.to_string()))
        }
    }

    /// 检查路径是否落在白名单根目录内，返回规范化后的绝对路径。
    pub fn check_path(&self, path: &Path, write: bool) -> AionResult<PathBuf> {
        let _ = write;
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            let cwd = std::env::current_dir()?;
            cwd.join(path)
        };
        let norm = normalize(&abs);
        let canon = match std::fs::canonicalize(&norm) {
            Ok(p) => p,
            Err(_) => {
                // 目标可能尚不存在（写场景）：规范化其父目录
                let parent = norm
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."));
                match std::fs::canonicalize(&parent) {
                    Ok(p) => p.join(norm.file_name().unwrap_or_default()),
                    Err(_) => norm.clone(),
                }
            }
        };
        for root in &self.fs_roots {
            let root_canon = std::fs::canonicalize(root).unwrap_or_else(|_| normalize(root));
            if canon.starts_with(&root_canon) {
                return Ok(canon);
            }
        }
        Err(AionError::PathDenied(canon))
    }

    /// 检查网络目标是否在白名单内。
    pub fn check_net(&self, host: &str, port: u16) -> AionResult<()> {
        for entry in &self.net_allow {
            if entry == "*" {
                return Ok(());
            }
            if let Some((h, p)) = entry.rsplit_once(':') {
                if h.eq_ignore_ascii_case(host)
                    && p.parse::<u16>().map(|pp| pp == port).unwrap_or(false)
                {
                    return Ok(());
                }
            } else if entry.eq_ignore_ascii_case(host) {
                return Ok(());
            }
        }
        Err(AionError::NetDenied(format!("{host}:{port}")))
    }

    /// 沙箱检查：要求强制沙箱时，确认上次执行确实被隔离。
    pub fn check_sandbox(&self, sandboxed: bool) -> AionResult<()> {
        if self.require_sandbox && !sandboxed {
            Err(AionError::Other(
                "sandbox required but platform cannot enforce it".into(),
            ))
        } else {
            Ok(())
        }
    }
}

/// 词法规范化路径（消解 `.` 与 `..`，不触碰文件系统）。
fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_check() {
        let sec = SecurityContext::new("a").allow("fs:read");
        assert!(sec.check_cap("fs:read").is_ok());
        assert!(sec.check_cap("fs:write").is_err());
        assert!(SecurityContext::new("b").allow_all().check_cap("anything").is_ok());
    }

    #[test]
    fn path_escape_denied() {
        let dir = std::env::temp_dir().join(format!("aion-sec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sec = SecurityContext::new("a").root(&dir);
        assert!(sec.check_path(&dir.join("ok.txt"), true).is_ok());
        assert!(sec.check_path(&dir.join("..").join("escape.txt"), true).is_err());
        assert!(sec.check_path(Path::new("/etc/passwd"), false).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn net_allowlist() {
        let sec = SecurityContext::new("a").net("example.com").net("api.test:8080");
        assert!(sec.check_net("example.com", 80).is_ok());
        assert!(sec.check_net("api.test", 8080).is_ok());
        assert!(sec.check_net("api.test", 9090).is_err());
        assert!(sec.check_net("evil.com", 80).is_err());
        assert!(SecurityContext::new("b").net("*").check_net("any", 1).is_ok());
    }
}

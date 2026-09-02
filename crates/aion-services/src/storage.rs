//! StorageService：存储管理。租户目录 + 字节配额。

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{AionError, AionResult};
use crate::security::SecurityContext;

/// 租户配额信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageQuota {
    pub tenant: String,
    pub max_bytes: u64,
    pub used_bytes: u64,
}

const QUOTA_FILE: &str = ".aion-quota.json";

/// 存储管理服务。
pub struct StorageService {
    root: PathBuf,
}

impl StorageService {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        StorageService { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn tenant_dir(&self, tenant: &str) -> AionResult<PathBuf> {
        if tenant.is_empty()
            || !tenant
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(AionError::Other(format!("invalid tenant name `{tenant}`")));
        }
        Ok(self.root.join(tenant))
    }

    fn load_quota(&self, dir: &Path) -> AionResult<u64> {
        let path = dir.join(QUOTA_FILE);
        if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            let quota: StorageQuota = serde_json::from_str(&text)
                .map_err(|e| AionError::Other(format!("corrupt quota file: {e}")))?;
            Ok(quota.max_bytes)
        } else {
            Ok(0)
        }
    }

    /// 为租户分配存储目录并设置配额。
    pub async fn allocate(
        &self,
        sec: &SecurityContext,
        tenant: &str,
        max_bytes: u64,
    ) -> AionResult<StorageQuota> {
        sec.check_cap("storage:allocate")?;
        let dir = self.tenant_dir(tenant)?;
        tokio::fs::create_dir_all(&dir).await?;
        let quota = StorageQuota {
            tenant: tenant.to_string(),
            max_bytes,
            used_bytes: dir_size(&dir).await,
        };
        tokio::fs::write(
            dir.join(QUOTA_FILE),
            serde_json::to_string_pretty(&quota)?.as_bytes(),
        )
        .await?;
        Ok(quota)
    }

    /// 在租户目录内写文件（超出配额拒绝）。
    pub async fn write_file(
        &self,
        sec: &SecurityContext,
        tenant: &str,
        rel_path: &str,
        data: &[u8],
    ) -> AionResult<PathBuf> {
        sec.check_cap("storage:write")?;
        let dir = self.tenant_dir(tenant)?;
        if !dir.exists() {
            return Err(AionError::Other(format!(
                "tenant `{tenant}` not allocated"
            )));
        }
        // rel_path 不得逃逸租户目录
        let target = dir.join(rel_path);
        let canonical_parent = match target.parent().map(Path::to_path_buf) {
            Some(p) => std::fs::canonicalize(&p).unwrap_or(p),
            None => dir.clone(),
        };
        let dir_canon = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if !canonical_parent.starts_with(&dir_canon) {
            return Err(AionError::Other(format!(
                "path `{rel_path}` escapes tenant directory"
            )));
        }

        let max = self.load_quota(&dir)?;
        if max > 0 {
            let used = dir_size(&dir).await;
            if used + data.len() as u64 > max {
                return Err(AionError::Limit(format!(
                    "tenant `{tenant}` quota exceeded: {used} + {} > {max}",
                    data.len()
                )));
            }
        }

        tokio::fs::create_dir_all(&canonical_parent).await?;
        tokio::fs::write(&target, data).await?;
        Ok(target)
    }

    /// 读取租户文件。
    pub async fn read_file(
        &self,
        sec: &SecurityContext,
        tenant: &str,
        rel_path: &str,
    ) -> AionResult<Vec<u8>> {
        sec.check_cap("storage:read")?;
        let dir = self.tenant_dir(tenant)?;
        let target = dir.join(rel_path);
        let dir_canon = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        let parent = target
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| dir.clone());
        let parent_canon = std::fs::canonicalize(&parent).unwrap_or(parent);
        if !parent_canon.starts_with(&dir_canon) {
            return Err(AionError::Other("path escapes tenant directory".into()));
        }
        Ok(tokio::fs::read(&target).await?)
    }

    /// 查询租户用量。
    pub async fn usage(&self, sec: &SecurityContext, tenant: &str) -> AionResult<StorageQuota> {
        sec.check_cap("storage:use")?;
        let dir = self.tenant_dir(tenant)?;
        if !dir.exists() {
            return Err(AionError::Other(format!(
                "tenant `{tenant}` not allocated"
            )));
        }
        Ok(StorageQuota {
            tenant: tenant.to_string(),
            max_bytes: self.load_quota(&dir)?,
            used_bytes: dir_size(&dir).await,
        })
    }

    /// 列出全部租户。
    pub async fn list_tenants(&self, sec: &SecurityContext) -> AionResult<Vec<String>> {
        sec.check_cap("storage:admin")?;
        let mut rd = tokio::fs::read_dir(&self.root).await?;
        let mut tenants = Vec::new();
        while let Some(entry) = rd.next_entry().await? {
            if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                tenants.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        tenants.sort();
        Ok(tenants)
    }
}

/// 目录大小（字节）。
async fn dir_size(dir: &Path) -> u64 {
    let path = dir.to_path_buf();
    tokio::task::spawn_blocking(move || dir_size_sync(&path))
        .await
        .unwrap_or(0)
}

fn dir_size_sync(dir: &Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                total += dir_size_sync(&entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

#[async_trait]
impl cordis::Service for StorageService {
    fn name(&self) -> &'static str {
        "storage"
    }

    fn description(&self) -> &'static str {
        "存储管理"
    }

    async fn start(&self, ctx: &cordis::Context) -> cordis::CordisResult<()> {
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|e| cordis::CordisError::Custom(e.to_string()))?;
        ctx.info(format!(
            "StorageService ready (root: {})",
            self.root.display()
        ));
        Ok(())
    }
}

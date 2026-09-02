//! Device 适配器：设备枚举与访问权限检查（GPU / USB 等设备管理基础）。

use std::path::Path;

use async_trait::async_trait;

use crate::{AdapterError, AdapterResult};

/// 设备类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Block,
    Char,
    Unknown,
}

/// 设备信息。
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub name: String,
    pub path: std::path::PathBuf,
    pub kind: DeviceKind,
    pub major: Option<u32>,
    pub minor: Option<u32>,
    pub readable: bool,
    pub writable: bool,
}

/// Device 适配器 trait。
#[async_trait]
pub trait DeviceAdapter: Send + Sync {
    /// 枚举 `root`（通常为 `/dev`）下的设备。
    async fn list(&self, root: &Path) -> AdapterResult<Vec<DeviceInfo>>;

    /// 检查当前进程对设备节点的访问权限。
    async fn check_access(&self, path: &Path, write: bool) -> AdapterResult<bool>;
}

/// 平台原生实现。
pub struct NativeDeviceAdapter;

#[async_trait]
impl DeviceAdapter for NativeDeviceAdapter {
    async fn list(&self, root: &Path) -> AdapterResult<Vec<DeviceInfo>> {
        let mut rd = tokio::fs::read_dir(root)
            .await
            .map_err(|e| AdapterError::Other(format!("read_dir {}: {e}", root.display())))?;
        let mut out = Vec::new();
        while let Some(entry) = rd.next_entry().await? {
            let Ok(meta) = entry.metadata().await else {
                continue;
            };
            let (kind, major, minor) = device_kind_of(&meta);
            let perms = meta.permissions();
            #[cfg(unix)]
            let (readable, writable) = {
                use std::os::unix::fs::PermissionsExt;
                let mode = perms.mode();
                (mode & 0o444 != 0, mode & 0o222 != 0)
            };
            #[cfg(not(unix))]
            let (readable, writable) = (!perms.readonly(), !perms.readonly());
            out.push(DeviceInfo {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: entry.path(),
                kind,
                major,
                minor,
                readable,
                writable,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn check_access(&self, path: &Path, write: bool) -> AdapterResult<bool> {
        let meta = tokio::fs::metadata(path).await?;
        let perms = meta.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = perms.mode();
            Ok(if write {
                mode & 0o222 != 0
            } else {
                mode & 0o444 != 0
            })
        }
        #[cfg(not(unix))]
        {
            let _ = write;
            Ok(!perms.readonly())
        }
    }
}

#[cfg(target_os = "linux")]
fn device_kind_of(meta: &std::fs::Metadata) -> (DeviceKind, Option<u32>, Option<u32>) {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let ft = meta.file_type();
    let kind = if ft.is_char_device() {
        DeviceKind::Char
    } else if ft.is_block_device() {
        DeviceKind::Block
    } else {
        DeviceKind::Unknown
    };
    if kind == DeviceKind::Unknown {
        return (kind, None, None);
    }
    let rdev = meta.rdev();
    (
        kind,
        Some(unsafe { libc::major(rdev) } as u32),
        Some(unsafe { libc::minor(rdev) } as u32),
    )
}

#[cfg(not(target_os = "linux"))]
fn device_kind_of(_meta: &std::fs::Metadata) -> (DeviceKind, Option<u32>, Option<u32>) {
    (DeviceKind::Unknown, None, None)
}

/// 平台默认的设备根目录。
pub fn default_device_root() -> std::path::PathBuf {
    #[cfg(target_os = "linux")]
    {
        std::path::PathBuf::from("/dev")
    }
    #[cfg(not(target_os = "linux"))]
    {
        // 非 Linux 平台没有 /dev：使用专用空目录，避免误把普通文件当设备
        let dir = std::env::temp_dir().join("aion-device-root");
        std::fs::create_dir_all(&dir).ok();
        dir
    }
}

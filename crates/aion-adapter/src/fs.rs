//! FS 适配器：文件系统调用封装（open / read / write / mount）。

use std::path::Path;

use async_trait::async_trait;

use crate::{AdapterError, AdapterResult};

/// 目录项。
#[derive(Debug, Clone)]
pub struct DirEntryInfo {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// 文件元数据。
#[derive(Debug, Clone, Copy)]
pub struct FileMeta {
    pub is_dir: bool,
    pub size: u64,
}

/// FS 适配器 trait。
#[async_trait]
pub trait FsAdapter: Send + Sync {
    async fn read(&self, path: &Path) -> AdapterResult<Vec<u8>>;

    /// 写文件（父目录自动创建）；`append` 为 true 时追加。
    async fn write(&self, path: &Path, data: &[u8], append: bool) -> AdapterResult<()>;

    async fn list(&self, path: &Path) -> AdapterResult<Vec<DirEntryInfo>>;

    async fn metadata(&self, path: &Path) -> AdapterResult<FileMeta>;

    /// 递归创建目录。
    async fn mkdir(&self, path: &Path) -> AdapterResult<()>;

    /// 删除文件或目录（`recursive` 对目录生效）。
    async fn remove(&self, path: &Path, recursive: bool) -> AdapterResult<()>;

    /// 挂载文件系统（仅 Linux，需 root/CAP_SYS_ADMIN）。
    async fn mount(&self, source: &str, target: &Path, fstype: &str, flags: u64) -> AdapterResult<()> {
        let _ = (source, target, fstype, flags);
        Err(AdapterError::Unsupported("mount requires Linux".into()))
    }

    /// 卸载（仅 Linux）。
    async fn umount(&self, target: &Path) -> AdapterResult<()> {
        let _ = target;
        Err(AdapterError::Unsupported("umount requires Linux".into()))
    }
}

/// 平台原生实现（tokio::fs；Linux 上额外支持 mount/umount）。
pub struct NativeFsAdapter;

#[async_trait]
impl FsAdapter for NativeFsAdapter {
    async fn read(&self, path: &Path) -> AdapterResult<Vec<u8>> {
        Ok(tokio::fs::read(path).await?)
    }

    async fn write(&self, path: &Path, data: &[u8], append: bool) -> AdapterResult<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }
        if append {
            use tokio::io::AsyncWriteExt;
            let mut f = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await?;
            f.write_all(data).await?;
            Ok(())
        } else {
            Ok(tokio::fs::write(path, data).await?)
        }
    }

    async fn list(&self, path: &Path) -> AdapterResult<Vec<DirEntryInfo>> {
        let mut out = Vec::new();
        let mut rd = tokio::fs::read_dir(path).await?;
        while let Some(entry) = rd.next_entry().await? {
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
            out.push(DirEntryInfo {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir,
                size,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn metadata(&self, path: &Path) -> AdapterResult<FileMeta> {
        let meta = tokio::fs::metadata(path).await?;
        Ok(FileMeta {
            is_dir: meta.is_dir(),
            size: meta.len(),
        })
    }

    async fn mkdir(&self, path: &Path) -> AdapterResult<()> {
        Ok(tokio::fs::create_dir_all(path).await?)
    }

    async fn remove(&self, path: &Path, recursive: bool) -> AdapterResult<()> {
        let meta = tokio::fs::symlink_metadata(path).await?;
        if meta.is_dir() {
            if recursive {
                Ok(tokio::fs::remove_dir_all(path).await?)
            } else {
                Ok(tokio::fs::remove_dir(path).await?)
            }
        } else {
            Ok(tokio::fs::remove_file(path).await?)
        }
    }

    /// 挂载文件系统（Linux 专属）。
    #[cfg(target_os = "linux")]
    async fn mount(&self, source: &str, target: &Path, fstype: &str, flags: u64) -> AdapterResult<()> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let c_source = CString::new(source)
            .map_err(|_| AdapterError::Other("mount source contains NUL".into()))?;
        let c_target = CString::new(target.as_os_str().as_bytes())
            .map_err(|_| AdapterError::Other("mount target contains NUL".into()))?;
        let c_fstype = CString::new(fstype)
            .map_err(|_| AdapterError::Other("mount fstype contains NUL".into()))?;
        // SAFETY: 所有指针均来自本函数创建的 CString，内核校验其余参数。
        let rc = unsafe {
            libc::mount(
                c_source.as_ptr(),
                c_target.as_ptr(),
                c_fstype.as_ptr(),
                flags as libc::c_ulong,
                std::ptr::null(),
            )
        };
        if rc != 0 {
            return Err(AdapterError::Io(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    async fn mount(&self, source: &str, target: &Path, fstype: &str, flags: u64) -> AdapterResult<()> {
        let _ = (source, target, fstype, flags);
        Err(AdapterError::Unsupported("mount requires Linux".into()))
    }

    #[cfg(target_os = "linux")]
    async fn umount(&self, target: &Path) -> AdapterResult<()> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        const MNT_DETACH: i32 = 2;
        let c_target = CString::new(target.as_os_str().as_bytes())
            .map_err(|_| AdapterError::Other("umount target contains NUL".into()))?;
        // SAFETY: 路径指针来自本函数创建的 CString。
        let rc = unsafe { libc::umount2(c_target.as_ptr(), MNT_DETACH) };
        if rc != 0 {
            return Err(AdapterError::Io(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    async fn umount(&self, target: &Path) -> AdapterResult<()> {
        let _ = target;
        Err(AdapterError::Unsupported("umount requires Linux".into()))
    }
}

/// 常用挂载标志（Linux）。
#[cfg(target_os = "linux")]
pub mod mount_flags {
    pub const MS_BIND: u64 = 4096;
    pub const MS_REC: u64 = 16384;
    pub const MS_PRIVATE: u64 = 262144;
    pub const MS_NOSUID: u64 = 2;
    pub const MS_NODEV: u64 = 4;
    pub const MS_NOEXEC: u64 = 8;
    pub const MS_RDONLY: u64 = 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip() {
        let dir = std::env::temp_dir().join(format!("aion-fs-test-{}", std::process::id()));
        let adapter = NativeFsAdapter;
        let file = dir.join("sub/hello.txt");
        adapter.mkdir(&dir.join("sub")).await.unwrap();
        adapter.write(&file, b"hello aion", false).await.unwrap();
        assert_eq!(adapter.read(&file).await.unwrap(), b"hello aion");
        let entries = adapter.list(&dir.join("sub")).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "hello.txt");
        adapter.write(&file, b"!", true).await.unwrap();
        assert_eq!(adapter.read(&file).await.unwrap(), b"hello aion!");
        adapter.remove(&dir, true).await.unwrap();
        assert!(adapter.metadata(&dir).await.is_err());
    }
}

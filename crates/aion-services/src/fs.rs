//! FileService：文件系统。路径白名单内的读写 / 列目录 / 删除。

use std::path::{Path, PathBuf};

use aion_adapter::{AdapterKit, DirEntryInfo, FileMeta};
use async_trait::async_trait;

use crate::error::AionResult;
use crate::security::SecurityContext;

/// 文件系统服务。
pub struct FileService {
    kit: AdapterKit,
}

impl FileService {
    pub fn new(kit: AdapterKit) -> Self {
        FileService { kit }
    }

    fn check(&self, sec: &SecurityContext, path: &Path, write: bool) -> AionResult<PathBuf> {
        let cap = if write { "fs:write" } else { "fs:read" };
        sec.check_cap(cap)?;
        sec.check_path(path, write)
    }

    pub async fn read(&self, sec: &SecurityContext, path: impl AsRef<Path>) -> AionResult<Vec<u8>> {
        let path = self.check(sec, path.as_ref(), false)?;
        Ok(self.kit.fs.read(&path).await?)
    }

    pub async fn write(
        &self,
        sec: &SecurityContext,
        path: impl AsRef<Path>,
        data: &[u8],
    ) -> AionResult<()> {
        let path = self.check(sec, path.as_ref(), true)?;
        Ok(self.kit.fs.write(&path, data, false).await?)
    }

    pub async fn list(
        &self,
        sec: &SecurityContext,
        path: impl AsRef<Path>,
    ) -> AionResult<Vec<DirEntryInfo>> {
        let path = self.check(sec, path.as_ref(), false)?;
        Ok(self.kit.fs.list(&path).await?)
    }

    pub async fn metadata(
        &self,
        sec: &SecurityContext,
        path: impl AsRef<Path>,
    ) -> AionResult<FileMeta> {
        let path = self.check(sec, path.as_ref(), false)?;
        Ok(self.kit.fs.metadata(&path).await?)
    }

    pub async fn mkdir(
        &self,
        sec: &SecurityContext,
        path: impl AsRef<Path>,
    ) -> AionResult<()> {
        let path = self.check(sec, path.as_ref(), true)?;
        Ok(self.kit.fs.mkdir(&path).await?)
    }

    pub async fn remove(
        &self,
        sec: &SecurityContext,
        path: impl AsRef<Path>,
        recursive: bool,
    ) -> AionResult<()> {
        let path = self.check(sec, path.as_ref(), true)?;
        Ok(self.kit.fs.remove(&path, recursive).await?)
    }
}

#[async_trait]
impl cordis::Service for FileService {
    fn name(&self) -> &'static str {
        "file"
    }

    fn description(&self) -> &'static str {
        "文件系统"
    }

    async fn start(&self, ctx: &cordis::Context) -> cordis::CordisResult<()> {
        ctx.info("FileService ready");
        Ok(())
    }
}

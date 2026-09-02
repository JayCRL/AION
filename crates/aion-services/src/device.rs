//! DeviceService：设备管理（GPU / USB 等节点枚举与访问控制）。

use std::path::{Path, PathBuf};

use aion_adapter::device::{default_device_root, DeviceInfo};
use aion_adapter::AdapterKit;
use async_trait::async_trait;

use crate::error::AionResult;
use crate::security::SecurityContext;

/// 设备管理服务。
pub struct DeviceService {
    kit: AdapterKit,
    root: PathBuf,
}

impl DeviceService {
    pub fn new(kit: AdapterKit) -> Self {
        DeviceService {
            kit,
            root: default_device_root(),
        }
    }

    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = root.into();
        self
    }

    /// 枚举设备。
    pub async fn list(&self, sec: &SecurityContext, root: Option<&Path>) -> AionResult<Vec<DeviceInfo>> {
        sec.check_cap("device:list")?;
        Ok(self.kit.device.list(root.unwrap_or(&self.root)).await?)
    }

    /// 检查设备节点访问权限。
    pub async fn check_access(
        &self,
        sec: &SecurityContext,
        path: impl AsRef<Path>,
        write: bool,
    ) -> AionResult<bool> {
        sec.check_cap("device:use")?;
        Ok(self.kit.device.check_access(path.as_ref(), write).await?)
    }
}

#[async_trait]
impl cordis::Service for DeviceService {
    fn name(&self) -> &'static str {
        "device"
    }

    fn description(&self) -> &'static str {
        "设备管理 (GPU/USB)"
    }

    async fn start(&self, ctx: &cordis::Context) -> cordis::CordisResult<()> {
        ctx.info(format!(
            "DeviceService ready (root: {})",
            self.root.display()
        ));
        Ok(())
    }
}

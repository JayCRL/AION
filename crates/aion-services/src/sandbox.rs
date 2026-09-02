//! SandboxService：沙箱 & 隔离。生成沙箱档案、报告平台能力。

use aion_adapter::{AdapterKit, SandboxProfile, SandboxSupport};
use async_trait::async_trait;

use crate::error::AionResult;
use crate::security::SecurityContext;

/// 沙箱请求参数。
#[derive(Debug, Clone, Default)]
pub struct SandboxRequest {
    /// memory.max（字节）；None 用默认值。
    pub memory_max_bytes: Option<u64>,
    /// cpu.max 配额（微秒 / 100ms 周期）。
    pub cpu_quota_us: Option<u64>,
    /// pids.max。
    pub pids_max: Option<i64>,
}

/// 沙箱 & 隔离服务。
pub struct SandboxService {
    kit: AdapterKit,
}

impl SandboxService {
    pub fn new(kit: AdapterKit) -> Self {
        SandboxService { kit }
    }

    /// 生成沙箱档案（在 spawn 前可检视 / 持久化）。
    pub async fn create_profile(
        &self,
        sec: &SecurityContext,
        request: &SandboxRequest,
    ) -> AionResult<SandboxProfile> {
        sec.check_cap("sandbox:create")?;
        let mut profile = SandboxProfile::strict();
        if let Some(cg) = profile.cgroup.as_mut() {
            if let Some(m) = request.memory_max_bytes {
                cg.memory_max_bytes = Some(m);
            }
            if let Some(q) = request.cpu_quota_us {
                cg.cpu_quota_us = Some(q);
                cg.cpu_period_us = Some(100_000);
            }
            if let Some(p) = request.pids_max {
                cg.pids_max = Some(p);
            }
        }
        Ok(profile)
    }

    /// 平台沙箱能力报告（安全隔离 / 可观测性）。
    pub async fn inspect(&self, sec: &SecurityContext) -> AionResult<SandboxSupport> {
        sec.check_cap("sandbox:inspect")?;
        Ok(self.kit.sandbox_support())
    }
}

/// 构建默认沙箱档案（供 ProcessService 等内部使用）。
pub fn build_profile() -> SandboxProfile {
    SandboxProfile::strict()
}

#[async_trait]
impl cordis::Service for SandboxService {
    fn name(&self) -> &'static str {
        "sandbox"
    }

    fn description(&self) -> &'static str {
        "沙箱 & 隔离"
    }

    async fn start(&self, ctx: &cordis::Context) -> cordis::CordisResult<()> {
        let support = self.kit.sandbox_support();
        ctx.info(format!(
            "SandboxService ready: {}",
            support.summary()
        ));
        Ok(())
    }
}

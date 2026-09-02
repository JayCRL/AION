//! Custom Agent：可扩展示例 —— 配置驱动的通用 Agent。
//!
//! 展示 AION 的可扩展性：新 Agent 只需实现 [`Agent`] trait 并注册。

use async_trait::async_trait;
use cordis::Context;

use aion_services::SecurityContext;

use crate::agents::{Agent, AgentTask};

/// 通用 Custom Agent：把输入按配置前缀处理后返回。
pub struct CustomAgent {
    label: String,
}

impl CustomAgent {
    pub fn new(label: impl Into<String>) -> Self {
        CustomAgent {
            label: label.into(),
        }
    }
}

impl Default for CustomAgent {
    fn default() -> Self {
        Self::new("custom")
    }
}

#[async_trait]
impl Agent for CustomAgent {
    fn name(&self) -> &'static str {
        "custom"
    }

    fn description(&self) -> &'static str {
        "Custom Agent — 可扩展示例（配置驱动）"
    }

    fn default_caps(&self) -> Vec<&'static str> {
        vec![]
    }

    async fn handle(
        &self,
        _ctx: &Context,
        _sec: &SecurityContext,
        task: &AgentTask,
    ) -> anyhow::Result<String> {
        let prefix = task
            .params
            .get("prefix")
            .and_then(|v| v.as_str())
            .unwrap_or("处理完成");
        Ok(format!(
            "✓ [{}] {prefix}: {}",
            self.label,
            if task.input.trim().is_empty() {
                "<empty task>"
            } else {
                task.input.trim()
            }
        ))
    }
}

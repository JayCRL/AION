//! Assistant Agent：通过模型服务对话。

use async_trait::async_trait;
use cordis::Context;

use aion_services::model::{ChatMessage, ModelService};
use aion_services::SecurityContext;

use crate::agents::{Agent, AgentTask};

pub struct AssistantAgent;

#[async_trait]
impl Agent for AssistantAgent {
    fn name(&self) -> &'static str {
        "assistant"
    }

    fn description(&self) -> &'static str {
        "Assistant Agent — LLM 对话"
    }

    fn default_caps(&self) -> Vec<&'static str> {
        vec!["model:use"]
    }

    async fn handle(
        &self,
        ctx: &Context,
        sec: &SecurityContext,
        task: &AgentTask,
    ) -> anyhow::Result<String> {
        let model = ctx.require::<ModelService>().await?;
        let input = if task.input.trim().is_empty() {
            "介绍一下你自己"
        } else {
            task.input.as_str()
        };
        let reply = model
            .chat(
                sec,
                None,
                &[
                    ChatMessage::system("你是运行在 AION Agent OS 上的助手，回答保持简洁。"),
                    ChatMessage::user(input),
                ],
            )
            .await?;
        Ok(reply)
    }
}

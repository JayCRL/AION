//! Assistant Agent：通过模型服务对话（支持多轮历史）。

use async_trait::async_trait;
use cordis::Context;

use aion_services::model::{ChatMessage, ModelService};
use aion_services::SecurityContext;

use crate::agents::{Agent, AgentTask};

/// Assistant 的 system prompt（Web 层回喂工具输出时复用同一段）。
pub const ASSISTANT_SYSTEM: &str = "你是运行在 AION Agent OS 上的助手，回答保持简洁。当需要执行命令时，\
用 ```run 代码块给出单条命令（用户会执行并把结果回传）。";

pub struct AssistantAgent;

/// 历史窗口：最多带最近 20 条进请求。
const HISTORY_LIMIT: usize = 20;

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

        let mut msgs = vec![ChatMessage::system(ASSISTANT_SYSTEM)];
        // Web 层透传的会话历史（仅 user / assistant，最新 20 条）
        if let Some(items) = task.params.get("history").and_then(|h| h.as_array()) {
            let recent: Vec<&serde_json::Value> = items.iter().rev().take(HISTORY_LIMIT).collect();
            for m in recent.into_iter().rev() {
                let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("");
                if content.is_empty() {
                    continue;
                }
                match m.get("role").and_then(|r| r.as_str()) {
                    Some("user") => msgs.push(ChatMessage::user(content)),
                    Some("assistant") => msgs.push(ChatMessage::assistant(content)),
                    _ => {}
                }
            }
        }
        msgs.push(ChatMessage::user(input));

        let reply = model.chat(sec, None, &msgs).await?;
        Ok(reply)
    }
}

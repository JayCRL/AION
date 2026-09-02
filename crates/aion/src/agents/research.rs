//! Research Agent：抓取网页并用模型总结。

use async_trait::async_trait;
use cordis::Context;

use aion_services::model::{ChatMessage, ModelService};
use aion_services::network::NetworkService;
use aion_services::SecurityContext;

use crate::agents::AgentTask;
use crate::agents::Agent;

pub struct ResearchAgent;

#[async_trait]
impl Agent for ResearchAgent {
    fn name(&self) -> &'static str {
        "research"
    }

    fn description(&self) -> &'static str {
        "Research Agent — 抓取网页并总结"
    }

    fn default_caps(&self) -> Vec<&'static str> {
        vec!["net:connect", "model:use"]
    }

    async fn handle(
        &self,
        ctx: &Context,
        sec: &SecurityContext,
        task: &AgentTask,
    ) -> anyhow::Result<String> {
        let url = task.input.trim().to_string();
        if url.is_empty() {
            anyhow::bail!("usage: fetch <http-url>");
        }
        let network = ctx.require::<NetworkService>().await?;
        let reply = network.http_get(sec, &url).await?;
        let title = reply.title().unwrap_or_else(|| "<no title>".into());
        let body_head: String = reply.body_text().chars().take(600).collect();

        let model = ctx.require::<ModelService>().await?;
        let summary = model
            .chat(
                sec,
                None,
                &[
                    ChatMessage::system("你是研究助手，请用三句话总结页面内容。"),
                    ChatMessage::user(format!("页面: {url}\n标题: {title}\n内容片段:\n{body_head}")),
                ],
            )
            .await?;
        Ok(format!(
            "✓ 抓取 {url}\n  status={} bytes={} title={title}\n  总结:\n{summary}",
            reply.status,
            reply.body.len()
        ))
    }
}

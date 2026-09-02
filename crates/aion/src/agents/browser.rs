//! Browser Agent：模拟浏览器抓取页面（HTTP GET + 标题提取）。

use async_trait::async_trait;
use cordis::Context;

use aion_services::network::NetworkService;
use aion_services::SecurityContext;

use crate::agents::{Agent, AgentTask};

pub struct BrowserAgent;

#[async_trait]
impl Agent for BrowserAgent {
    fn name(&self) -> &'static str {
        "browser"
    }

    fn description(&self) -> &'static str {
        "Browser Agent — 抓取并解析页面"
    }

    fn default_caps(&self) -> Vec<&'static str> {
        vec!["net:connect"]
    }

    async fn handle(
        &self,
        ctx: &Context,
        sec: &SecurityContext,
        task: &AgentTask,
    ) -> anyhow::Result<String> {
        let url = task.input.trim().to_string();
        if url.is_empty() {
            anyhow::bail!("usage: open <http-url>");
        }
        let network = ctx.require::<NetworkService>().await?;
        let reply = network.http_get(sec, &url).await?;
        let title = reply.title().unwrap_or_else(|| "<no title>".into());
        Ok(format!(
            "✓ 打开 {url}\n  status={} | {} bytes | title: {title}",
            reply.status,
            reply.body.len()
        ))
    }
}

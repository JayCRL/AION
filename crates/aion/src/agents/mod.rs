//! Agents / Apps：应用层。每个 Agent 通过 Cordis Context 获取系统服务完成任务。

pub mod assistant;
pub mod browser;
pub mod coder;
pub mod custom;
pub mod research;

use std::path::PathBuf;
use std::sync::Arc;

use aion_services::SecurityContext;
use async_trait::async_trait;
use cordis::Context;
use serde_json::Value;

/// 一次 Agent 任务。
#[derive(Debug, Clone)]
pub struct AgentTask {
    /// 任务类型（如 `run` / `chat` / `fetch` / `open` / `write`）。
    pub kind: String,
    /// 主要输入。
    pub input: String,
    /// 附加参数。
    pub params: Value,
}

impl AgentTask {
    pub fn new(kind: impl Into<String>, input: impl Into<String>) -> Self {
        AgentTask {
            kind: kind.into(),
            input: input.into(),
            params: serde_json::json!({}),
        }
    }
}

/// Agent trait：应用层统一接口。
#[async_trait]
pub trait Agent: Send + Sync {
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    /// 该 Agent 需要的 capability（最小权限建议集）。
    fn default_caps(&self) -> Vec<&'static str>;

    async fn handle(
        &self,
        ctx: &Context,
        sec: &SecurityContext,
        task: &AgentTask,
    ) -> anyhow::Result<String>;
}

/// 内置 Agent 列表。
pub fn builtin() -> Vec<Arc<dyn Agent>> {
    vec![
        Arc::new(coder::CoderAgent),
        Arc::new(research::ResearchAgent),
        Arc::new(assistant::AssistantAgent),
        Arc::new(browser::BrowserAgent),
        Arc::new(custom::CustomAgent::default()),
    ]
}

/// 开发/演示用安全上下文：全 capability + 本机根目录（仅限本机试用）。
pub fn developer_sec(agent: &str, extra_roots: &[PathBuf]) -> SecurityContext {
    let mut sec = SecurityContext::new(agent)
        .allow_all()
        .net("*")
        .max_processes(32);
    #[cfg(target_os = "windows")]
    sec.fs_roots.push(PathBuf::from("C:\\"));
    #[cfg(not(target_os = "windows"))]
    sec.fs_roots.push(PathBuf::from("/"));
    for root in extra_roots {
        sec.fs_roots.push(root.clone());
    }
    sec
}

/// 用系统 shell 执行一段命令。
pub fn shell_args(input: &str) -> (&'static str, Vec<String>) {
    #[cfg(target_os = "windows")]
    {
        ("cmd", vec!["/C".to_string(), input.to_string()])
    }
    #[cfg(not(target_os = "windows"))]
    {
        ("sh", vec!["-c".to_string(), input.to_string()])
    }
}

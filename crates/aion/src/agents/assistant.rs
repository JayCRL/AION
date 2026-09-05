//! Assistant Agent：通过模型服务对话（支持多轮历史）。
//! Web 流式路由复用 `chat_messages`，保证与 Agent 走同一套上下文组装。

use async_trait::async_trait;
use cordis::Context;

use aion_services::model::{ChatMessage, ModelService};
use aion_services::SecurityContext;

use crate::agents::{Agent, AgentTask};

/// 历史窗口：最多带最近 20 条进请求。
pub const HISTORY_LIMIT: usize = 20;

/// Assistant 的 system prompt（Web 层回喂工具输出时复用同一段）。
///
/// 关键约定：AION 通过原生 `tools` 提供工具调用能力。`tools` 里带**目标导向的
/// Capability**（如 `web.view`：模型只需说清"看哪个 URL"，AION 自动选最合适的
/// 抓取/渲染实现），其余是叶子工具。模型应**优先发起原生工具调用（tool_use）**
/// 完成任务；仅当本次请求未携带 `tools`（纯文本模式）时，才用 ```run 代码块
/// 回退执行命令。绝不把执行推给用户。
pub const ASSISTANT_SYSTEM: &str = r#"你是运行在 AION Agent OS 上的助手。回答保持简洁、直接行动，不要只给建议。
你拥有真实的能力与沙箱终端：本机文件、进程、系统状态都可查询/操作，网页浏览可直接达成，命令执行后会收到真实 stdout/stderr/退出码并继续分析。
当你需要达成某个目标时，优先使用对话里提供的能力/工具（发起 tool_use 调用）：对目标导向的能力（如 web.view 看网页）只需给出意图与参数，AION 会内部自动选择最合适的执行引擎/实现，不要臆造输出，也不要把执行推给用户。
需要多步就连续发起多个调用，AION 会逐个执行并把结果回填给你。
收到真实结果后基于它继续作答；正文用自然语言，不要把整条命令再复制进正文。"#;

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
        // Web 层透传的会话历史（仅 user / assistant）
        let mut history: Vec<ChatMessage> = Vec::new();
        if let Some(items) = task.params.get("history").and_then(|h| h.as_array()) {
            for m in items {
                let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("");
                if content.is_empty() {
                    continue;
                }
                match m.get("role").and_then(|r| r.as_str()) {
                    Some("user") => history.push(ChatMessage::user(content)),
                    Some("assistant") => history.push(ChatMessage::assistant(content)),
                    _ => {}
                }
            }
        }
        let msgs = chat_messages(&history, &task.input);
        let reply = model.chat(sec, None, &msgs).await?;
        Ok(reply)
    }
}

/// 组装发给模型的 messages：system + 最近 `HISTORY_LIMIT` 条会话 + 本次输入。
/// Web 流式路由也走这里，保证与 Agent 使用同一套上下文组装逻辑。
pub fn chat_messages(history: &[ChatMessage], input: &str) -> Vec<ChatMessage> {
    let mut msgs = vec![ChatMessage::system(ASSISTANT_SYSTEM)];
    let recent: Vec<&ChatMessage> = history.iter().rev().take(HISTORY_LIMIT).collect();
    for m in recent.into_iter().rev() {
        if m.content.is_empty() {
            continue;
        }
        match m.role.as_str() {
            "user" => msgs.push(ChatMessage::user(&m.content)),
            "assistant" => msgs.push(ChatMessage::assistant(&m.content)),
            _ => {}
        }
    }
    let input = if input.trim().is_empty() {
        "介绍一下你自己"
    } else {
        input
    };
    msgs.push(ChatMessage::user(input));
    msgs
}

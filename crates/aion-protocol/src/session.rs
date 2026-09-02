//! Session + Message —— 一次交互的全部状态。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::call::{CallId, ToolCall};
use crate::result::RequestId;
use crate::ui::{UIAction, UIBlock};

// ------------------------------------------------------------------
// SessionId / MessageId
// ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new() -> Self {
        Self(format!("session-{}", monotonic_suffix()))
    }
    pub fn as_str(&self) -> &str { &self.0 }
}

impl Default for SessionId { fn default() -> Self { Self::new() } }
impl std::fmt::Display for SessionId { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) } }
impl From<&str> for SessionId { fn from(s: &str) -> Self { Self(s.to_string()) } }
impl From<String> for SessionId { fn from(s: String) -> Self { Self(s) } }

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageId(pub String);

impl MessageId {
    pub fn new() -> Self {
        Self(format!("msg-{}", monotonic_suffix()))
    }
    pub fn as_str(&self) -> &str { &self.0 }
}

impl Default for MessageId { fn default() -> Self { Self::new() } }
impl std::fmt::Display for MessageId { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) } }
impl From<&str> for MessageId { fn from(s: &str) -> Self { Self(s.to_string()) } }
impl From<String> for MessageId { fn from(s: String) -> Self { Self(s) } }

// ------------------------------------------------------------------
// Role
// ------------------------------------------------------------------

/// 消息来源角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    Tool,
    System,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
            Self::System => "system",
        }
    }
}

// ------------------------------------------------------------------
// Session
// ------------------------------------------------------------------

/// 会话状态机。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Open,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,

    /// 拥有此 Session 的 Agent 名（例如 `"coder"` / `"assistant"`）。
    pub agent: String,

    /// 创建时间（unix nanos；UI 可格式化为本地时间）。
    pub created_at: u64,

    /// 关闭时间（Closed 时存在）；未关闭时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<u64>,

    pub state: SessionState,

    /// 消息历史（顺序即用户/Agent 视角的对话顺序）。
    pub conversation: Vec<Message>,

    /// 挂起中、等待用户 Confirmation 的 StalledCall。
    /// key = `RequestId`（与 `ToolResult.status: Pending` 共享）。
    pub pending: BTreeMap<RequestId, StalledCall>,
}

impl Session {
    pub fn new(agent: impl Into<String>) -> Self {
        Self {
            id: SessionId::new(),
            agent: agent.into(),
            created_at: now_unix_nanos(),
            closed_at: None,
            state: SessionState::Open,
            conversation: Vec::new(),
            pending: BTreeMap::new(),
        }
    }

    pub fn append(&mut self, message: Message) {
        self.conversation.push(message);
    }

    /// 用 `ToolResult` 的 `Pending` 状态登记一个挂起调用。
    /// 同时把对应 `ConfirmationBlock` 加进最近一条 Assistant 消息的 `blocks`。
    pub fn register_pending(
        &mut self,
        request_id: RequestId,
        tool: String,
        arguments: serde_json::Value,
        prompt: String,
        confirmation: UIBlock,
    ) -> Option<StalledCall> {
        if !matches!(self.state, SessionState::Open) {
            return None;
        }
        if self.pending.contains_key(&request_id) {
            return None;
        }
        let stalled = StalledCall {
            tool,
            arguments,
            prompt,
            pending_since: now_unix_nanos(),
        };
        self.pending.insert(request_id.clone(), stalled.clone());
        if let Some(last) = self.conversation.last_mut() {
            if matches!(last.role, Role::Assistant) {
                last.blocks.push(confirmation);
            }
        }
        Some(stalled)
    }

    /// 用户确认后调用：把挂起调用取出，返回其内容供 Runtime 真正派发。
    pub fn resolve_pending(&mut self, request_id: &RequestId) -> Option<StalledCall> {
        self.pending.remove(request_id)
    }
}

/// 一个挂起中的 ToolCall（等用户 Confirmation）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StalledCall {
    /// 工具名。
    pub tool: String,
    /// 已校验过的参数。
    #[serde(default)]
    pub arguments: serde_json::Value,
    /// 人可读的"我准备做什么"提示（用于前端显示在 ConfirmationBlock 附近）。
    pub prompt: String,
    /// 何时被挂起。
    pub pending_since: u64,
}

// ------------------------------------------------------------------
// Message
// ------------------------------------------------------------------

/// 对话中的一条消息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub role: Role,
    /// 纯文本内容（可与下面的 `blocks` 并存）。
    /// `None` 表示纯 block 消息；`Some("")` 表示空文本但保留 block；这两者区别由 Agent 决定。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,

    pub blocks: Vec<UIBlock>,
    pub actions: Vec<UIAction>,

    /// 此消息发出的 ToolCall（例如 Assistant 想要执行文件查找）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,

    /// 创建时间（unix nanos）。
    pub timestamp: u64,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            role: Role::User,
            text: Some(text.into()),
            blocks: vec![],
            actions: vec![],
            tool_calls: vec![],
            timestamp: now_unix_nanos(),
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            role: Role::Assistant,
            text: Some(text.into()),
            blocks: vec![],
            actions: vec![],
            tool_calls: vec![],
            timestamp: now_unix_nanos(),
        }
    }

    /// 添加 block 并消费 self（builder 风格）。一次只追加一组块。
    pub fn with_blocks(mut self, blocks: Vec<UIBlock>) -> Self {
        self.blocks = blocks;
        self
    }

    /// 添加 Action。
    pub fn with_actions(mut self, actions: Vec<UIAction>) -> Self {
        self.actions = actions;
        self
    }
}

// ------------------------------------------------------------------
// Time helper
// ------------------------------------------------------------------

fn now_unix_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn monotonic_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{pid:x}-{nanos:x}-{n:x}")
}

// 不暴露给 prelude
#[allow(dead_code)]
fn _unused_callid_check(c: CallId) -> String {
    c.to_string()
}

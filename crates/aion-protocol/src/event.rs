//! AION 协议级事件。
//!
//! 与 `cordis::event::Event`（runtime 内部）区分：`AionEvent` 是
//! 带 `session_id` 的协议层事件，由 Phase 2 的 Runtime 转
//! 发给前端 / 日志 / 指标。
//!
//! `payload` 是 `serde_json::Value` 而非 `Arc<dyn Any>`，便于序列化传输。

use serde::{Deserialize, Serialize};

use crate::session::SessionId;

/// Aion 事件类别（稳定字符串集 — 新增种类请向下加，不重用旧名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AionEventKind {
    /// Session 新建。
    SessionStarted,
    /// Session 关闭。
    SessionEnded,
    /// Runtime 收到 ToolCall，校验通过开始执行。
    ToolCallStarted,
    /// Tool 正常返回（Success 状态）。
    ToolCallFinished,
    /// Tool 返回 Error / Denied。
    ToolCallFailed,
    /// SecurityContext 拒绝该 ToolCall（被 Runtime 拦截）。
    PermissionDenied,
    /// 进入等待用户 Confirmation（与 `ResultStatus::Pending` 配对）。
    ConfirmationRequested,
    /// 用户完成 Confirmation（选择路径或取消）。
    ConfirmationGiven,
}

impl AionEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionStarted => "session_started",
            Self::SessionEnded => "session_ended",
            Self::ToolCallStarted => "tool_call_started",
            Self::ToolCallFinished => "tool_call_finished",
            Self::ToolCallFailed => "tool_call_failed",
            Self::PermissionDenied => "permission_denied",
            Self::ConfirmationRequested => "confirmation_requested",
            Self::ConfirmationGiven => "confirmation_given",
        }
    }
}

/// 事件本身。`id` 全局唯一（前端订阅去重）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AionEvent {
    pub id: String,
    pub session_id: SessionId,
    pub kind: AionEventKind,
    /// unix nanos。
    pub timestamp: u64,
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl AionEvent {
    pub fn new(session_id: SessionId, kind: AionEventKind, payload: serde_json::Value) -> Self {
        Self {
            id: format!("evt-{}", monotonic_suffix()),
            session_id,
            kind,
            timestamp: now_unix_nanos(),
            payload,
        }
    }
}

fn monotonic_suffix() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{pid:x}-{nanos:x}-{n:x}")
}

fn now_unix_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

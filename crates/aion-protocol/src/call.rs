//! Tool 调用请求 + 唯一 ID。

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// 用于追踪一次具体调用的唯一 ID。Phase 2 Runtime 用此 ID 把结果回给调用方。
///
/// 生成：进程唯一（基于 `pid + 系统时间纳秒 + 进程内原子计数器）。
/// Phase 2 可换 `getrandom` 取真熵，Phase 1 进程内足够。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CallId(pub String);

impl CallId {
    pub fn new() -> Self {
        Self(format!("call-{}", monotonic_suffix()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for CallId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for CallId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for CallId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() { Err("empty CallId".into()) } else { Ok(Self(s.to_string())) }
    }
}

impl From<&str> for CallId { fn from(s: &str) -> Self { Self(s.to_string()) } }
impl From<String> for CallId { fn from(s: String) -> Self { Self(s) } }

/// 进程唯一后缀：pid + nanos + 进程内原子计数器。
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

// -----------------------------------------------------------------------------

/// 一次 Tool 调用的请求载荷。
///
/// 由前端或 Agent 发出，Runtime 收到后执行：
///   1. 解析 `tool` 找到实现
///   2. 按 `ToolDefinition.input` schema 校验 `arguments`
///   3. 按 `ToolDefinition.required_caps` 校验 SecurityContext
///   4. 派发到 Tool::call
///   5. 返回 `ToolResult`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub call_id: CallId,

    /// 工具名（与 `ToolDefinition.name` 一致）。Agent 派生具体的工具名约定
    /// 例如 `"file.read"` / `"process.kill"` / `"system.stats"`。
    pub tool: String,

    /// 调用参数（按 tool 的 `input` schema）。
    #[serde(default)]
    pub arguments: serde_json::Value,

    /// 调用方请求的沙箱策略（Phase 2 由 Runtime 解释执行）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<ToolSandboxHint>,
}

/// 调用方请求的沙箱策略。Phase 2 Runtime 据此决定实际 sandbox。
///
/// Phase 1 仅作为数据保留 —— 字段未来可加（如 `cgroup_limits`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolSandboxHint {
    /// 委托 Runtime 默认策略（无额外要求）。
    Default,
    /// 要求 no_new_privs 已设置 + cgroup 资源限制；namespace/seccomp 跳过。
    /// 适用于不需要完整隔离但需要资源上下文的工具。
    NoneExec,
    /// 要求完整严格沙箱（namespace + seccomp + capability 收缩）；
    /// 在非 root / 不支持环境下 Runtime 返回 `Denied`。
    Strict,
}

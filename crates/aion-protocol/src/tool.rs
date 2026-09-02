//! Tool 元数据（纯数据）。
//!
//! Phase 2 中 [`ToolDefinition`] 将由 Registry 在 register 时提供（一个
//! Tool trait 持有 definition + 实现 `call(ctx, args)`）。Phase 1 仅定义形状。

use serde::{Deserialize, Serialize};

use crate::schema::JsonSchemaDocument;

/// 一个 Tool 的描述（名称、参数 schema、所需能力、风险等级）。
///
/// 可被前端拉取以渲染"我能做什么"列表；也可被 Runtime 用于参数校验 +
/// 权限绑定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// 全小写 snake_case，全局唯一（例如 `"file.read"`）。
    pub name: String,

    /// 人类可读描述（前端"工具描述"卡片原样显示）。
    pub description: String,

    /// 输入参数的 schema。Agent 收到 `ToolDefinition` 后按此 schema 生成参数。
    pub input: JsonSchemaDocument,

    /// 输出 schema（可选；用于在前端预渲染预期）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<JsonSchemaDocument>,

    /// 调用此工具所需的 capability 列表（Runtime 在调用前逐项 `check_cap`）。
    pub required_caps: Vec<String>,

    /// 风险等级，UI 用以决定是否需要 `ConfirmationBlock`。
    pub risk: Risk,
}

/// 工具风险等级。
///
/// 风险决定 UI 行为：
/// - `Low`: 不需要二次确认
/// - `Medium`: 默认执行，可选显示 `ConfirmationBlock` 由 Agent 决定
/// - `High` / `Critical`: 默认必须显示 `ConfirmationBlock` 才能继续
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Low,
    Medium,
    High,
    Critical,
}

impl Risk {
    /// 是否需要二次确认。
    pub fn requires_confirmation(self) -> bool {
        matches!(self, Risk::High | Risk::Critical)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Risk::Low => "low",
            Risk::Medium => "medium",
            Risk::High => "high",
            Risk::Critical => "critical",
        }
    }
}

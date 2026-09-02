//! UI Block —— 数据驱动 UI 的渲染单元。
//!
//! 每种变体是一种"how to render"指令；Agent / Tool 选择投放哪种 block，
//! 前端依据 `tag`（如 `"type": "chart"`）决定渲染风格。
//! **绝对禁止**让 LLM 输出 HTML 或 React 替代这套结构。

use serde::{Deserialize, Serialize};

use crate::call::{CallId, ToolCall};
use crate::result::RequestId;

// ------------------------------------------------------------------
// UIBlock
// ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UIBlock {
    /// 内联文本（Markdown 渲染）。
    Text { markdown: String },

    /// 完整 Markdown 块（用 markdown source 而非渲染后 HTML）。
    Markdown { source: String },

    /// 表格。
    Table {
        headers: Vec<String>,
        /// 每行每列是 Value，支持字符串 / 数字 / 嵌套结构。
        rows: Vec<Vec<serde_json::Value>>,
    },

    /// 图表（柱 / 折线 / 散点）。
    Chart(Chart),

    /// 文件展示。
    File {
        path: String,
        kind: super::result::PathKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime: Option<String>,
    },

    /// 图片展示。
    Image {
        src: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
    },

    /// 终端流式输出（绑定到一个 ToolCall 用于刷新）。
    Terminal {
        tool_call_id: CallId,
        kind: TerminalKind,
        /// `None` = 完整保留历史
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_lines: Option<u32>,
    },

    /// 进程列表（绑定到一个产生 process.list 结果的 ToolCall）。
    ProcessList {
        tool_call_id: CallId,
        max: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<String>,
    },

    /// 系统状态（CPU/Mem/Disk 等），绑定 ToolCall 用于实时刷新。
    SystemStats {
        tool_call_id: CallId,
        kinds: Vec<StatKind>,
    },

    /// 二次确认请求（服务端要求用户做选择）。
    Confirmation(ConfirmationBlock),
}

impl UIBlock {
    /// 该 block 是否要求用户交互（Confirmation / 带 Action 的 Confirm）。
    pub fn requires_user_action(&self) -> bool {
        matches!(self, UIBlock::Confirmation(_))
    }
}

// ------------------------------------------------------------------
// Chart
// ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Chart {
    Line {
        title: String,
        x_label: String,
        y_label: String,
        series: Vec<ChartSeries>,
    },
    Bar {
        title: String,
        x_label: String,
        y_label: String,
        bars: Vec<ChartBar>,
    },
    Points {
        title: String,
        series: Vec<ChartSeries>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartSeries {
    pub name: String,
    pub points: Vec<ChartPoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartBar {
    pub label: String,
    pub value: f64,
}

// ------------------------------------------------------------------
// Terminal / Process / System
// ------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalKind {
    Exec,
    Watch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatKind {
    Cpu,
    Memory,
    Disk,
    Network,
    Load,
    Uptime,
}

impl StatKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Memory => "memory",
            Self::Disk => "disk",
            Self::Network => "network",
            Self::Load => "load",
            Self::Uptime => "uptime",
        }
    }
}

// ------------------------------------------------------------------
// Confirmation Block / Action
// ------------------------------------------------------------------

/// 二次确认请求：服务端告诉前端"我将执行 X，但你先确认"。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfirmationBlock {
    pub request_id: RequestId,
    pub title: String,
    pub description: String,
    /// 行为后果（markdown 片段，逐条说明）。
    pub consequences: Vec<String>,
    pub options: Vec<ConfirmationOption>,
    /// `options[*].choice` 之一。Agent 选择 + 用户确认可以走不同分支。
    pub default_choice: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfirmationOption {
    /// 该选项回传给服务端时的 `choice` 值。
    pub choice: String,
    /// 显示在按钮上的文案。
    pub label: String,
    /// 选项的次级说明（hover 提示 / 副标题）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub style: ConfirmationStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationStyle {
    /// 主操作（蓝色 / 主按钮样式）。
    Primary,
    /// 危险操作（红色）。
    Danger,
    /// 次要（默认取消按钮样式）。
    Ghost,
}

// ------------------------------------------------------------------
// UIAction — 用户真实交互行为
// ------------------------------------------------------------------

/// 用户在 UI 上做的事：点按钮、确认、取消。**必须与 [`UIAction::Invoke`] /
/// [`UIAction::Confirm`] / [`UIAction::Cancel`] 一一对应地转回 [`ToolCall`]
/// 发回服务端**。
///
/// IMPORTANT: 前端永远不要"直接执行"任何东西，所有动作都映射到 `ToolCall`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UIAction {
    /// 按钮：调用某 tool。前端将映射成 `ToolCall { tool, arguments }`。
    Invoke {
        tool: String,
        #[serde(default)]
        arguments: serde_json::Value,
        label: String,
    },
    /// 确认框：用户选择了该 option 的 `choice`。
    Confirm {
        request_id: RequestId,
        choice: String,
    },
    /// 确认框：用户取消。
    Cancel {
        request_id: RequestId,
    },
}

impl UIAction {
    /// 把该 Action 转回服务端认识的 `ToolCall`。
    /// `Confirm` / `Cancel` 在 `system.continue` 这一协议约定里实现。
    pub fn to_tool_call(&self) -> ToolCall {
        match self {
            UIAction::Invoke { tool, arguments, .. } => ToolCall {
                call_id: CallId::new(),
                tool: tool.clone(),
                arguments: arguments.clone(),
                sandbox: None,
            },
            UIAction::Confirm { request_id, choice } => ToolCall {
                call_id: CallId::new(),
                tool: "system.continue".into(),
                arguments: serde_json::json!({
                    "request_id": request_id.as_str(),
                    "choice": choice,
                }),
                sandbox: None,
            },
            UIAction::Cancel { request_id } => ToolCall {
                call_id: CallId::new(),
                tool: "system.cancel".into(),
                arguments: serde_json::json!({ "request_id": request_id.as_str() }),
                sandbox: None,
            },
        }
    }
}

// ------------------------------------------------------------------
// 工具函数
// ------------------------------------------------------------------

impl ConfirmationBlock {
    /// 工具方法：生成一个简单的"是 / 否"确认（仅作为前端初始 UI 模板）。
    pub fn yes_no(request_id: RequestId, title: impl Into<String>) -> Self {
        Self {
            request_id,
            title: title.into(),
            description: String::new(),
            consequences: vec![],
            options: vec![
                ConfirmationOption {
                    choice: "confirm".into(),
                    label: "确认".into(),
                    description: None,
                    style: ConfirmationStyle::Primary,
                },
                ConfirmationOption {
                    choice: "cancel".into(),
                    label: "取消".into(),
                    description: None,
                    style: ConfirmationStyle::Ghost,
                },
            ],
            default_choice: "cancel".into(),
        }
    }
}

// `Result<UIBlock, _>` 之类用得上；保留 `Result` 别名做预防。
#[allow(dead_code)]
pub type Result<T> = std::result::Result<T, UIBlockError>;

/// UIBlock 解析/编码侧可产生的错误（Phase 1 仅做结构占位）。
#[derive(Debug, thiserror::Error)]
pub enum UIBlockError {
    #[error("invalid ui block: {0}")]
    Invalid(String),
}

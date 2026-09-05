//! # AION 协议层
//!
//! AION Agent OS Runtime 的协议数据层。Phase 1 范围：**纯数据类型**，
//! 不包含 `Tool` trait / `ToolRegistry` / 与 `cordis` 的任何耦合。
//!
//! Phase 2 时，`aion-services` 会增加对 `aion-protocol` 的依赖并加入
//! `Tool` trait + `ToolRuntime`；`aion-protocol` 自身将保持零运行时副作用。
//!
//! ## 模块
//!
//! | 模块 | 角色 |
//! |------|------|
//! | [`schema`] | JSON Schema 类型 + `validate(Value)` |
//! | [`tool`] | `ToolDefinition` + `Risk`（无 trait） |
//! | [`call`] | `ToolCall` + `CallId` + `ToolSandboxHint` |
//! | [`result`] | `ToolResult` + `ResultStatus` + `Artifact` + `RequestId` |
//! | [`ui`] | `UIBlock` + `UIAction` + `ConfirmationBlock` + `Chart` |
//! | [`session`] | `Session` + `Message` + `Role` + `StalledCall` |
//! | [`event`] | `AionEvent` + `AionEventKind`（协议层独立于 cordis::Event） |
//! | [`error`] | `ProtocolError` + `SchemaError` |
//!
//! ## 序列化约定
//!
//! 全部协议类型 `#[derive(Serialize, Deserialize)]`。
//! - 枚举带数据变体使用 `tag = "type"`，变体名 `rename_all = "snake_case"`。
//! - 纯枚举（风险等级 / 角色 / 状态机）使用 `rename_all = "snake_case"`，不带 tag。
//! - `serde_json::Value` 作为动态 payload 的标准容器（与 `Config` / `AgentTask.params`
//!   现有惯例一致）。
//!
//! ## 使用约定
//!
//! 前端 + 命令行 + 远程控制端都基于这套数据结构。Runtime 是唯一
//! 把 agent 决策变成 ToolCall 的地方，ToolCall 是唯一流入 Runtime 的
//! 指令。`UIAction` 必须被前端转回 `ToolCall`（`UIAction::to_tool_call`）
//! 而不是直接触发任何后端动作。

#![doc = "\u{0041}AION \u{4e0a}\u{4e00}\u{4ee3} Phase 1"]

pub mod call;
pub mod capability;
pub mod error;
pub mod event;
pub mod llm_schema;
pub mod result;
pub mod schema;
pub mod session;
pub mod tool;
pub mod ui;

/// 常用类型集中导出：
/// `use aion_protocol::prelude::*;`
pub mod prelude {
    pub use crate::call::{CallId, ToolCall, ToolSandboxHint};
    pub use crate::capability::CapabilityDefinition;
    pub use crate::error::{ProtocolError, SchemaError};
    pub use crate::event::{AionEvent, AionEventKind};
    pub use crate::result::{Artifact, ErrorKind, PathKind, RequestId, ResultStatus, ToolResult};
    pub use crate::schema::{JsonSchema, JsonSchemaDocument};
    pub use crate::session::{
        Message, MessageId, Role, Session, SessionId, SessionState, StalledCall,
    };
    pub use crate::tool::{Risk, ToolDefinition};
    pub use crate::ui::{
        Chart, ChartBar, ChartPoint, ChartSeries, ConfirmationBlock, ConfirmationOption,
        ConfirmationStyle, StatKind, TerminalKind, UIAction, UIBlock,
    };
}

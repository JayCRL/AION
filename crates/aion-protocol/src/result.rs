//! Tool 执行结果。
//!
//! 由 Runtime 在 Tool 调用之后返回。结构化载荷（`data`）便于前端按
//! [`UIBlock`](crate::ui::UIBlock) 渲染。`status` 决定主流程走向。
//!
//! 关联协议：Confirmation 流程在 `status: Pending` 时同时携带 `UIBlock::Confirmation`
//! + `request_id`，前端用 `UIAction::Confirm{request_id, choice}` 转回 `ToolCall`
//! (`tool: "system.continue"`, `arguments: { request_id, choice }`) 让 Runtime 继续。

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::call::CallId;
use crate::ui::UIBlock;

/// 一次 Tool 调用的结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: CallId,
    pub status: ResultStatus,

    /// 结构化载荷（success 时为工具产出的 data；error / pending 时可为空或附加上下文）。
    #[serde(default)]
    pub data: serde_json::Value,

    /// 产物（文件、URL、blob 等可外传的实体）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,

    /// 工具执行过程中产生的事件（UI 可按顺序渲染）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<UIBlock>,
}

/// Tool 执行状态。
///
/// 终结态：`Success / Error / Denied`。**需要用户介入**的中间态：`Pending`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResultStatus {
    /// 工具成功结束。`data` 携带产物，`artifacts`/`events` 可选。
    Success,

    /// 工具执行中失败；`data` 可携带部分结果（如果有）或错误详情。
    Error {
        kind: ErrorKind,
        message: String,
    },

    /// **需要二次确认**——Runtime 已挂起等待用户在 UI 中选择。
    ///
    /// `request_id` 与 [`UIBlock::Confirmation`](crate::ui::UIBlock::Confirmation)
    /// 共享同一值；前端需要配套发送 `UIBlock::Confirmation` 给用户看。
    Pending {
        request_id: RequestId,
        summary: String,
    },

    /// SecurityContext 拒绝：所需 capability 不在 Agent 的许可集。
    /// `data` 可为空。Runtime 通常不再回退，让 Agent 重新发起可满足的 ToolCall。
    Denied { cap: String, hint: String },
}

/// 工具执行错误的原因分类（便于前端以不同 UI 提示）。
///
/// 默认 `Internal`；`Timeout` 可在 UI 显示倒计时；`ExternalService`
/// 提示"依赖的下游服务挂了"等。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// 实现 bug 或未归类错误。
    Internal,
    /// 超过时间限制（例如 `terminal.exec` 超时）。
    Timeout,
    /// Agent/前端给的 `arguments` 不合 schema 或语义。
    InvalidInput,
    /// 目标资源不存在（文件 / 进程 / 设备节点）。
    NotFound,
    /// 工具调用了外部服务（HTTP / DB）失败。
    ExternalService,
    /// 依赖暂时不可用（adapter 在非 root / 非 Linux 上优雅降级时报）。
    Unavailable,
}

// ------------------------------------------------------------------
// Artifact
// ------------------------------------------------------------------

/// 工具产出的"可外传实体"：文件 / URL / blob。Phase 2 Tool 可填充。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Artifact {
    /// 本地路径（前端可渲染为"打开 / 下载"按钮）。
    Path {
        path: String,
        /// 仅作 UI 提示用，不影响内容。
        kind: PathKind,
        /// 字节数（`None` 表示未知）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },

    /// 远端 URL（OpenInBrowser / 下载）。
    Url {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime: Option<String>,
    },

    /// 内联 blob（小型可渲染内容，例如生成的简短图片 / 数据快照）。
    Blob {
        content_type: String,
        /// 标准 base64。
        base64: String,
        byte_size: u64,
    },
}

/// `Artifact::Path` 的来源/语义分类，用于前端"打开"按钮行为决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    /// 工具读取/产出来自现有源文件（只读）。
    Source,
    /// 工具产生的输出（可分享 / 保留 / 上传）。
    Generated,
    /// 需要被下载的目标（执行完成后，UI 应提供下载按钮）。
    Downloadable,
}

impl PathKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Generated => "generated",
            Self::Downloadable => "downloadable",
        }
    }
}

// ------------------------------------------------------------------
// RequestId — 与 ConfirmationBlock 配对的挂起请求 ID
// ------------------------------------------------------------------

/// Confirmation 配套 ID：在 `ResultStatus::Pending` 和 `UIBlock::Confirmation`、
/// `UIAction::Confirm` / `UIAction::Cancel` 之间共享。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub String);

impl RequestId {
    pub fn new() -> Self {
        Self(format!("req-{}", monotonic_suffix()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RequestId {
    fn default() -> Self { Self::new() }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for RequestId { fn from(s: &str) -> Self { Self(s.to_string()) } }
impl From<String> for RequestId { fn from(s: String) -> Self { Self(s) } }

// 与 call.rs 同源的单调计数器（独立 AtomicU64，所以两个 ID 类型各自单调）。
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

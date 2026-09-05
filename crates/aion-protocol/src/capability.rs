//! Capability 元数据（纯数据）。
//!
//! Phase "Capability 主线" 的协议层类型：**Capability 是给 Agent 看的目标导向
//! 接口面**（`web.view`、将来的 `video.view`），**Tool 是其下的叶子执行原语
//! （Provider）**。模型看见能力名与目标描述；执行时由服务层 `resolver` 把一次
//! 能力调用解析到某个 Provider（叶子工具）——见 `aion-services::capability`。
//!
//! 本文件只声明"一个能力长什么样"，不携带解析逻辑（解析是代码，在服务层）。

use serde::{Deserialize, Serialize};

use crate::schema::JsonSchemaDocument;
use crate::tool::Risk;

/// 依赖项的安装方式。
///
/// 能力广场「安装」= 把依赖的可执行软件补装到机器上。优先用户级下载（零 root），
/// apt 兜底（需 sudo，AION 不在内存放 sudo 密码，只走 `sudo -n` NOPASSWD，否则给手动提示）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstallMethod {
    /// 发行版包管理器安装（需 root）。AION 只尝试 `sudo -n apt-get install -y`；
    /// 探测到无 NOPASSWD 时返回"请手动执行"提示，不在本进程持密码。
    Apt { packages: Vec<String> },
    /// 下载独立二进制/归档到 `~/.local/bin/<to>`（零 root）。
    /// `extract=true`：内容为 tar（如 moli），解包后按文件名 `<to>` 找可执行文件放到位。
    Download { url: String, to: String, extract: bool },
}

/// 能力的一项运行时依赖：一个"可安装单元"。
///
/// 判断满足 = `binaries` 里任一名字在 PATH（含 `~/.local/bin`）命中可执行。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityDep {
    /// 人类可读标签（"媒体播放器" / "无头网页引擎"）。
    pub label: String,
    /// 候选可执行名：任一命中即视为已满足（如 `["mpv","ffplay","vlc"]`）。
    pub binaries: Vec<String>,
    /// 未命中时的安装方式。
    pub method: InstallMethod,
}

/// 一个 Capability 的描述：Agent 视角的"我能让用户达成什么目标"。
///
/// 与 [`crate::tool::ToolDefinition`] 的区别：
/// - Tool 是**动词粒度、可执行**的叶子（`web.fetch` / `terminal.exec`）；
/// - Capability 是**目标粒度、可解析**的接口（`web.view`——内部可能对应多个 Provider）。
///
/// 可被前端拉取渲染"我能做什么"；可被 Agent 以原生 `tools` 形式调用，
/// 命中后运行时经 resolver 落成一个具体 Tool 执行。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityDefinition {
    /// 全小写 snake_case，全局唯一（例如 `"web.view"`）。
    pub name: String,

    /// 一句话目标摘要（系统提示词清单 / 前端清单用，给模型看到"目标"）。
    pub summary: String,

    /// 给模型的详细说明：何时用、怎么填参、能达成什么。
    pub description: String,

    /// 输入参数 schema。Agent 收到后按此 schema 生成参数。
    pub input: JsonSchemaDocument,

    /// 解析到叶子工具执行前所需 capability 列表（与 ToolDefinition 同一语义）。
    pub required_caps: Vec<String>,

    /// 风险等级（透传：能力最终落在风险最高的叶子时按叶子判；此处做内省展示）。
    pub risk: Risk,

    /// 本能力可落到的叶子 Provider 工具名（内省：一能力多实现）。
    pub providers: Vec<String>,

    /// 本能力运行时依赖的外部软件（广场展示 + 安装器使用）。空 = 无外部依赖。
    #[serde(default)]
    pub deps: Vec<CapabilityDep>,
}

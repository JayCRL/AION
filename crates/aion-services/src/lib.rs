//! # AION 服务层 — System Services（基于 Cordis-RS Service）
//!
//! 每个服务都是一个 Cordis 服务（惰性启动、依赖注入、事件可观测），
//! 在调用 Linux Adapter 之前执行 **权限 & Capability 检查**：
//!
//! | 服务 | 职责 |
//! |------|------|
//! | [`process`]  | 进程管理（沙箱启动 / 退出事件 / kill） |
//! | [`fs`]       | 文件系统（路径白名单内的读写） |
//! | [`network`]  | 网络管理（目标白名单 + HTTP 抓取） |
//! | [`terminal`] | 终端 / IO（沙箱内执行命令并超时控制） |
//! | [`sandbox`]  | 沙箱 & 隔离（生成沙箱档案 / 能力报告） |
//! | [`storage`]  | 存储管理（租户配额） |
//! | [`device`]   | 设备管理（GPU/USB 枚举与访问控制） |
//! | [`model`]    | 模型服务（LLM 后端抽象，内置 Echo 后端） |

pub mod capability;
pub mod device;
pub mod error;
pub mod fs;
pub mod model;
pub mod network;
pub mod process;
pub mod provider;
pub mod sandbox;
pub mod security;
pub mod storage;
pub mod system;
pub mod terminal;
pub mod tool;

use aion_adapter::AdapterKit;

pub use capability::{
    image_mime, is_external_ext, is_text_ext, lower_ext, register_builtin_capabilities,
    viewer_icon, CapabilityRegistry, ResolvedProvider,
};
pub use error::{AionError, AionResult};
pub use provider::{backend_from_provider, LlmProtocol, LlmProvider, LlmProviderStore};
pub use sandbox::SandboxRequest;
pub use security::SecurityContext;
pub use tool::{populate_builtin_registry, Tool, ToolCallScope, ToolRegistry, ToolRuntime};

/// 一整套 AION 系统服务（共享同一个 AdapterKit）。
pub struct SystemServices {
    pub process: process::ProcessService,
    pub file: fs::FileService,
    pub network: network::NetworkService,
    pub terminal: terminal::TerminalService,
    pub sandbox: sandbox::SandboxService,
    pub storage: storage::StorageService,
    pub device: device::DeviceService,
    pub model: model::ModelService,
}

/// 构建平台默认的系统服务集合。
///
/// - `cgroup_root`：cgroup v2 挂载点（Linux 上 `/sys/fs/cgroup`）；
/// - `storage_root`：StorageService 根目录。
pub fn system_services(
    kit: &AdapterKit,
    storage_root: std::path::PathBuf,
    cgroup_root: std::path::PathBuf,
) -> SystemServices {
    SystemServices {
        process: process::ProcessService::new(kit.clone(), cgroup_root),
        file: fs::FileService::new(kit.clone()),
        network: network::NetworkService::new(kit.clone()),
        terminal: terminal::TerminalService::new(),
        sandbox: sandbox::SandboxService::new(kit.clone()),
        storage: storage::StorageService::new(storage_root),
        device: device::DeviceService::new(kit.clone()),
        model: model::ModelService::new(),
    }
}

/// 把整套服务注册到 Cordis 上下文。
/// 同时初始化 ToolRegistry + ToolRuntime 并注册 7 个内置 Tool。
pub fn provide_all(
    ctx: &cordis::Context,
    services: SystemServices,
) -> cordis::CordisResult<()> {
    ctx.provide(services.process)?;
    ctx.provide(services.file)?;
    ctx.provide(services.network)?;
    ctx.provide(services.sandbox)?;
    ctx.provide(services.storage)?;
    ctx.provide(services.device)?;
    ctx.provide(services.model)?;
    // terminal 依赖 process（通过 Service::inject 声明），放最后注册
    ctx.provide(services.terminal)?;

    // Phase 2: Tool 层
    let registry = tool::ToolRegistry::new();
    tool::populate_builtin_registry(&registry)?;
    let runtime = tool::ToolRuntime::new(std::sync::Arc::new(registry.clone()));
    ctx.provide(registry)?;
    ctx.provide(runtime)?;

    Ok(())
}

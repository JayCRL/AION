//! # Cordis-RS — AION 核心层运行时
//!
//! Cordis-RS 为 AION 提供 Agent 运行所需的「操作系统式」核心机制：
//!
//! | 模块 | 职责 |
//! |------|------|
//! | [`context`]  | 上下文管理（一切 API 的入口，派生子作用域） |
//! | [`service`]  | 服务注册与发现（`provide` / `require` / 依赖注入） |
//! | [`event`]    | 事件总线（`on` / `once` / `emit` / 监视通道） |
//! | [`plugin`]   | 插件系统（在独立子作用域中加载 / 卸载） |
//! | [`fiber`]    | Fiber 并发模型（作用域托管的任务，随作用域取消） |
//! | [`effect`]   | Effect 副作用与清理（作用域销毁时逆序执行） |
//! | [`scope`]    | 作用域隔离（生命周期与级联销毁） |
//! | [`logger`]   | 日志系统（分级输出 + 环形缓冲，可观测性） |
//! | [`config`]   | 配置系统（点路径取值 / 深合并 / 文件加载） |
//! | [`lifecycle`]| 生命周期状态机 |
//! | [`loader`]   | 加载器 / 分组（`App` 构建器 + 配置装载） |
//!
//! 典型用法见 `crates/aion`（AION 应用层）与 `tests/runtime.rs`。
//!
//! ```no_run
//! use cordis::prelude::*;
//!
//! # async fn demo() -> cordis::CordisResult<()> {
//! let ctx = cordis::App::new()
//!     .plugin(cordis::plugin_fn("hello", |ctx: Context| {
//!         Box::pin(async move {
//!             ctx.info("plugin loaded");
//!             Ok(())
//!         })
//!     }))
//!     .run()
//!     .await?;
//! ctx.dispose().await?;
//! # Ok(())
//! # }
//! ```

pub mod config;
pub mod context;
pub mod effect;
pub mod event;
pub mod fiber;
pub mod lifecycle;
pub mod loader;
pub mod logger;
pub mod plugin;
pub mod scope;
pub mod service;

/// 装箱后的 Future 别名，贯穿整个运行时的异步 API。
pub type BoxFut<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>;

/// Cordis 运行时错误。
#[derive(Debug, thiserror::Error)]
pub enum CordisError {
    #[error("service `{0}` not found")]
    ServiceNotFound(String),
    #[error("service `{0}` already provided")]
    ServiceAlreadyProvided(String),
    #[error("service `{0}` failed to start: {1}")]
    ServiceStartFailed(String, String),
    #[error("circular dependency detected at `{0}`")]
    CircularDependency(String),
    #[error("scope `{0}` is disposed")]
    ScopeDisposed(String),
    #[error("plugin `{0}` failed: {1}")]
    PluginFailed(String, String),
    #[error("config error: {0}")]
    Config(String),
    #[error("{0}")]
    Custom(String),
}

/// Cordis 运行时 Result 别名。
pub type CordisResult<T> = Result<T, CordisError>;

// 根路径重导出，便于 `cordis::Context` / `cordis::Service` 式引用
pub use config::Config;
pub use context::Context;
pub use effect::Effect;
pub use event::{Event, ListenerId};
pub use fiber::FiberHandle;
pub use lifecycle::LifecycleState;
pub use loader::App;
pub use logger::{Level, Logger};
pub use plugin::{plugin_fn, Plugin};
pub use scope::Scope;
pub use service::{Service, ServiceInfo, ServiceState};

/// 常用类型集中导出。
pub mod prelude {
    pub use crate::config::Config;
    pub use crate::context::Context;
    pub use crate::effect::Effect;
    pub use crate::event::{Event, ListenerId};
    pub use crate::fiber::FiberHandle;
    pub use crate::lifecycle::LifecycleState;
    pub use crate::loader::App;
    pub use crate::logger::{Level, Logger};
    pub use crate::plugin::{plugin_fn, Plugin};
    pub use crate::scope::Scope;
    pub use crate::service::{Service, ServiceInfo, ServiceState};
    pub use crate::{BoxFut, CordisError, CordisResult};
}

//! Tool 运行时（Phase 2）。
//!
//! Phase 2 Tool runtime: validates ToolCall (schema + capability), then
//! dispatches to the Tool implementation. The Tool is given a child
//! `cordis::Context` and a `ToolCallScope` (SecurityContext + CallId) so it
//! can call existing Services, but does NOT own the SecurityContext.
//!
//! Tool trait 签名按 Phase 1 已锁定的版本：见 [`Tool`]。

pub mod file;
pub mod process;
pub mod risk;
pub mod system;
pub mod terminal;
pub mod web;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cordis::{CordisError, CordisResult, Service};
use serde_json::Value;

use aion_protocol::error::SchemaError;
use aion_protocol::prelude::*;

use crate::security::SecurityContext;

// ---------------------------------------------------------------------------
// Tool trait
// ---------------------------------------------------------------------------

/// 工具 trait。
///
/// 签名（**已与用户确认锁定**）：
/// - `Tool::definition` 返回工具契约（名称 / 描述 / 输入 schema / 所需 capabilities / 风险）。
/// - `Tool::call` 接收 child-scoped `Context` + `args`，**不接收 SecurityContext**。
///   Tool 通过 `ctx.require::<ToolCallScope>()` 拿到 Runtime 注入的 SecurityContext，
///   再 `ctx.require::<FileService>()` 拿到受权限控制的 Service。
///   路径 / 网络的二次检查由 Service 内部完成，返回 AionError；Tool 转成 `ToolResult::Error`。
#[async_trait]
pub trait Tool: Send + Sync + 'static {
    fn definition(&self) -> &ToolDefinition;
    async fn call(
        &self,
        ctx: &cordis::Context,
        scope: &ToolCallScope,
        args: Value,
    ) -> ToolResult;
}

// ---------------------------------------------------------------------------
// ToolCallScope — Runtime 借给 Tool 的一次调用上下文
// ---------------------------------------------------------------------------

/// 一次 Tool 调用期间的上下文：SecurityContext + CallId。**按引用**传入
/// `Tool::call`，Tool 既不构造也不拥有 SecurityContext。
///
/// Phase 1 原设计是"Tool 不接收 SecurityContext"——Phase 2 落地时为
/// 了不重复向 cordis 注册同名 Service（`provide` 在同一 Context 里同名
/// 会冲突），改成"Tool 不构造 SecurityContext，但 Runtime 借出 scope
/// 引用，Tool 内部按需用 scope.security 调用 Service"。
pub struct ToolCallScope {
    pub security: SecurityContext,
    pub call_id: CallId,
}

// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

/// 工具注册表。
///
/// `aion-services::provide_all_system` 会建一个并塞进 Cordis，
/// 同时调用 `populate_builtin_registry` 把 7 个内置 Tool 注册进去。
#[derive(Default, Clone)]
pub struct ToolRegistry {
    inner: Arc<Mutex<BTreeMap<String, Arc<dyn Tool>>>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个 Tool。名字与现有 Tool 重名时返 `DuplicateName`。
    pub fn register<T: Tool + 'static>(&self, tool: T) -> CordisResult<()> {
        let def = tool.definition().clone();
        let name = def.name.clone();
        let mut guard = self.inner.lock().expect("tool registry poisoned");
        if guard.contains_key(&name) {
            return Err(CordisError::Custom(format!("duplicate tool `{name}`")));
        }
        guard.insert(name, Arc::new(tool));
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.inner
            .lock()
            .expect("tool registry poisoned")
            .get(name)
            .cloned()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("tool registry poisoned").len()
    }

    pub fn list(&self) -> Vec<ToolDefinition> {
        self.inner
            .lock()
            .expect("tool registry poisoned")
            .values()
            .map(|t| t.definition().clone())
            .collect()
    }
}

#[async_trait]
impl Service for ToolRegistry {
    fn name(&self) -> &'static str {
        "aion.tool.registry"
    }
    fn description(&self) -> &'static str {
        "Tool 注册表（Runtime 查找 Tool 实现用）"
    }
    async fn start(&self, _ctx: &cordis::Context) -> CordisResult<()> {
        Ok(())
    }
    async fn stop(&self, _ctx: &cordis::Context) -> CordisResult<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ToolRuntime — 执行 ToolCall
// ---------------------------------------------------------------------------

/// Tool 运行时。
///
/// 见本模块开头的执行链；任何阶段失败都映射成 `ToolResult` 的对应变体，
/// 不会返回 `Err`（这样 Agent 看到的是统一形状的 ToolResult，便于按状态路由）。
#[derive(Clone)]
pub struct ToolRuntime {
    registry: Arc<ToolRegistry>,
}

#[async_trait]
impl Service for ToolRuntime {
    fn name(&self) -> &'static str {
        "aion.tool.runtime"
    }
    fn description(&self) -> &'static str {
        "Tool 运行时：校验 args schema + capability，然后派发到 Tool 实现"
    }
    async fn start(&self, _ctx: &cordis::Context) -> CordisResult<()> {
        Ok(())
    }
    async fn stop(&self, _ctx: &cordis::Context) -> CordisResult<()> {
        Ok(())
    }
}

impl ToolRuntime {
    /// 从已有 Registry 构建 Runtime。共享内部 `Arc<ToolRegistry>`，零拷贝。
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }

    pub fn registry(&self) -> Arc<ToolRegistry> {
        Arc::clone(&self.registry)
    }

    /// 该 ToolCall 的**有效**风险等级。
    ///
    /// `terminal.exec` 在定义上静态为 `High`，但 web agent 只驱动它一个工具，
    /// 若一律按 High 处理，`ls`/`uptime` 也会弹确认框。因此对 `terminal.exec`
    /// 用命令串启发式分类器覆盖（命中危险模式才 `High`）；其余工具直接用
    /// 工具自己声明的 `definition().risk`。
    ///
    /// 这是「是否需要真人二次确认」的唯一判定入口，web 层在真正执行前调用。
    pub fn effective_risk(&self, call: &ToolCall) -> Risk {
        let def_risk = self
            .registry
            .get(&call.tool)
            .map(|t| t.definition().risk)
            .unwrap_or(Risk::Low);
        if call.tool == "terminal.exec" {
            if let Some(cmd) = call.arguments.get("command").and_then(|v| v.as_str()) {
                return risk::classify_terminal_command(cmd);
            }
            return def_risk;
        }
        def_risk
    }

    /// 执行一次 ToolCall。
    ///
    /// 分四阶段（任一阶段失败都映射成 `ToolResult` 对应变体，不返回 Err）：
    /// 1. **查找** Tool（不存在 → `ToolResult::Error NotFound`）
    /// 2. **参数 schema 校验**（不符 → `ToolResult::Error InvalidInput`）
    /// 3. **capability 校验**（缺 → `ToolResult::Denied {cap, hint}`）
    /// 4. **派发**——在子作用域里 `provide(ToolCallScope { security, call_id })`
    ///    然后调用 `Tool::call`。Tool 通过 ctx 自己拿 SecurityContext + 各种 Service。
    pub async fn execute(
        &self,
        ctx: &cordis::Context,
        call: ToolCall,
        security: SecurityContext,
    ) -> ToolResult {
        // 1) lookup
        let tool = match self.registry.get(&call.tool) {
            Some(t) => t,
            None => {
                return ToolResult::error(
                    aion_protocol::result::ErrorKind::NotFound,
                    format!("unknown tool `{}`", call.tool),
                );
            }
        };

        // 2) schema validate
        if let Err(e) = tool.definition().input.validate(&call.arguments) {
            return ToolResult::error(
                aion_protocol::result::ErrorKind::InvalidInput,
                schema_error_msg(&e),
            );
        }

        // 3) capability check
        for cap in tool.definition().required_caps.iter() {
            if security.check_cap(cap).is_err() {
                return ToolResult::denied(
                    cap,
                    format!(
                        "grant `{}` in the agent's SecurityContext to call `{}`",
                        cap, call.tool
                    ),
                );
            }
        }

        // 4) dispatch in a sub-scope that exposes SecurityContext as a Service
        let dispatch = ctx.child(format!("tool:{}", call.tool));
        let scope = ToolCallScope {
            security: security.clone(),
            call_id: call.call_id.clone(),
        };
        // 注意：ToolCallScope 是数据 struct，不是 cordis Service。
        // 直接把引用传给 Tool.call——避免 `dispatch.provide` 在同一进程
        // 重复注册同名 service 报"already provided"。

        // invoke tool
        let args = call.arguments.clone();
        tool.call(&dispatch, &scope, args).await
    }

    /// 串行执行一组 ToolCall。Phase 3 Agent 循环会用到。
    pub async fn execute_many(
        &self,
        ctx: &cordis::Context,
        calls: Vec<ToolCall>,
        security: SecurityContext,
    ) -> Vec<ToolResult> {
        let mut out = Vec::with_capacity(calls.len());
        for call in calls {
            out.push(self.execute(ctx, call, security.clone()).await);
        }
        out
    }
}

/// 在已建好的 Registry 上注册 7 个内置 Tool。
///
/// 不需要 ctx——纯数据操作。`provide_all_system` 在 `ctx.provide(registry)`
/// 之前调用它。
pub fn populate_builtin_registry(reg: &ToolRegistry) -> CordisResult<()> {
    use aion_protocol::result::ErrorKind;
    fn err<E: std::fmt::Display>(e: E) -> CordisError {
        CordisError::Custom(format!("builtin tool register: {e}"))
    }

    reg.register(file::FileReadTool::new()).map_err(err)?;
    reg.register(file::FileWriteTool::new()).map_err(err)?;
    reg.register(file::FileListTool::new()).map_err(err)?;
    reg.register(process::ProcessListTool::new()).map_err(err)?;
    reg.register(process::ProcessStartTool::new()).map_err(err)?;
    reg.register(terminal::TerminalExecTool::new()).map_err(err)?;
    reg.register(system::SystemStatsTool::new()).map_err(err)?;
    reg.register(web::WebFetchTool::new()).map_err(err)?;
    // Silence the unused ErrorKind import warning if any; the symbol is used inline.
    let _ = ErrorKind::Internal;
    Ok(())
}

// ---------------------------------------------------------------------------
// SchemaError 错误信息格式化
// ---------------------------------------------------------------------------

fn schema_error_msg(e: &SchemaError) -> String {
    use aion_protocol::error::SchemaError as E;
    match e {
        E::TypeMismatch { at, expected, got } => {
            format!("type mismatch at `{at}`: expected `{expected}`, got `{got}`")
        }
        E::MissingRequired { at, name } => {
            format!("missing required `{name}` at `{at}`")
        }
        E::NumberOutOfRange { at, got, min, max } => {
            format!("number at `{at}` out of range: {got} (min={min:?}, max={max:?})")
        }
        E::IntegerOutOfRange { at, got, min, max } => {
            format!("integer at `{at}` out of range: {got} (min={min:?}, max={max:?})")
        }
        E::StringLength { at, got, min, max } => {
            format!("string at `{at}` length out of range: {got} (min={min:?}, max={max:?})")
        }
        E::RefUnknown { name } => format!("unknown schema reference `#{name}`"),
        E::SchemaTooDeep { depth, max } => {
            format!("schema nesting too deep ({depth} > {max})")
        }
    }
}

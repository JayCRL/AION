//! Context：Cordis-RS 上下文，一切 API 的入口。
//!
//! `Context` 是可克隆的句柄，绑定一个[`crate::scope::Scope`]；
//! 通过 `child()` 派生子作用域，实现「作用域隔离」与「级联销毁」。

use std::any::TypeId;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;

use crate::config::Config;
use crate::effect::Effect;
use crate::event::{Event, EventBus, ListenerId, Payload};
use crate::fiber::FiberHandle;
use crate::lifecycle::LifecycleState;
use crate::logger::Logger;
use crate::plugin::Plugin;
use crate::scope::Scope;
use crate::service::{Registry, Service, ServiceInfo};
use crate::{BoxFut, CordisError, CordisResult};

/// 全局共享状态（所有 Context 克隆共享）。
pub struct RootState {
    pub(crate) bus: EventBus,
    pub(crate) registry: Registry,
    pub(crate) logger: Logger,
    pub(crate) config: RwLock<Config>,
    pub(crate) next_scope: AtomicU64,
    pub(crate) next_fiber: AtomicU64,
}

/// Cordis 上下文句柄。
#[derive(Clone)]
pub struct Context {
    pub(crate) root: Arc<RootState>,
    pub(crate) scope: Arc<Scope>,
}

impl Context {
    /// 创建根上下文。
    pub fn root(config: Config, logger: Logger) -> Context {
        let root = Arc::new(RootState {
            bus: EventBus::new(),
            registry: Registry::default(),
            logger,
            config: RwLock::new(config),
            next_scope: AtomicU64::new(1),
            next_fiber: AtomicU64::new(1),
        });
        let scope = Scope::new(0, "root", None);
        Context { root, scope }
    }

    /// 派生子作用域（继承全局状态，独立生命周期）。
    pub fn child(&self, name: impl Into<String>) -> Context {
        let id = self.root.next_scope.fetch_add(1, Ordering::Relaxed);
        let scope = Scope::new(id, name, Some(Arc::downgrade(&self.scope)));
        self.scope.add_child(&scope);
        Context {
            root: self.root.clone(),
            scope,
        }
    }

    // ------------------------------------------------------------------
    // 访问器
    // ------------------------------------------------------------------

    pub fn scope_id(&self) -> u64 {
        self.scope.id()
    }

    pub fn scope_name(&self) -> &str {
        self.scope.name()
    }

    pub fn state(&self) -> LifecycleState {
        self.scope.state()
    }

    pub fn is_root(&self) -> bool {
        self.scope.parent_name().is_none()
    }

    /// 父作用域（若仍存活）。
    pub fn parent(&self) -> Option<Context> {
        // 通过 root 无法直接拿父 Arc；Scope 只保留 Weak。此处用 children 无法反查，
        // 因此 Context 只暴露 parent_name；如需父句柄请在创建时自行保存。
        None
    }

    /// 作用域是否仍然活跃（未销毁）。
    pub fn is_active(&self) -> bool {
        matches!(
            self.scope.state(),
            LifecycleState::Created | LifecycleState::Starting | LifecycleState::Started | LifecycleState::Failed
        )
    }

    pub(crate) fn ensure_active(&self) -> CordisResult<()> {
        if self.is_active() {
            Ok(())
        } else {
            Err(CordisError::ScopeDisposed(self.scope.name().to_string()))
        }
    }

    // ------------------------------------------------------------------
    // 日志 / 配置
    // ------------------------------------------------------------------

    pub fn logger(&self) -> &Logger {
        &self.root.logger
    }

    /// 当前配置快照。
    pub fn config(&self) -> Config {
        self.root.config.read().expect("config poisoned").clone()
    }

    /// 更新配置（进程内生效）。
    pub fn update_config(&self, f: impl FnOnce(&mut Config)) {
        let mut cfg = self.root.config.write().expect("config poisoned");
        f(&mut cfg);
    }

    // 便捷日志（自动携带作用域名）。
    pub fn trace(&self, message: impl Into<String>) {
        self.root.logger.trace(self.scope.name(), message);
    }
    pub fn debug(&self, message: impl Into<String>) {
        self.root.logger.debug(self.scope.name(), message);
    }
    pub fn info(&self, message: impl Into<String>) {
        self.root.logger.info(self.scope.name(), message);
    }
    pub fn warn(&self, message: impl Into<String>) {
        self.root.logger.warn(self.scope.name(), message);
    }
    pub fn error(&self, message: impl Into<String>) {
        self.root.logger.error(self.scope.name(), message);
    }

    // ------------------------------------------------------------------
    // 事件
    // ------------------------------------------------------------------

    /// 订阅事件（监听器登记到当前作用域，随作用域销毁自动移除）。
    pub fn on<F>(&self, name: &str, handler: F) -> ListenerId
    where
        F: Fn(Context, Event) -> BoxFut<()> + Send + Sync + 'static,
    {
        let id = self.root.bus.on(name, handler);
        self.scope.track_listener(id);
        id
    }

    /// 订阅一次性事件。
    pub fn once<F>(&self, name: &str, handler: F) -> ListenerId
    where
        F: Fn(Context, Event) -> BoxFut<()> + Send + Sync + 'static,
    {
        let id = self.root.bus.once(name, handler);
        self.scope.track_listener(id);
        id
    }

    /// 取消监听。
    pub fn off(&self, id: ListenerId) {
        self.root.bus.off(id);
    }

    /// 发出事件（无负载）。
    pub async fn emit(&self, name: &str) -> CordisResult<()> {
        self.ensure_active()?;
        self.root.bus.emit(self.clone(), name, None).await;
        Ok(())
    }

    /// 发出事件（带负载）。
    pub async fn emit_with(&self, name: &str, payload: Payload) -> CordisResult<()> {
        self.ensure_active()?;
        self.root.bus.emit(self.clone(), name, Some(payload)).await;
        Ok(())
    }

    /// 订阅监视通道（观测所有事件，与监听器无关）。
    pub fn monitor(&self) -> broadcast::Receiver<Event> {
        self.root.bus.subscribe_monitor()
    }

    /// 当前事件监听器总数（可观测性）。
    pub fn listener_count(&self) -> usize {
        self.root.bus.listener_count()
    }

    // ------------------------------------------------------------------
    // Fiber / Effect
    // ------------------------------------------------------------------

    /// 在当前作用域内派生一个 Fiber（作用域销毁时自动 abort）。
    pub fn spawn<F, T>(&self, name: impl Into<String>, fut: F) -> FiberHandle
    where
        F: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let join = tokio::spawn(async move {
            let _ = fut.await;
        });
        let id = self.root.next_fiber.fetch_add(1, Ordering::Relaxed);
        let handle = FiberHandle::new(id, name, &join);
        self.scope.track_fiber(handle.clone());
        handle
    }

    /// 登记副作用清理函数（作用域销毁时逆序执行）。
    pub fn effect(
        &self,
        name: impl Into<String>,
        cleanup: impl Fn() -> BoxFut<()> + Send + Sync + 'static,
    ) {
        self.scope.push_effect(Effect::new(name, cleanup));
    }

    // ------------------------------------------------------------------
    // 服务（注册 / 发现 / DI）
    // ------------------------------------------------------------------

    /// 注册服务。服务默认处于 Pending，首次 `require` 时惰性启动。
    pub fn provide<S: Service>(&self, svc: S) -> CordisResult<()> {
        self.ensure_active()?;
        let name = svc.name();
        self.root.registry.provide(svc, self.scope.id())?;
        self.scope.track_service(name.to_string());
        self.debug(format!("service `{name}` provided"));
        Ok(())
    }

    /// 按类型获取服务（未启动则惰性启动，依赖自动解析）。
    pub async fn require<S: Service>(&self) -> CordisResult<Arc<S>> {
        self.ensure_active()?;
        let type_id = TypeId::of::<S>();
        let type_name = std::any::type_name::<S>();
        let snapshot = self
            .root
            .registry
            .snapshot_by_type(&type_id)
            .ok_or_else(|| CordisError::ServiceNotFound(type_name.to_string()))?;
        let mut stack = Vec::new();
        self.root
            .registry
            .ensure_started(&snapshot.name, self, &mut stack)
            .await?;
        let snapshot = self
            .root
            .registry
            .snapshot_by_type(&type_id)
            .ok_or_else(|| CordisError::ServiceNotFound(type_name.to_string()))?;
        snapshot
            .instance
            .clone()
            .downcast::<S>()
            .map_err(|_| CordisError::Custom(format!("service `{}` type mismatch", snapshot.name)))
    }

    /// 按名称获取服务（依赖注入）。
    pub async fn require_named(&self, name: &str) -> CordisResult<Arc<dyn Service>> {
        self.ensure_active()?;
        let snapshot = self
            .root
            .registry
            .snapshot_by_name(name)
            .ok_or_else(|| CordisError::ServiceNotFound(name.to_string()))?;
        let mut stack = Vec::new();
        self.root
            .registry
            .ensure_started(name, self, &mut stack)
            .await?;
        Ok(snapshot.service)
    }

    /// 依赖注入：按名称批量解析服务。
    pub async fn inject(&self, names: &[&str]) -> CordisResult<Vec<Arc<dyn Service>>> {
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            out.push(self.require_named(name).await?);
        }
        Ok(out)
    }

    /// 服务清单（可观测性）。
    pub fn list_services(&self) -> Vec<ServiceInfo> {
        self.root.registry.list()
    }

    /// 服务状态查询。
    pub fn service_state(&self, name: &str) -> Option<crate::service::ServiceState> {
        self.root.registry.state_of(name)
    }

    // ------------------------------------------------------------------
    // 插件
    // ------------------------------------------------------------------

    /// 加载插件：在独立子作用域中执行 `apply`；失败则销毁该作用域。
    pub async fn plugin<P: Plugin>(&self, plugin: P) -> CordisResult<()> {
        self.plugin_arc(Arc::new(plugin)).await
    }

    /// 加载插件（Arc 形式）。
    pub async fn plugin_arc(&self, plugin: Arc<dyn Plugin>) -> CordisResult<()> {
        self.ensure_active()?;
        let child = self.child(format!("plugin:{}", plugin.name()));
        self.info(format!("loading plugin `{}`", plugin.name()));
        match plugin.apply(child.clone()).await {
            Ok(()) => {
                self.info(format!("plugin `{}` loaded", plugin.name()));
                let _ = self
                    .emit(crate::lifecycle::lifecycle_events::STARTED)
                    .await;
                Ok(())
            }
            Err(e) => {
                let _ = child.dispose().await;
                Err(CordisError::PluginFailed(plugin.name().to_string(), e.to_string()))
            }
        }
    }

    // ------------------------------------------------------------------
    // 生命周期
    // ------------------------------------------------------------------

    /// 销毁当前作用域：级联销毁子作用域 → 移除监听器 → 停止归属服务 →
    /// abort Fiber → 逆序执行 Effect。幂等。
    pub async fn dispose(&self) -> CordisResult<()> {
        if !self.scope.try_begin_dispose() {
            return Ok(());
        }
        self.debug("scope disposing");
        // 1. 子作用域先销毁
        for child in self.scope.children_alive() {
            Box::pin(
                Context {
                    root: self.root.clone(),
                    scope: child,
                }
                .dispose(),
            )
            .await?;
        }
        // 2. 移除事件监听器
        for id in self.scope.take_listeners() {
            self.root.bus.off(id);
        }
        // 3. 停止归属服务
        for name in self.scope.take_services() {
            if let Err(e) = self.root.registry.stop_one(&name, self).await {
                self.warn(format!("service `{name}` stop failed: {e}"));
            }
        }
        // 4. 取消 Fiber
        for handle in self.scope.take_fibers() {
            handle.abort();
        }
        // 5. 逆序执行 Effect
        while let Some(effect) = self.scope.pop_effect() {
            self.debug(format!("effect `{}` running", effect.name));
            effect.run().await;
        }
        self.scope.set_state(LifecycleState::Disposed);
        let payload: Payload = Arc::new(self.scope.name().to_string());
        let _ = self
            .root
            .bus
            .emit(
                Context {
                    root: self.root.clone(),
                    scope: self.scope.clone(),
                },
                crate::lifecycle::lifecycle_events::SCOPE_DISPOSED,
                Some(payload),
            )
            .await;
        Ok(())
    }
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("scope", &self.scope.name())
            .field("scope_id", &self.scope.id())
            .field("state", &self.scope.state().as_str())
            .finish()
    }
}

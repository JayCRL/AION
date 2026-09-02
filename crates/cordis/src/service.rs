//! 服务注册与发现：`provide` 注册，`require` 按需惰性启动（依赖自动解析）。

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::watch;

use crate::context::Context;
use crate::{CordisError, CordisResult};

/// AION 服务 trait：实现方声明名称、依赖（DI）与启停逻辑。
#[async_trait]
pub trait Service: Send + Sync + 'static {
    /// 服务名（注册与依赖注入时使用）。
    fn name(&self) -> &'static str;

    /// 服务描述。
    fn description(&self) -> &'static str {
        ""
    }

    /// 依赖的服务名列表（启动前自动解析并启动）。
    fn inject(&self) -> Vec<&'static str> {
        Vec::new()
    }

    /// 服务启动（首次被 require 时惰性调用）。
    async fn start(&self, ctx: &Context) -> CordisResult<()> {
        let _ = ctx;
        Ok(())
    }

    /// 服务停止（所属作用域销毁时调用）。
    async fn stop(&self, ctx: &Context) -> CordisResult<()> {
        let _ = ctx;
        Ok(())
    }
}

/// 服务运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Pending,
    Starting,
    Started,
    Stopping,
    Stopped,
    Failed,
}

impl ServiceState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceState::Pending => "pending",
            ServiceState::Starting => "starting",
            ServiceState::Started => "started",
            ServiceState::Stopping => "stopping",
            ServiceState::Stopped => "stopped",
            ServiceState::Failed => "failed",
        }
    }
}

/// 服务元信息（用于 `aion services` 与可观测性展示）。
#[derive(Debug, Clone)]
pub struct ServiceInfo {
    pub name: String,
    pub type_name: String,
    pub state: ServiceState,
    pub owner_scope: u64,
    pub deps: Vec<String>,
}

pub(crate) struct Entry {
    pub name: String,
    pub type_name: String,
    pub instance: Arc<dyn Any + Send + Sync>,
    pub service: Arc<dyn Service>,
    pub state_tx: watch::Sender<ServiceState>,
    pub state_rx: watch::Receiver<ServiceState>,
    pub owner_scope: u64,
    pub deps: Vec<String>,
    pub last_error: Arc<Mutex<Option<String>>>,
    pub start_lock: Arc<tokio::sync::Mutex<()>>,
}

pub(crate) struct EntrySnapshot {
    pub name: String,
    pub instance: Arc<dyn Any + Send + Sync>,
    pub service: Arc<dyn Service>,
    pub state_tx: watch::Sender<ServiceState>,
    pub state_rx: watch::Receiver<ServiceState>,
    pub owner_scope: u64,
    pub deps: Vec<String>,
    pub last_error: Arc<Mutex<Option<String>>>,
    pub start_lock: Arc<tokio::sync::Mutex<()>>,
}

impl Entry {
    fn snapshot(&self) -> EntrySnapshot {
        EntrySnapshot {
            name: self.name.clone(),
            instance: self.instance.clone(),
            service: self.service.clone(),
            state_tx: self.state_tx.clone(),
            state_rx: self.state_rx.clone(),
            owner_scope: self.owner_scope,
            deps: self.deps.clone(),
            last_error: self.last_error.clone(),
            start_lock: self.start_lock.clone(),
        }
    }
}

#[derive(Default)]
pub(crate) struct Registry {
    by_type: Mutex<HashMap<TypeId, Entry>>,
    by_name: Mutex<HashMap<String, TypeId>>,
    start_order: Mutex<Vec<String>>,
}

impl Registry {
    pub(crate) fn provide<S: Service>(&self, svc: S, owner_scope: u64) -> CordisResult<()> {
        let type_id = TypeId::of::<S>();
        let name = svc.name().to_string();
        let type_name = std::any::type_name::<S>().to_string();
        let deps: Vec<String> = svc.inject().iter().map(|s| s.to_string()).collect();

        let arc = Arc::new(svc);
        let instance: Arc<dyn Any + Send + Sync> = arc.clone();
        let service: Arc<dyn Service> = arc;

        let mut by_type = self.by_type_lock();
        if by_type.contains_key(&type_id) {
            return Err(CordisError::ServiceAlreadyProvided(name));
        }
        if self
            .by_name
            .lock()
            .expect("registry by_name poisoned")
            .contains_key(&name)
        {
            return Err(CordisError::ServiceAlreadyProvided(name));
        }
        let (state_tx, state_rx) = watch::channel(ServiceState::Pending);
        let entry = Entry {
            name: name.clone(),
            type_name,
            instance,
            service,
            state_tx,
            state_rx,
            owner_scope,
            deps,
            last_error: Arc::new(Mutex::new(None)),
            start_lock: Arc::new(tokio::sync::Mutex::new(())),
        };
        by_type.insert(type_id, entry);
        self.by_name
            .lock()
            .expect("registry by_name poisoned")
            .insert(name.clone(), type_id);
        self.start_order.lock().expect("registry order poisoned").push(name);
        Ok(())
    }

    fn by_type_lock(&self) -> std::sync::MutexGuard<'_, HashMap<TypeId, Entry>> {
        self.by_type.lock().expect("registry by_type poisoned")
    }

    pub(crate) fn snapshot_by_type(&self, type_id: &TypeId) -> Option<EntrySnapshot> {
        self.by_type_lock().get(type_id).map(Entry::snapshot)
    }

    pub(crate) fn snapshot_by_name(&self, name: &str) -> Option<EntrySnapshot> {
        let type_id = *self
            .by_name
            .lock()
            .expect("registry by_name poisoned")
            .get(name)?;
        self.snapshot_by_type(&type_id)
    }

    pub(crate) fn state_of(&self, name: &str) -> Option<ServiceState> {
        self.snapshot_by_name(name).map(|s| *s.state_rx.borrow())
    }

    /// 服务清单（按注册顺序）。
    pub fn list(&self) -> Vec<ServiceInfo> {
        let order = self.start_order.lock().expect("registry order poisoned");
        let by_type = self.by_type_lock();
        order
            .iter()
            .filter_map(|name| {
                let tid = *self
                    .by_name
                    .lock()
                    .expect("registry by_name poisoned")
                    .get(name)?;
                let e = by_type.get(&tid)?;
                Some(ServiceInfo {
                    name: e.name.clone(),
                    type_name: e.type_name.clone(),
                    state: *e.state_rx.borrow(),
                    owner_scope: e.owner_scope,
                    deps: e.deps.clone(),
                })
            })
            .collect()
    }

    /// 确保服务已启动：按需解析依赖并调用 `start`。
    ///
    /// `stack` 用于环形依赖检测，调用方传入空 Vec。
    pub(crate) async fn ensure_started(
        &self,
        name: &str,
        ctx: &Context,
        stack: &mut Vec<String>,
    ) -> CordisResult<()> {
        let Some(snap) = self.snapshot_by_name(name) else {
            return Err(CordisError::ServiceNotFound(name.to_string()));
        };
        loop {
            let state = *snap.state_rx.borrow();
            match state {
                ServiceState::Started => return Ok(()),
                ServiceState::Failed => {
                    let err = snap
                        .last_error
                        .lock()
                        .expect("last_error poisoned")
                        .clone()
                        .unwrap_or_default();
                    return Err(CordisError::ServiceStartFailed(name.to_string(), err));
                }
                ServiceState::Starting | ServiceState::Stopping => {
                    let mut rx = snap.state_rx.clone();
                    rx.changed()
                        .await
                        .map_err(|_| CordisError::Custom(format!("service `{name}` dropped")))?;
                    continue;
                }
                ServiceState::Pending | ServiceState::Stopped => break,
            }
        }

        if stack.iter().any(|s| s == name) {
            return Err(CordisError::CircularDependency(name.to_string()));
        }
        stack.push(name.to_string());

        let lock = {
            let _g = self.by_type_lock(); // 与 provide 互斥
            snap.start_lock.clone()
        };
        let _guard = lock.lock().await;

        // 双重检查：可能已被其他 require 启动
        let state = *snap.state_rx.borrow();
        match state {
            ServiceState::Started => {
                stack.pop();
                return Ok(());
            }
            ServiceState::Pending | ServiceState::Stopped => {}
            ServiceState::Starting | ServiceState::Stopping | ServiceState::Failed => {}
        }

        let _ = snap.state_tx.send(ServiceState::Starting);
        ctx.debug(format!("service `{name}` starting"));

        // 解析依赖（DI / Inject）
        for dep in snap.deps.clone() {
            Box::pin(self.ensure_started(&dep, ctx, stack)).await?;
        }
        stack.pop();

        match snap.service.start(ctx).await {
            Ok(()) => {
                let _ = snap.state_tx.send(ServiceState::Started);
                let _ = ctx
                    .emit_with(crate::lifecycle::lifecycle_events::SERVICE_STARTED, {
                        let payload: crate::event::Payload = Arc::new(name.to_string());
                        payload
                    })
                    .await;
                ctx.info(format!("service `{name}` started"));
                Ok(())
            }
            Err(e) => {
                *snap.last_error.lock().expect("last_error poisoned") = Some(e.to_string());
                let _ = snap.state_tx.send(ServiceState::Failed);
                Err(CordisError::ServiceStartFailed(name.to_string(), e.to_string()))
            }
        }
    }

    /// 停止单个服务（幂等）。
    pub(crate) async fn stop_one(&self, name: &str, ctx: &Context) -> CordisResult<()> {
        let Some(snap) = self.snapshot_by_name(name) else {
            return Ok(());
        };
        let state = *snap.state_rx.borrow();
        match state {
            ServiceState::Started => {}
            ServiceState::Pending => {
                let _ = snap.state_tx.send(ServiceState::Stopped);
                return Ok(());
            }
            _ => return Ok(()),
        }
        let _ = snap.state_tx.send(ServiceState::Stopping);
        let result = snap.service.stop(ctx).await;
        let _ = snap.state_tx.send(ServiceState::Stopped);
        ctx.info(format!("service `{name}` stopped"));
        result
    }
}

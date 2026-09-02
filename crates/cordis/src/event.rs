//! 事件总线：`on` / `once` / `off` / `emit` + 监视通道（可观测性）。

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

use crate::context::Context;
use crate::BoxFut;

/// 事件负载：类型擦除的共享数据。
pub type Payload = Arc<dyn Any + Send + Sync>;

/// 事件处理器。
pub type Handler = Arc<dyn Fn(Context, Event) -> BoxFut<()> + Send + Sync>;

/// 监听器 ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListenerId(pub u64);

/// 一个事件。
#[derive(Clone)]
pub struct Event {
    pub name: String,
    pub payload: Option<Payload>,
    /// 发出事件时所在作用域的名称。
    pub scope: String,
    /// 全局递增序号。
    pub seq: u64,
}

impl Event {
    /// 取出指定类型的负载。
    pub fn payload<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.payload.as_ref()?.clone().downcast::<T>().ok()
    }

    pub fn is(&self, name: &str) -> bool {
        self.name == name
    }
}

impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Event")
            .field("name", &self.name)
            .field("scope", &self.scope)
            .field("seq", &self.seq)
            .field("has_payload", &self.payload.is_some())
            .finish()
    }
}

struct Listener {
    id: ListenerId,
    handler: Handler,
    once: bool,
}

#[derive(Default)]
struct BusInner {
    listeners: HashMap<String, Vec<Listener>>,
    next_id: AtomicU64,
}

/// 事件总线。处理器 panic 会被捕获并转为 error 日志，不影响其他监听器。
#[derive(Clone, Default)]
pub struct EventBus {
    inner: Arc<Mutex<BusInner>>,
    monitor: Option<Arc<broadcast::Sender<Event>>>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        EventBus {
            inner: Arc::new(Mutex::new(BusInner::default())),
            monitor: Some(Arc::new(tx)),
        }
    }

    /// 订阅事件。
    pub fn on(
        &self,
        name: &str,
        handler: impl Fn(Context, Event) -> BoxFut<()> + Send + Sync + 'static,
    ) -> ListenerId {
        self.push(name, handler, false)
    }

    /// 订阅一次性事件（触发后自动移除）。
    pub fn once(
        &self,
        name: &str,
        handler: impl Fn(Context, Event) -> BoxFut<()> + Send + Sync + 'static,
    ) -> ListenerId {
        self.push(name, handler, true)
    }

    fn push(
        &self,
        name: &str,
        handler: impl Fn(Context, Event) -> BoxFut<()> + Send + Sync + 'static,
        once: bool,
    ) -> ListenerId {
        let mut inner = self.inner.lock().expect("event bus poisoned");
        let id = ListenerId(inner.next_id.fetch_add(1, Ordering::Relaxed));
        inner
            .listeners
            .entry(name.to_string())
            .or_default()
            .push(Listener {
                id,
                handler: Arc::new(handler),
                once,
            });
        id
    }

    /// 取消监听。
    pub fn off(&self, id: ListenerId) {
        let mut inner = self.inner.lock().expect("event bus poisoned");
        for list in inner.listeners.values_mut() {
            list.retain(|l| l.id != id);
        }
    }

    /// 当前监听器总数。
    pub fn listener_count(&self) -> usize {
        let inner = self.inner.lock().expect("event bus poisoned");
        inner.listeners.values().map(|v| v.len()).sum()
    }

    /// 订阅监视通道：所有事件（无论是否有人监听）都会广播到这里，用于可观测性。
    pub fn subscribe_monitor(&self) -> broadcast::Receiver<Event> {
        match &self.monitor {
            Some(tx) => tx.subscribe(),
            None => unreachable!("EventBus always has a monitor sender"),
        }
    }

    /// 发出事件：通知监视通道并依序调用监听器。
    pub async fn emit(&self, ctx: Context, name: &str, payload: Option<Payload>) {
        let seq;
        let handlers: Vec<Handler>;
        {
            let mut inner = self.inner.lock().expect("event bus poisoned");
            seq = inner.next_id.fetch_add(1, Ordering::Relaxed);
            handlers = match inner.listeners.get_mut(name) {
                Some(list) => {
                    let taken: Vec<Handler> = list
                        .iter()
                        .map(|l| l.handler.clone())
                        .collect();
                    list.retain(|l| !l.once);
                    taken
                }
                None => Vec::new(),
            };
        }
        let event = Event {
            name: name.to_string(),
            payload,
            scope: ctx.scope_name().to_string(),
            seq,
        };
        if let Some(tx) = &self.monitor {
            let _ = tx.send(event.clone());
        }
        for handler in handlers {
            let fut = handler(ctx.clone(), event.clone());
            let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(fut))
                .await;
            if let Err(panic) = outcome {
                let msg = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "<non-string panic>".into());
                ctx.error(format!("event handler for `{name}` panicked: {msg}"));
            }
        }
    }
}

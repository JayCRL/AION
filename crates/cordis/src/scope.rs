//! 作用域：隔离单元，承载 Fiber / Effect / 监听器 / 服务归属，支持级联销毁。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crate::effect::Effect;
use crate::event::ListenerId;
use crate::fiber::FiberHandle;
use crate::lifecycle::LifecycleState;

/// 一个作用域。作用域之间构成树；父作用域销毁时子作用域先被销毁。
#[derive(Debug)]
pub struct Scope {
    id: u64,
    name: String,
    parent: Option<Weak<Scope>>,
    children: Mutex<Vec<Weak<Scope>>>,
    fibers: Mutex<Vec<FiberHandle>>,
    effects: Mutex<Vec<Effect>>,
    listeners: Mutex<Vec<ListenerId>>,
    services: Mutex<Vec<String>>,
    state: AtomicU8,
}

impl Scope {
    pub(crate) fn new(id: u64, name: impl Into<String>, parent: Option<Weak<Scope>>) -> Arc<Scope> {
        Arc::new(Scope {
            id,
            name: name.into(),
            parent,
            children: Mutex::new(Vec::new()),
            fibers: Mutex::new(Vec::new()),
            effects: Mutex::new(Vec::new()),
            listeners: Mutex::new(Vec::new()),
            services: Mutex::new(Vec::new()),
            state: AtomicU8::new(LifecycleState::Created as u8),
        })
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn state(&self) -> LifecycleState {
        LifecycleState::from_u8(self.state.load(Ordering::Relaxed))
    }

    pub(crate) fn set_state(&self, state: LifecycleState) {
        self.state.store(state as u8, Ordering::Relaxed);
    }

    pub fn parent_name(&self) -> Option<String> {
        self.parent.as_ref().and_then(|p| p.upgrade()).map(|p| p.name.clone())
    }

    /// 尝试进入 Stopping 状态；已销毁/销毁中返回 false（dispose 幂等）。
    pub(crate) fn try_begin_dispose(&self) -> bool {
        self.state
            .compare_exchange(
                LifecycleState::Created as u8,
                LifecycleState::Stopping as u8,
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_ok()
            || self
                .state
                .compare_exchange(
                    LifecycleState::Started as u8,
                    LifecycleState::Stopping as u8,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            || self
                .state
                .compare_exchange(
                    LifecycleState::Starting as u8,
                    LifecycleState::Stopping as u8,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            || self
                .state
                .compare_exchange(
                    LifecycleState::Failed as u8,
                    LifecycleState::Stopping as u8,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
    }

    pub(crate) fn add_child(&self, child: &Arc<Scope>) {
        let mut children = self.children.lock().expect("scope children poisoned");
        children.retain(|w| w.strong_count() > 0);
        children.push(Arc::downgrade(child));
    }

    /// 存活的子作用域快照。
    pub(crate) fn children_alive(&self) -> Vec<Arc<Scope>> {
        let mut children = self.children.lock().expect("scope children poisoned");
        children.retain(|w| w.strong_count() > 0);
        children.iter().filter_map(|w| w.upgrade()).collect()
    }

    pub(crate) fn track_fiber(&self, handle: FiberHandle) {
        let mut fibers = self.fibers.lock().expect("scope fibers poisoned");
        fibers.retain(|f| !f.is_finished());
        fibers.push(handle);
    }

    pub(crate) fn take_fibers(&self) -> Vec<FiberHandle> {
        std::mem::take(&mut *self.fibers.lock().expect("scope fibers poisoned"))
    }

    pub(crate) fn push_effect(&self, effect: Effect) {
        self.effects.lock().expect("scope effects poisoned").push(effect);
    }

    pub(crate) fn pop_effect(&self) -> Option<Effect> {
        self.effects.lock().expect("scope effects poisoned").pop()
    }

    pub(crate) fn track_listener(&self, id: ListenerId) {
        self.listeners.lock().expect("scope listeners poisoned").push(id);
    }

    pub(crate) fn take_listeners(&self) -> Vec<ListenerId> {
        std::mem::take(&mut *self.listeners.lock().expect("scope listeners poisoned"))
    }

    pub(crate) fn track_service(&self, name: String) {
        self.services.lock().expect("scope services poisoned").push(name);
    }

    pub(crate) fn take_services(&self) -> Vec<String> {
        std::mem::take(&mut *self.services.lock().expect("scope services poisoned"))
    }
}

//! Fiber 并发模型：由作用域托管的任务。
//!
//! 通过 [`crate::context::Context::spawn`] 创建的 Fiber 会登记到当前作用域；
//! 作用销毁时全部 Fiber 被 abort，实现「并发 & 生命周期」绑定。

use tokio::task::AbortHandle;

/// 作用域内一个运行中（或已结束）的 Fiber 句柄。
#[derive(Debug, Clone)]
pub struct FiberHandle {
    pub id: u64,
    pub name: String,
    abort: AbortHandle,
}

impl FiberHandle {
    pub(crate) fn new(id: u64, name: impl Into<String>, join: &tokio::task::JoinHandle<()>) -> Self {
        FiberHandle {
            id,
            name: name.into(),
            abort: join.abort_handle(),
        }
    }

    /// 取消该 Fiber。
    pub fn abort(&self) {
        self.abort.abort();
    }

    /// Fiber 是否已经结束。
    pub fn is_finished(&self) -> bool {
        self.abort.is_finished()
    }
}

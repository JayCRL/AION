//! Effect 机制：向当前作用域登记「副作用清理函数」，作用域销毁时逆序执行。
//!
//! 与 Fiber 类比：
//! - Fiber 负责「活的并发任务」，作用域销毁时被 abort；
//! - Effect 负责「登记的清理动作」，作用域销毁时被 await 执行。

use std::sync::Arc;

use crate::BoxFut;

/// 一个已登记的副作用。
#[derive(Clone)]
pub struct Effect {
    pub name: String,
    cleanup: Arc<dyn Fn() -> BoxFut<()> + Send + Sync>,
}

impl Effect {
    pub fn new(
        name: impl Into<String>,
        cleanup: impl Fn() -> BoxFut<()> + Send + Sync + 'static,
    ) -> Self {
        Effect {
            name: name.into(),
            cleanup: Arc::new(cleanup),
        }
    }

    /// 执行清理动作。
    pub async fn run(&self) {
        (self.cleanup)().await;
    }
}

impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Effect").field("name", &self.name).finish()
    }
}

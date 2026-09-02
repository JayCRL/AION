//! 插件系统：插件在独立子作用域中执行 `apply`，可注册服务 / 监听事件 / 登记 Effect。

use async_trait::async_trait;

use crate::context::Context;
use crate::{BoxFut, CordisResult};

/// 插件 trait。
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// 插件名。
    fn name(&self) -> &str;

    /// 插件描述。
    fn description(&self) -> &str {
        ""
    }

    /// 插件入口：传入插件专属子作用域的 Context。
    async fn apply(&self, ctx: Context) -> CordisResult<()>;
}

/// 闭包插件。
pub struct FnPlugin<F> {
    name: String,
    description: &'static str,
    f: F,
}

#[async_trait]
impl<F> Plugin for FnPlugin<F>
where
    F: Fn(Context) -> BoxFut<CordisResult<()>> + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    async fn apply(&self, ctx: Context) -> CordisResult<()> {
        (self.f)(ctx).await
    }
}

/// 用闭包快速构造插件。
pub fn plugin_fn<F>(name: impl Into<String>, f: F) -> FnPlugin<F>
where
    F: Fn(Context) -> BoxFut<CordisResult<()>> + Send + Sync + 'static,
{
    FnPlugin {
        name: name.into(),
        description: "",
        f,
    }
}

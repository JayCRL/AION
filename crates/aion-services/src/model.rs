//! ModelService：模型服务（LLM 后端抽象）。内置离线 Echo 后端，可注册任意后端。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use crate::error::{AionError, AionResult};
use crate::security::SecurityContext;

/// 一条对话消息。
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// `system` / `user` / `assistant`。
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        ChatMessage { role: "system".into(), content: content.into() }
    }

    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage { role: "user".into(), content: content.into() }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        ChatMessage { role: "assistant".into(), content: content.into() }
    }
}

/// 模型后端抽象。
#[async_trait]
pub trait ModelBackend: Send + Sync {
    fn name(&self) -> &str;

    async fn chat(&self, messages: &[ChatMessage]) -> AionResult<String>;
}

/// 内置离线后端：确定性的回声响应（便于测试与演示，无需网络）。
pub struct EchoBackend {
    name: String,
}

impl EchoBackend {
    pub fn new() -> Self {
        EchoBackend { name: "echo".into() }
    }
}

impl Default for EchoBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ModelBackend for EchoBackend {
    fn name(&self) -> &str {
        &self.name
    }

    async fn chat(&self, messages: &[ChatMessage]) -> AionResult<String> {
        let last_user = messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let n = messages.len();
        let chars = last_user.chars().count();
        let words = last_user.split_whitespace().count();
        let preview: String = last_user.chars().take(120).collect();
        Ok(format!(
            "【AION Echo 模型】已收到 {n} 条消息。\n\
             最后输入（{chars} 字符 / {words} 词）：「{preview}」\n\
             提示：这是离线 Echo 后端；通过 ModelService::register_backend 可接入真实 LLM。"
        ))
    }
}

/// 模型服务。
pub struct ModelService {
    backends: RwLock<HashMap<String, Arc<dyn ModelBackend>>>,
    default_backend: RwLock<Option<String>>,
}

impl ModelService {
    pub fn new() -> Self {
        ModelService {
            backends: RwLock::new(HashMap::new()),
            default_backend: RwLock::new(None),
        }
    }

    /// 注册后端；`set_default` 时同时设为默认。
    pub fn register_backend(&self, backend: Arc<dyn ModelBackend>, set_default: bool) {
        let name = backend.name().to_string();
        self.backends
            .write()
            .expect("backends poisoned")
            .insert(name.clone(), backend);
        if set_default {
            *self.default_backend.write().expect("default poisoned") = Some(name);
        }
    }

    pub fn set_default(&self, name: &str) -> AionResult<()> {
        let exists = self
            .backends
            .read()
            .expect("backends poisoned")
            .contains_key(name);
        if !exists {
            return Err(AionError::Model(format!("backend `{name}` not registered")));
        }
        *self.default_backend.write().expect("default poisoned") = Some(name.to_string());
        Ok(())
    }

    fn pick(&self, name: Option<&str>) -> AionResult<Arc<dyn ModelBackend>> {
        let backends = self.backends.read().expect("backends poisoned");
        let key = name
            .map(|s| s.to_string())
            .or_else(|| self.default_backend.read().expect("default poisoned").clone())
            .or_else(|| backends.keys().next().cloned());
        key.and_then(|k| backends.get(&k).cloned())
            .ok_or_else(|| AionError::Model("no model backend registered".into()))
    }

    /// 发起对话。
    pub async fn chat(
        &self,
        sec: &SecurityContext,
        backend: Option<&str>,
        messages: &[ChatMessage],
    ) -> AionResult<String> {
        sec.check_cap("model:use")?;
        let backend = self.pick(backend)?;
        backend.chat(messages).await
    }

    /// 已注册的后端列表。
    pub fn list_backends(&self, sec: &SecurityContext) -> AionResult<Vec<String>> {
        sec.check_cap("model:use")?;
        let mut names: Vec<String> = self
            .backends
            .read()
            .expect("backends poisoned")
            .keys()
            .cloned()
            .collect();
        names.sort();
        Ok(names)
    }
}

impl Default for ModelService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl cordis::Service for ModelService {
    fn name(&self) -> &'static str {
        "model"
    }

    fn description(&self) -> &'static str {
        "模型服务 (LLM)"
    }

    async fn start(&self, ctx: &cordis::Context) -> cordis::CordisResult<()> {
        self.register_backend(Arc::new(EchoBackend::new()), true);
        let backend = ctx
            .config()
            .get_string("model.default_backend")
            .unwrap_or_else(|| "echo".into());
        if let Err(e) = self.set_default(&backend) {
            ctx.warn(format!("config default backend: {e}"));
        }
        ctx.info("ModelService ready (backends: echo)");
        Ok(())
    }
}

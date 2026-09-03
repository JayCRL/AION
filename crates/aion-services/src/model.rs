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

// ---------------------------------------------------------------------------
// OpenAI 兼容后端（支持 DeepSeek / Qwen / OpenAI / 小米等）
// ---------------------------------------------------------------------------

/// OpenAI-compatible Chat API 后端。
///
/// 任何兼容 `POST {base_url}/chat/completions` 协议的 LLM 服务都可以接入：
/// - DeepSeek: `https://api.deepseek.com/v1` + `deepseek-chat`
/// - Qwen: `https://dashscope.aliyuncs.com/compatible-mode/v1` + `qwen-turbo`
/// - OpenAI: `https://api.openai.com/v1` + `gpt-4o`
/// - 小米 MiMo / 其他
pub struct OpenAiCompatBackend {
    name: String,
    base_url: String,
    model: String,
    api_key: String,
    client: reqwest::Client,
}

impl OpenAiCompatBackend {
    pub fn new(name: impl Into<String>, base_url: impl Into<String>, model: impl Into<String>, api_key: impl Into<String>) -> Self {
        let base_url: String = base_url.into();
        OpenAiCompatBackend {
            name: name.into(),
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.into(),
            api_key: api_key.into(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl ModelBackend for OpenAiCompatBackend {
    fn name(&self) -> &str {
        &self.name
    }

    async fn chat(&self, messages: &[ChatMessage]) -> AionResult<String> {
        let url = format!("{}/chat/completions", self.base_url);
        let msgs: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| serde_json::json!({"role": m.role, "content": m.content}))
            .collect();

        let body = serde_json::json!({
            "model": self.model,
            "messages": msgs,
            "temperature": 0.7,
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| AionError::Model(format!("LLM request failed: {e}")))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| AionError::Model(format!("LLM response read failed: {e}")))?;

        if !status.is_success() {
            return Err(AionError::Model(format!(
                "LLM API returned {status}: {text}"
            )));
        }

        let data: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| AionError::Model(format!("LLM response parse failed: {e}")))?;

        data.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                AionError::Model(format!(
                    "LLM response missing choices[0].message.content: {}",
                    &text[..text.len().min(200)]
                ))
            })
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

//! ModelService：模型服务（LLM 后端抽象）。内置离线 Echo 后端，可注册任意后端。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use futures::StreamExt;

use crate::error::{AionError, AionResult};
use crate::security::SecurityContext;

/// 一条对话消息。
///
/// 原生工具协议下,一条 assistant 消息可携带若干 `tool_use`,一条 user 消息可
/// 携带若干 `tool_result`(回填上一次 tool_use 的真实输出)。纯文本后端忽略
/// 这两组字段即可(构造器只填 `content`)。
#[derive(Debug, Clone, Default)]
pub struct ChatMessage {
    /// `system` / `user` / `assistant`。
    pub role: String,
    /// 纯文本正文(无结构化块时)。有 `tool_uses` / `tool_results` 时可为空。
    pub content: String,
    /// assistant 发出的工具调用块。
    pub tool_uses: Vec<ToolUseBlock>,
    /// user 回填的工具结果块(回应前面的 tool_use)。
    pub tool_results: Vec<ToolResultBlock>,
}

/// assistant 消息里的一个工具调用块。
#[derive(Debug, Clone)]
pub struct ToolUseBlock {
    /// 工具调用 ID(与 `tool_result.tool_use_id` 配对)。
    pub id: String,
    /// 工具名(与 `ToolDefinition.name` 一致)。
    pub name: String,
    /// 按工具 schema 生成的参数。
    pub input: serde_json::Value,
}

/// user 消息里回填工具结果的一个块。
#[derive(Debug, Clone)]
pub struct ToolResultBlock {
    /// 回应哪个 `tool_use`。
    pub tool_use_id: String,
    /// 工具输出的文本形式。
    pub content: String,
    /// 是否执行出错(前端/模型据此判断继续或中止)。
    pub is_error: bool,
}

/// 一轮模型对话的产物:正文文本 + 可选的工具调用列表。
#[derive(Debug, Clone, Default)]
pub struct AgentTurn {
    pub text: String,
    pub tool_uses: Vec<ToolUseBlock>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        ChatMessage {
            role: "system".into(),
            content: content.into(),
            ..Default::default()
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage {
            role: "user".into(),
            content: content.into(),
            ..Default::default()
        }
    }

    /// 携带一个工具结果的 user 消息(纯文本后端只看到空正文)。
    pub fn user_tool_result(block: ToolResultBlock) -> Self {
        ChatMessage {
            role: "user".into(),
            content: String::new(),
            tool_results: vec![block],
            ..Default::default()
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        ChatMessage {
            role: "assistant".into(),
            content: content.into(),
            ..Default::default()
        }
    }

    /// 携带若干 tool_use 的 assistant 消息(正文可为空)。
    pub fn assistant_tool_uses(tool_uses: Vec<ToolUseBlock>) -> Self {
        ChatMessage {
            role: "assistant".into(),
            content: String::new(),
            tool_uses,
            ..Default::default()
        }
    }
}

/// 模型后端抽象。
#[async_trait]
pub trait ModelBackend: Send + Sync {
    fn name(&self) -> &str;

    async fn chat(&self, messages: &[ChatMessage]) -> AionResult<String>;

    /// 流式对话：文本增量经 `tx` 实时送出，返回完整文本。
    /// 后端可覆盖以逐 token 推送；默认实现退化为一次性整段返回。
    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> AionResult<String> {
        let full = self.chat(messages).await?;
        if !full.is_empty() {
            let _ = tx.send(full.clone());
        }
        Ok(full)
    }

    /// 原生工具对话：携带 `tools` 参数调用模型，解析 `tool_use` 并回填 `tool_result`。
    ///
    /// 默认实现退化为纯文本(无视 `tools`，`tool_uses` 恒空)——Echo 等纯文本后端
    /// 不破。只有能理解工具协议的后端(Anthropic / 支持 function-calling 的
    /// OpenAI-compat)才需要覆盖。
    async fn chat_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
        _tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> AionResult<AgentTurn> {
        let text = self.chat(_messages).await?;
        Ok(AgentTurn {
            text,
            tool_uses: Vec::new(),
        })
    }
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

/// 逐块消费一条 SSE 字节流：按 `\n` 切行，凡以 `data:` 开头（非 `[DONE]`、
/// 非注释/空行）的载荷都会回调 `on_data`。跨 chunk 的半行会留在缓冲区等待补齐。
async fn read_sse_stream<S, B>(
    mut stream: S,
    mut on_data: impl FnMut(String) -> AionResult<()>,
) -> AionResult<()>
where
    S: futures::Stream<Item = Result<B, reqwest::Error>> + Unpin,
    B: AsRef<[u8]>,
{
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| AionError::Model(format!("LLM stream read error: {e}")))?;
        buf.extend_from_slice(chunk.as_ref());
        loop {
            let Some(pos) = buf.iter().position(|&b| b == b'\n') else { break };
            let raw: Vec<u8> = buf.drain(..=pos).collect();
            let mut line = String::from_utf8_lossy(&raw[..raw.len() - 1]).into_owned();
            while line.ends_with('\r') {
                line.pop();
            }
            if let Some(payload) = line.strip_prefix("data:") {
                let payload = payload.trim();
                if payload == "[DONE]" || payload.is_empty() {
                    continue;
                }
                on_data(payload.to_string())?;
            }
        }
    }
    Ok(())
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
        ensure_http_scheme(&self.base_url)?;
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

    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> AionResult<String> {
        ensure_http_scheme(&self.base_url)?;
        let url = format!("{}/chat/completions", self.base_url);
        let msgs: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| serde_json::json!({"role": m.role, "content": m.content}))
            .collect();

        let body = serde_json::json!({
            "model": self.model,
            "messages": msgs,
            "temperature": 0.7,
            "stream": true,
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await
            .map_err(|e| AionError::Model(format!("LLM request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp
                .text()
                .await
                .map_err(|e| AionError::Model(format!("LLM error read failed: {e}")))?;
            return Err(AionError::Model(format!(
                "LLM API returned {status}: {text}"
            )));
        }

        let mut full = String::new();
        read_sse_stream(resp.bytes_stream(), |payload| {
            let v: serde_json::Value = serde_json::from_str(&payload)
                .map_err(|e| AionError::Model(format!("bad SSE json: {e}: {payload}")))?;
            if let Some(delta) = v
                .pointer("/choices/0/delta/content")
                .and_then(|c| c.as_str())
            {
                full.push_str(delta);
                let _ = tx.send(delta.to_string());
            }
            Ok(())
        })
        .await?;

        if full.trim().is_empty() {
            return Err(AionError::Model("LLM returned empty streamed reply".into()));
        }
        Ok(full)
    }
}

// ---------------------------------------------------------------------------
// Anthropic 兼容后端（Messages API：POST {base_url}/v1/messages）
// ---------------------------------------------------------------------------

/// 规范化 Anthropic base_url：去尾斜杠与多余的 `/v1`（实际请求拼 `{base}/v1/messages`）。
pub fn normalize_anthropic_base(base_url: &str) -> String {
    let s = base_url.trim().trim_end_matches('/');
    let s = s.strip_suffix("/v1").unwrap_or(s);
    s.to_string()
}

/// 校验 base_url 至少带 http(s) 前缀，避免 reqwest 报出难懂的 builder error。
fn ensure_http_scheme(base_url: &str) -> AionResult<()> {
    if base_url.starts_with("http://") || base_url.starts_with("https://") {
        Ok(())
    } else {
        Err(AionError::Model(format!(
            "invalid base_url `{base_url}`: must start with http:// or https://"
        )))
    }
}

/// 合并连续同角色消息（Anthropic Messages API 要求 user/assistant 交替，且首条为 user）。
fn merge_anthropic_messages(msgs: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (role, content) in msgs {
        if let Some(last) = out.last_mut() {
            if last.0 == role {
                if !content.is_empty() {
                    if !last.1.is_empty() {
                        last.1.push_str("\n\n");
                    }
                    last.1.push_str(&content);
                }
                continue;
            }
        }
        out.push((role, content));
    }
    if out.first().map(|m| m.0.as_str()) != Some("user") {
        out.insert(0, ("user".into(), "(继续)".into()));
    }
    out
}

/// 把 AION `ChatMessage` 组装成 Anthropic `messages`(支持 tool_use/tool_result 块)。
///
/// 返回 `(system 拼接串, messages)`。与旧 `merge_anthropic_messages`(纯文本、粗暴
/// 同角色拼接)不同:带结构块的 message 必须**逐条**保留(Anthropic 要求 tool_result
/// 紧跟对应 assistant tool_use,不能合并),故只合并两侧都是纯文本的相邻同角色消息。
fn build_anthropic_messages(
    messages: &[ChatMessage],
) -> (String, Vec<serde_json::Value>) {
    let mut system_parts: Vec<String> = Vec::new();
    let mut out: Vec<serde_json::Value> = Vec::new();

    let mut last_role: Option<&str> = None;
    for m in messages {
        if m.role == "system" {
            if !m.content.is_empty() {
                system_parts.push(m.content.clone());
            }
            continue;
        }
        let role: &str = if m.role == "user" { "user" } else { "assistant" };
        let mut blocks: Vec<serde_json::Value> = Vec::new();
        if !m.content.trim().is_empty() {
            blocks.push(serde_json::json!({ "type": "text", "text": m.content }));
        }
        for tu in &m.tool_uses {
            blocks.push(serde_json::json!({
                "type": "tool_use",
                "id": tu.id,
                "name": tu.name,
                "input": tu.input,
            }));
        }
        for tr in &m.tool_results {
            blocks.push(serde_json::json!({
                "type": "tool_result",
                "tool_use_id": tr.tool_use_id,
                "content": tr.content,
                "is_error": tr.is_error,
            }));
        }
        // 纯文本相邻同角色才合并(否则各自独立成消息,保序)。仅当上一条消息的
        // content 是纯字符串(单 text)才拼接;若上一条已带 tool 块则不可合并。
        let mut merged = false;
        if blocks.len() == 1
            && blocks[0].get("type").and_then(|t| t.as_str()) == Some("text")
            && last_role == Some(role)
        {
            if let Some(last) = out.last_mut() {
                let cur = last
                    .get("content")
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string());
                if let Some(cur) = cur {
                    let add = blocks[0]["text"].as_str().unwrap_or("");
                    last["content"] = serde_json::Value::String(format!("{cur}\n\n{add}"));
                    merged = true;
                }
            }
        }
        if merged {
            continue;
        }
        // 全是 text 的单消息 → content 用字符串(兼容最宽松的 Anthropic 实现)。
        let content: serde_json::Value = if blocks.len() == 1
            && blocks[0].get("type").and_then(|t| t.as_str()) == Some("text")
        {
            serde_json::json!(blocks[0]["text"].as_str().unwrap_or(""))
        } else {
            serde_json::Value::Array(blocks)
        };
        out.push(serde_json::json!({ "role": role, "content": content }));
        last_role = Some(role);
    }
    if out.first().map(|m| m.get("role").and_then(|r| r.as_str())) != Some(Some("user")) {
        out.insert(
            0,
            serde_json::json!({ "role": "user", "content": "(继续)" }),
        );
    }
    (system_parts.join("\n\n"), out)
}

/// Anthropic Messages API 后端。
///
/// 任何兼容 `POST {base_url}/v1/messages` 协议的 LLM 服务都可以接入：
/// - 智谱 GLM: `https://open.bigmodel.cn/api/anthropic` + `glm-5.3-flash`
/// - Anthropic: `https://api.anthropic.com` + `claude-sonnet-5`
///
/// 注意：`base_url` 不需要（也不应该）带 `/v1` 后缀，会自动补全。
/// 思考模型（如 GLM-5.3-Flash）返回的 `thinking` 块会被跳过，仅取 `text` 块。
pub struct AnthropicCompatBackend {
    name: String,
    base_url: String,
    model: String,
    api_key: String,
    max_tokens: u32,
    client: reqwest::Client,
}

impl AnthropicCompatBackend {
    pub fn new(
        name: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self::with_max_tokens(name, base_url, model, api_key, 8192)
    }

    pub fn with_max_tokens(
        name: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
        max_tokens: u32,
    ) -> Self {
        AnthropicCompatBackend {
            name: name.into(),
            base_url: normalize_anthropic_base(&base_url.into()),
            model: model.into(),
            api_key: api_key.into(),
            max_tokens,
            client: reqwest::Client::new(),
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/v1/messages", self.base_url)
    }
}

#[async_trait]
impl ModelBackend for AnthropicCompatBackend {
    fn name(&self) -> &str {
        &self.name
    }

    async fn chat(&self, messages: &[ChatMessage]) -> AionResult<String> {
        ensure_http_scheme(&self.base_url)?;
        let mut system_parts: Vec<String> = Vec::new();
        let mut pairs: Vec<(String, String)> = Vec::new();
        for m in messages {
            match m.role.as_str() {
                "system" => system_parts.push(m.content.clone()),
                "user" => pairs.push(("user".into(), m.content.clone())),
                _ => pairs.push(("assistant".into(), m.content.clone())),
            }
        }
        let msgs: Vec<serde_json::Value> = merge_anthropic_messages(pairs)
            .into_iter()
            .map(|(role, content)| serde_json::json!({"role": role, "content": content}))
            .collect();

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": msgs,
        });
        if !system_parts.is_empty() {
            body["system"] = serde_json::Value::String(system_parts.join("\n\n"));
        }

        let resp = self
            .client
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(120))
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

        // content 可能是块数组（当前协议）或纯字符串（旧协议），都兼容。
        let reply: Option<String> = match data.get("content") {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(serde_json::Value::Array(blocks)) => {
                let parts: Vec<&str> = blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect();
                if parts.is_empty() {
                    None
                } else {
                    Some(parts.join("\n"))
                }
            }
            _ => None,
        };

        match reply {
            Some(s) if !s.trim().is_empty() => Ok(s),
            _ => {
                let stop = data
                    .get("stop_reason")
                    .and_then(|s| s.as_str())
                    .unwrap_or("unknown");
                Err(AionError::Model(format!(
                    "LLM returned no text content (stop_reason={stop}; \
                     思考模型可能耗尽了 max_tokens，可调大): {}",
                    &text[..text.len().min(200)]
                )))
            }
        }
    }

    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> AionResult<String> {
        ensure_http_scheme(&self.base_url)?;
        let mut system_parts: Vec<String> = Vec::new();
        let mut pairs: Vec<(String, String)> = Vec::new();
        for m in messages {
            match m.role.as_str() {
                "system" => system_parts.push(m.content.clone()),
                "user" => pairs.push(("user".into(), m.content.clone())),
                _ => pairs.push(("assistant".into(), m.content.clone())),
            }
        }
        let msgs: Vec<serde_json::Value> = merge_anthropic_messages(pairs)
            .into_iter()
            .map(|(role, content)| serde_json::json!({"role": role, "content": content}))
            .collect();

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": msgs,
            "stream": true,
        });
        if !system_parts.is_empty() {
            body["system"] = serde_json::Value::String(system_parts.join("\n\n"));
        }

        let resp = self
            .client
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await
            .map_err(|e| AionError::Model(format!("LLM request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp
                .text()
                .await
                .map_err(|e| AionError::Model(format!("LLM error read failed: {e}")))?;
            return Err(AionError::Model(format!(
                "LLM API returned {status}: {text}"
            )));
        }

        // 流式事件：只取 content_block_delta 里的 text_delta；thinking/signature 块跳过。
        let mut full = String::new();
        read_sse_stream(resp.bytes_stream(), |payload| {
            let v: serde_json::Value = serde_json::from_str(&payload)
                .map_err(|e| AionError::Model(format!("bad SSE json: {e}: {payload}")))?;
            let ev = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if ev == "content_block_delta" {
                let dt = v
                    .pointer("/delta/type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                if dt == "text_delta" {
                    if let Some(text) = v.pointer("/delta/text").and_then(|t| t.as_str()) {
                        full.push_str(text);
                        let _ = tx.send(text.to_string());
                    }
                }
            }
            Ok(())
        })
        .await?;

        if full.trim().is_empty() {
            return Err(AionError::Model("LLM returned empty streamed reply".into()));
        }
        Ok(full)
    }

    /// 原生工具对话(Anthropic 消息含 content 块数组 + `tools` 参数)。
    ///
    /// 流式解析三类事件:
    /// - `content_block_start` type=text → 后续 text_delta 逐 token 送 `tx`;
    /// - type=tool_use → 记下 id/name;
    /// - `input_json_delta` → 逐片拼接该 tool_use 的 `input` JSON(跨 chunk 分片)。
    /// 结束后把每个 tool_use 的拼接串解析成 `Value`。
    async fn chat_turn(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> AionResult<AgentTurn> {
        ensure_http_scheme(&self.base_url)?;
        let (system, msgs) = build_anthropic_messages(messages);
        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": msgs,
            "stream": true,
        });
        if !system.is_empty() {
            body["system"] = serde_json::Value::String(system);
        }
        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(tools.to_vec());
        }

        let resp = self
            .client
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(180))
            .send()
            .await
            .map_err(|e| AionError::Model(format!("LLM request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp
                .text()
                .await
                .map_err(|e| AionError::Model(format!("LLM error read failed: {e}")))?;
            return Err(AionError::Model(format!(
                "LLM API returned {status}: {text}"
            )));
        }

        // 按 content block index 累积进行中的 tool_use。
        struct Acc {
            id: String,
            name: String,
            json: String,
            /// 最后一次「拼接串整体可解析」时的值——兼容部分服务把整段 JSON 当
            /// partial_json 反复下发（拼接会失败）的情况；最终优先整体解析。
            fallback: Option<serde_json::Value>,
        }
        let mut accs: std::collections::HashMap<u64, Acc> = std::collections::HashMap::new();
        let mut order: Vec<u64> = Vec::new();
        let mut full = String::new();

        read_sse_stream(resp.bytes_stream(), |payload| {
            let v: serde_json::Value = serde_json::from_str(&payload)
                .map_err(|e| AionError::Model(format!("bad SSE json: {e}: {payload}")))?;
            let ev = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match ev {
                "content_block_start" => {
                    let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                    let blk = v.get("content_block").cloned().unwrap_or(serde_json::Value::Null);
                    let btype = blk.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if btype == "tool_use" {
                        let raw_id = blk
                            .get("id")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string();
                        // 个别 Anthropic 兼容实现不带 id——给个确定性的合成 id，
                        // 否则后面 tool_result 无法配对。
                        let id = if raw_id.is_empty() {
                            format!("toolu_{idx}")
                        } else {
                            raw_id
                        };
                        let name = blk
                            .get("name")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string();
                        if accs
                            .insert(idx, Acc { id, name, json: String::new(), fallback: None })
                            .is_none()
                        {
                            order.push(idx);
                        }
                    }
                }
                "content_block_delta" => {
                    let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                    let dt = v.pointer("/delta/type").and_then(|t| t.as_str()).unwrap_or("");
                    if dt == "text_delta" {
                        if let Some(text) = v.pointer("/delta/text").and_then(|t| t.as_str()) {
                            full.push_str(text);
                            let _ = tx.send(text.to_string());
                        }
                    } else if dt == "input_json_delta" {
                        if let Some(seg) = v
                            .pointer("/delta/partial_json")
                            .and_then(|t| t.as_str())
                        {
                            if let Some(a) = accs.get_mut(&idx) {
                                a.json.push_str(seg);
                                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&a.json)
                                {
                                    a.fallback = Some(parsed);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            Ok(())
        })
        .await?;

        let mut tool_uses: Vec<ToolUseBlock> = Vec::new();
        for idx in order {
            if let Some(a) = accs.remove(&idx) {
                let input = if a.json.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str(&a.json)
                        .ok()
                        .or(a.fallback)
                        .unwrap_or_else(|| serde_json::json!({ "_raw": a.json }))
                };
                tool_uses.push(ToolUseBlock {
                    id: a.id,
                    name: a.name,
                    input,
                });
            }
        }
        Ok(AgentTurn {
            text: full,
            tool_uses,
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

    /// 发起流式对话：文本增量经 `tx` 实时送出，返回完整文本。
    pub async fn chat_stream(
        &self,
        sec: &SecurityContext,
        backend: Option<&str>,
        messages: &[ChatMessage],
        tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> AionResult<String> {
        sec.check_cap("model:use")?;
        let backend = self.pick(backend)?;
        backend.chat_stream(messages, tx).await
    }

    /// 原生工具对话：携带 `tools` 调用模型，解析 `tool_use` 并回填 `tool_result`。
    pub async fn chat_turn(
        &self,
        sec: &SecurityContext,
        backend: Option<&str>,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> AionResult<AgentTurn> {
        sec.check_cap("model:use")?;
        let backend = self.pick(backend)?;
        backend.chat_turn(messages, tools, tx).await
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_anthropic_base() {
        assert_eq!(
            normalize_anthropic_base("https://open.bigmodel.cn/api/anthropic"),
            "https://open.bigmodel.cn/api/anthropic"
        );
        assert_eq!(
            normalize_anthropic_base("https://open.bigmodel.cn/api/anthropic/"),
            "https://open.bigmodel.cn/api/anthropic"
        );
        // 用户误带 /v1 后缀也能正确补全
        assert_eq!(
            normalize_anthropic_base("https://api.anthropic.com/v1/"),
            "https://api.anthropic.com"
        );
    }

    #[test]
    fn test_merge_anthropic_messages() {
        // 连续同角色合并（system 在调用前已被抽出，不会进入本函数）
        let merged = merge_anthropic_messages(vec![
            ("user".into(), "hi".into()),
            ("assistant".into(), "hello".into()),
            ("assistant".into(), "again".into()),
        ]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0], ("user".into(), "hi".into()));
        assert_eq!(merged[1], ("assistant".into(), "hello\n\nagain".into()));
    }

    #[test]
    fn test_merge_first_must_be_user() {
        let merged = merge_anthropic_messages(vec![("assistant".into(), "hello".into())]);
        assert_eq!(merged[0].0, "user");
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn test_ensure_http_scheme() {
        assert!(ensure_http_scheme("https://example.com").is_ok());
        assert!(ensure_http_scheme("http://127.0.0.1:8000").is_ok());
        assert!(ensure_http_scheme("api.example.com/v1").is_err());
    }
}

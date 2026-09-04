//! AION Web 服务：axum 静态文件 + API 端点。
//!
//! 启动时 boot cordis Runtime（services + ToolRuntime + builtin Tools），
//! 然后挂 axum 路由把前端的 Chat 请求接到 Agent + ToolRuntime。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

use aion_protocol::prelude::*;

use aion_services::model::ChatMessage;
use aion_services::{
    backend_from_provider, LlmProtocol, LlmProvider, LlmProviderStore,
};

/// Web 层共享状态。
pub struct AppState {
    pub ctx: cordis::Context,
    pub tool_runtime: Arc<aion_services::tool::ToolRuntime>,
    /// cc-switch 式 LLM 供应商存储（持久化于 aion.providers.json）。
    pub llm_store: Arc<LlmProviderStore>,
    /// 会话历史（内存态，Phase 3 简化版）。
    pub sessions: tokio::sync::Mutex<HashMap<String, Vec<ChatMessage>>>,
}

/// 启动 Web 服务（先 boot cordis Runtime 再起 axum）。
pub async fn run(port: u16) -> anyhow::Result<()> {
    let ctx = aion::boot::boot(aion::boot::load_config()).await?;
    let tool_runtime = ctx
        .require::<aion_services::tool::ToolRuntime>()
        .await
        .map_err(|e| anyhow::anyhow!("ToolRuntime not available: {e}"))?;

    // 恢复持久化的 LLM 供应商：有激活档位则直接注册为默认后端
    let llm_store = Arc::new(LlmProviderStore::load_default());
    if let Some(p) = llm_store.active_provider() {
        match ctx.require::<aion_services::model::ModelService>().await {
            Ok(svc) => {
                svc.register_backend(backend_from_provider(&p), true);
                println!(
                    "   LLM provider: {} ({}, {})",
                    p.name,
                    p.protocol.as_str(),
                    p.model
                );
            }
            Err(e) => eprintln!("restore llm provider failed: {e}"),
        }
    }

    let state = Arc::new(AppState {
        ctx: ctx.clone(),
        tool_runtime,
        llm_store,
        sessions: tokio::sync::Mutex::new(HashMap::new()),
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/logo.png", get(logo_handler))
        .route("/api/health", get(api_health))
        .route("/api/tools", get(api_tools))
        .route("/api/chat", post(api_chat))
        .route("/api/config", post(api_config))
        .route(
            "/api/llm/providers",
            get(api_llm_list).post(api_llm_upsert),
        )
        .route("/api/llm/test", post(api_llm_test))
        .route("/api/llm/providers/:id/activate", post(api_llm_activate))
        .route("/api/llm/providers/:id", delete(api_llm_delete))
        .route("/api/sessions", get(api_sessions).post(api_new_session))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("0.0.0.0:{}", port);
    println!();
    println!("   AION Web UI (API v1)");
    println!("   listening on 0.0.0.0:{port}");
    println!();

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    ctx.dispose().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn index_handler() -> impl IntoResponse {
    Html(include_str!("../static/index.html"))
}

async fn logo_handler() -> Response {
    let bytes = include_bytes!("../static/logo.png");
    Response::builder()
        .header("Content-Type", "image/png")
        .body(axum::body::Body::from(bytes.to_vec()))
        .unwrap()
}

async fn api_health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let services: Vec<serde_json::Value> = state
        .ctx
        .list_services()
        .into_iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "state": s.state.as_str(),
                "deps": s.deps,
            })
        })
        .collect();
    let llm = state.llm_store.active_provider().map(|p| {
        serde_json::json!({
            "id": p.id,
            "name": p.name,
            "protocol": p.protocol.as_str(),
            "model": p.model,
        })
    });
    Json(serde_json::json!({
        "status": "ok",
        "platform": std::env::consts::OS,
        "llm": llm,
        "services": services,
    }))
}

async fn api_tools(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let registry = state.tool_runtime.registry();
    let tools: Vec<serde_json::Value> = registry
        .list()
        .into_iter()
        .map(|def| {
            serde_json::json!({
                "name": def.name,
                "description": def.description,
                "required_caps": def.required_caps,
                "risk": def.risk.as_str(),
            })
        })
        .collect();
    Json(serde_json::json!({ "tools": tools, "count": tools.len() }))
}

// ---------------------------------------------------------------------------
// Chat API — Agent 处理用户消息并返回 UIBlocks
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ChatRequest {
    pub message: String,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Serialize)]
pub struct ChatResponse {
    pub session_id: String,
    pub reply_text: String,
    pub blocks: Vec<serde_json::Value>,
    pub actions: Vec<serde_json::Value>,
    pub tool_calls: Vec<serde_json::Value>,
    pub tool_results: Vec<serde_json::Value>,
}

async fn api_chat(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, StatusCode> {
    let ctx = state.ctx.clone();
    let runtime = state.tool_runtime.clone();

    let agent_impl = aion::agents::builtin()
        .into_iter()
        .find(|a| a.name() == "assistant")
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let sec = aion::agents::developer_sec("web-user", &[]);
    let session_id = req
        .session_id
        .clone()
        .unwrap_or_else(|| format!("web-{}", std::process::id()));

    // 会话历史：透传给 Agent，实现多轮上下文
    let history: Vec<ChatMessage> = state
        .sessions
        .lock()
        .await
        .get(&session_id)
        .cloned()
        .unwrap_or_default();

    let task = aion::agents::AgentTask {
        kind: "chat".into(),
        input: req.message.clone(),
        params: serde_json::json!({
            "history": history
                .iter()
                .map(|m| serde_json::json!({"role": m.role, "content": m.content}))
                .collect::<Vec<_>>(),
        }),
    };

    let reply = agent_impl
        .handle(&ctx, &sec, &task)
        .await
        .map_err(|e| {
            eprintln!("agent error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut tool_calls = Vec::new();
    let mut tool_results = Vec::new();
    let mut blocks = Vec::new();
    let mut reply_text = reply.clone();

    blocks.push(serde_json::json!({
        "type": "text",
        "markdown": reply,
    }));

    // 更新会话历史（user + assistant），超出上限保留最近 40 条
    let mut new_history = history;
    new_history.push(ChatMessage::user(req.message.clone()));
    new_history.push(ChatMessage::assistant(reply.clone()));
    if new_history.len() > 40 {
        let drop = new_history.len() - 40;
        new_history.drain(..drop);
    }

    // Phase 3 简化：检测 reply 里的 ```run ... ``` 块并执行
    if let Some(cmd) = extract_run_block(&reply) {
        let tc = ToolCall {
            call_id: CallId::new(),
            tool: "terminal.exec".into(),
            arguments: serde_json::json!({ "command": cmd }),
            sandbox: Some(ToolSandboxHint::Default),
        };
        let result = runtime.execute(&ctx, tc.clone(), sec.clone()).await;
        tool_calls.push(serde_json::json!({
            "call_id": tc.call_id.as_str(),
            "tool": tc.tool,
            "arguments": tc.arguments,
        }));
        let result_json = serde_json::to_value(&result).unwrap_or(serde_json::Value::Null);
        tool_results.push(result_json.clone());
        let stdout = result
            .data
            .get("stdout")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if !stdout.is_empty() {
            blocks.push(serde_json::json!({
                "type": "terminal",
                "tool_call_id": tc.call_id.as_str(),
                "kind": "exec",
                "output": stdout,
            }));
        }

        // Agent 闭环：把真实执行结果回喂模型，基于输出继续回答
        let output_text = summarize_tool_output(&result_json);
        match continue_after_tool(&ctx, &sec, &new_history, &output_text).await {
            Ok(followup) if !followup.trim().is_empty() => {
                blocks.push(serde_json::json!({
                    "type": "text",
                    "markdown": followup,
                }));
                reply_text = format!("{reply}\n\n{followup}");
                new_history.push(ChatMessage::user(format!(
                    "我上一条回复发起的命令已执行，输出如下：\n{output_text}"
                )));
                new_history.push(ChatMessage::assistant(followup));
                if new_history.len() > 40 {
                    let drop = new_history.len() - 40;
                    new_history.drain(..drop);
                }
            }
            Ok(_) => {}
            Err(e) => eprintln!("continue_after_tool error: {e}"),
        }
    }

    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), new_history);

    Ok(Json(ChatResponse {
        session_id,
        reply_text,
        blocks,
        actions: vec![],
        tool_calls,
        tool_results,
    }))
}

/// 把 ToolResult 的关键字段压缩成一段可回喂模型的文本。
fn summarize_tool_output(result_json: &serde_json::Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(so) = result_json.get("stdout").and_then(|v| v.as_str()) {
        if !so.trim().is_empty() {
            parts.push(format!("stdout:\n{so}"));
        }
    }
    if let Some(se) = result_json.get("stderr").and_then(|v| v.as_str()) {
        if !se.trim().is_empty() {
            parts.push(format!("stderr:\n{se}"));
        }
    }
    if let Some(code) = result_json.get("exit_code").and_then(|v| v.as_i64()) {
        parts.push(format!("exit_code: {code}"));
    }
    if parts.is_empty() {
        "(无输出)".into()
    } else {
        parts.join("\n")
    }
}

/// 工具执行后，把输出回喂模型生成后续回答。
async fn continue_after_tool(
    ctx: &cordis::Context,
    sec: &aion_services::SecurityContext,
    history: &[ChatMessage],
    tool_output: &str,
) -> anyhow::Result<String> {
    let model = ctx
        .require::<aion_services::model::ModelService>()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut msgs: Vec<ChatMessage> =
        vec![ChatMessage::system(aion::agents::assistant::ASSISTANT_SYSTEM)];
    msgs.extend_from_slice(history);
    msgs.push(ChatMessage::user(format!(
        "上面那条命令的真实执行输出如下：\n{tool_output}\n请基于真实输出继续回答，保持简洁。"
    )));
    Ok(model.chat(sec, None, &msgs).await?)
}

/// 从 markdown reply 里提取 ```run\n command \n``` 代码块。
fn extract_run_block(reply: &str) -> Option<String> {
    let marker = "```run\n";
    let start = reply.find(marker)?;
    let rest = &reply[start + marker.len()..];
    let end = rest.find("```")?;
    let cmd = rest[..end].trim().to_string();
    if cmd.is_empty() { None } else { Some(cmd) }
}

// ---------------------------------------------------------------------------
// LLM Provider API — cc-switch 风格多供应商管理
// 持久化于工作目录 aion.providers.json（含密钥，已 gitignore）
// ---------------------------------------------------------------------------

fn err_json(code: StatusCode, msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    (code, Json(serde_json::json!({ "status": "error", "error": msg })))
}

fn provider_json(p: &LlmProvider, active: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "id": p.id,
        "name": p.name,
        "protocol": p.protocol.as_str(),
        "base_url": p.base_url,
        "model": p.model,
        "max_tokens": p.max_tokens,
        "has_key": !p.api_key.is_empty(),
        "active": active == Some(p.id.as_str()),
    })
}

/// 把指定供应商注册为 ModelService 默认后端。
async fn activate_provider(
    state: &AppState,
    id: &str,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let p = state
        .llm_store
        .get(id)
        .ok_or_else(|| err_json(StatusCode::NOT_FOUND, &format!("provider `{id}` not found")))?;
    let svc = state
        .ctx
        .require::<aion_services::model::ModelService>()
        .await
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    svc.register_backend(backend_from_provider(&p), true);
    Ok(())
}

async fn api_llm_list(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let active = state.llm_store.active_id();
    let providers: Vec<serde_json::Value> = state
        .llm_store
        .list()
        .iter()
        .map(|p| provider_json(p, active.as_deref()))
        .collect();
    Json(serde_json::json!({ "providers": providers, "active": active }))
}

#[derive(Deserialize)]
pub struct ProviderInput {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    pub model: String,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub activate: Option<bool>,
}

fn default_protocol() -> String {
    "openai".into()
}

async fn api_llm_upsert(
    State(state): State<Arc<AppState>>,
    Json(input): Json<ProviderInput>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let p = LlmProvider {
        id: input.id.unwrap_or_default(),
        name: input.name,
        protocol: LlmProtocol::parse(&input.protocol),
        base_url: input.base_url.trim().to_string(),
        api_key: input.api_key,
        model: input.model,
        max_tokens: input.max_tokens.unwrap_or(8192),
        created_at: 0,
    };
    if p.base_url.is_empty() || p.model.is_empty() {
        return Err(err_json(StatusCode::BAD_REQUEST, "base_url 与 model 不能为空"));
    }
    let id = state
        .llm_store
        .upsert(p)
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    let activate = input.activate.unwrap_or(true);
    if activate {
        state
            .llm_store
            .set_active(&id)
            .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
        activate_provider(&state, &id).await?;
    }
    Ok(Json(serde_json::json!({ "status": "ok", "id": id, "active": activate })))
}

async fn api_llm_activate(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    state
        .llm_store
        .set_active(&id)
        .map_err(|e| err_json(StatusCode::NOT_FOUND, &e.to_string()))?;
    activate_provider(&state, &id).await?;
    Ok(Json(serde_json::json!({ "status": "ok", "active": id })))
}

async fn api_llm_delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let removed = state
        .llm_store
        .remove(&id)
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    if !removed {
        return Err(err_json(
            StatusCode::NOT_FOUND,
            &format!("provider `{id}` not found"),
        ));
    }
    // 删掉激活档位后回退到内置 echo，保证服务始终可用
    if state.llm_store.active_id().is_none() {
        if let Ok(svc) = state
            .ctx
            .require::<aion_services::model::ModelService>()
            .await
        {
            svc.register_backend(std::sync::Arc::new(
                aion_services::model::EchoBackend::new(),
            ), true);
        }
    }
    Ok(Json(serde_json::json!({ "status": "ok" })))
}

#[derive(Deserialize)]
pub struct ProviderTestInput {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

/// 连通性测试：按 id（使用存储的密钥）或按临时传入的配置发起一次真实对话。
async fn api_llm_test(
    State(state): State<Arc<AppState>>,
    Json(input): Json<ProviderTestInput>,
) -> Json<serde_json::Value> {
    let p = match input.id {
        Some(id) => match state.llm_store.get(&id) {
            Some(p) => p,
            None => {
                return Json(serde_json::json!({
                    "status": "error",
                    "error": format!("provider `{id}` not found"),
                }))
            }
        },
        None => {
            let base_url = input.base_url.unwrap_or_default();
            let model = input.model.unwrap_or_default();
            if base_url.trim().is_empty() || model.trim().is_empty() {
                return Json(serde_json::json!({
                    "status": "error",
                    "error": "base_url 与 model 不能为空",
                }));
            }
            LlmProvider {
                id: "test".into(),
                name: "test".into(),
                protocol: LlmProtocol::parse(input.protocol.as_deref().unwrap_or("openai")),
                base_url,
                api_key: input.api_key.unwrap_or_default(),
                model,
                max_tokens: 8192,
                created_at: 0,
            }
        }
    };

    let backend = backend_from_provider(&p);
    match backend
        .chat(&[ChatMessage::user("ping，请只回复：pong")])
        .await
    {
        Ok(reply) => Json(serde_json::json!({
            "status": "ok",
            "reply": reply,
            "model": p.model,
        })),
        Err(e) => Json(serde_json::json!({ "status": "error", "error": e.to_string() })),
    }
}

// ---------------------------------------------------------------------------
// 旧版 LLM Config API（兼容保留）— 现在落到 provider 存储上
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LlmConfig {
    pub provider: String,
    pub api_key: String,
    pub api_url: String,
    pub model: String,
}

async fn api_config(
    State(state): State<Arc<AppState>>,
    Json(config): Json<LlmConfig>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let p = LlmProvider {
        id: config.provider.clone(),
        name: config.provider.clone(),
        protocol: LlmProtocol::OpenAi,
        base_url: config.api_url.clone(),
        api_key: config.api_key,
        model: config.model.clone(),
        max_tokens: 8192,
        created_at: 0,
    };
    let id = state
        .llm_store
        .upsert(p)
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    state
        .llm_store
        .set_active(&id)
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    activate_provider(&state, &id).await?;
    Ok(Json(serde_json::json!({
        "status": "ok",
        "provider": config.provider,
        "model": config.model,
        "backend": "llm",
    })))
}

// ---------------------------------------------------------------------------
// Session API（Phase 3 简化版）
// ---------------------------------------------------------------------------

async fn api_sessions() -> impl IntoResponse {
    Json(serde_json::json!({
        "sessions": [],
        "note": "session persistence coming in Phase 3 full",
    }))
}

async fn api_new_session() -> impl IntoResponse {
    Json(serde_json::json!({
        "session_id": format!("web-{}", std::process::id()),
        "state": "open",
    }))
}

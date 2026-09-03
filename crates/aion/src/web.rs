//! AION Web 服务：axum 静态文件 + API 端点。
//!
//! 启动时 boot cordis Runtime（services + ToolRuntime + builtin Tools），
//! 然后挂 axum 路由把前端的 Chat 请求接到 Agent + ToolRuntime。

use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_http::cors::CorsLayer;

use aion_protocol::prelude::*;

/// Web 层共享状态。
pub struct AppState {
    pub ctx: cordis::Context,
    pub tool_runtime: Arc<aion_services::tool::ToolRuntime>,
}

/// 启动 Web 服务（先 boot cordis Runtime 再起 axum）。
pub async fn run(port: u16) -> anyhow::Result<()> {
    let ctx = aion::boot::boot(aion::boot::load_config()).await?;
    let tool_runtime = ctx
        .require::<aion_services::tool::ToolRuntime>()
        .await
        .map_err(|e| anyhow::anyhow!("ToolRuntime not available: {e}"))?;

    let state = Arc::new(AppState {
        ctx: ctx.clone(),
        tool_runtime,
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/logo.png", get(logo_handler))
        .route("/api/health", get(api_health))
        .route("/api/tools", get(api_tools))
        .route("/api/chat", post(api_chat))
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
    Json(serde_json::json!({
        "status": "ok",
        "platform": std::env::consts::OS,
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
    let task = aion::agents::AgentTask {
        kind: "chat".into(),
        input: req.message.clone(),
        params: serde_json::json!({}),
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

    blocks.push(serde_json::json!({
        "type": "text",
        "markdown": reply,
    }));

    // Phase 3 简化：检测 reply 里的 ```run ... ``` 块并执行
    if let Some(cmd) = extract_run_block(&reply) {
        let tc = ToolCall {
            call_id: CallId::new(),
            tool: "terminal.exec".into(),
            arguments: serde_json::json!({ "command": cmd }),
            sandbox: Some(ToolSandboxHint::Default),
        };
        let result = runtime.execute(&ctx, tc.clone(), sec).await;
        tool_calls.push(serde_json::json!({
            "call_id": tc.call_id.as_str(),
            "tool": tc.tool,
            "arguments": tc.arguments,
        }));
        let result_json = serde_json::to_value(&result).unwrap_or(serde_json::Value::Null);
        tool_results.push(result_json);
        if let Some(stdout) = result.data.get("stdout").and_then(|v| v.as_str()) {
            blocks.push(serde_json::json!({
                "type": "terminal",
                "tool_call_id": tc.call_id.as_str(),
                "kind": "exec",
                "output": stdout,
            }));
        }
    }

    let session_id = req
        .session_id
        .unwrap_or_else(|| format!("web-{}", std::process::id()));

    Ok(Json(ChatResponse {
        session_id,
        reply_text: reply,
        blocks,
        actions: vec![],
        tool_calls,
        tool_results,
    }))
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

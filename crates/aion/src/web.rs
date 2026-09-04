//! AION Web 服务：axum 静态文件 + API 端点。
//!
//! 启动时 boot cordis Runtime（services + ToolRuntime + builtin Tools），
//! 然后挂 axum 路由把前端的 Chat 请求接到 Agent + ToolRuntime。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse, Response,
    },
    routing::{delete, get, post},
    Json, Router,
};
use futures::stream as fstream;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

use aion_protocol::prelude::*;

use aion_services::model::{ChatMessage, ModelService};
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
        .route("/api/chat/stream", post(api_chat_stream))
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

    // 拆出正文与 ```run 命令：正文是唯一 bubble，命令逐个自动执行（杜绝重复渲染）
    let parts = split_run_blocks(&reply);
    let prose = parts.prose;
    if !prose.is_empty() {
        blocks.push(serde_json::json!({
            "type": "text",
            "markdown": prose,
        }));
    }

    // 会话历史：user + assistant(正文)；命令执行走后续的 assistant 续答
    let mut new_history = history;
    new_history.push(ChatMessage::user(req.message.clone()));
    new_history.push(ChatMessage::assistant(prose.clone()));

    // 逐个执行 reply 里的 ```run 命令
    for cmd in &parts.cmds {
        let tc = ToolCall {
            call_id: CallId::new(),
            tool: "terminal.exec".into(),
            arguments: serde_json::json!({ "command": cmd }),
            sandbox: Some(ToolSandboxHint::Default),
        };
        tool_calls.push(serde_json::json!({
            "call_id": tc.call_id.as_str(),
            "tool": tc.tool,
            "arguments": tc.arguments,
        }));
        let result = runtime.execute(&ctx, tc.clone(), sec.clone()).await;
        let result_json = serde_json::to_value(&result).unwrap_or(serde_json::Value::Null);
        tool_results.push(result_json.clone());

        let output_text = summarize_tool_output(&result_json);
        blocks.push(terminal_block(cmd, &result_json));
        new_history.push(ChatMessage::user(format!(
            "我发出的命令 `{cmd}` 已自动执行，输出：\n{output_text}"
        )));
    }

    // Agent 闭环：有执行过命令就把真实输出回喂模型，进入有界多轮循环，
    // 直到模型不再发 run 命令（避免续答里再发命令却不再执行、围栏泄漏进气泡）
    let mut reply_text = prose;
    if !parts.cmds.is_empty() {
        let model = match ctx.require::<ModelService>().await {
            Ok(m) => m,
            Err(e) => {
                eprintln!("model unavailable: {e}");
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        };
        let mut cont = vec![ChatMessage::system(aion::agents::assistant::ASSISTANT_SYSTEM)];
        for m in new_history.iter().rev().take(20).rev() {
            cont.push(m.clone());
        }
        cont.push(ChatMessage::user(
            "若还需要执行命令，可继续用 run 块发出（AION 会自动执行）；否则直接给出最终结论。",
        ));
        let mut rounds = 0usize;
        loop {
            rounds += 1;
            if rounds > 6 {
                break;
            }
            let fu = match model.chat(&sec, None, &cont).await {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("agent followup error: {e}");
                    break;
                }
            };
            let parts2 = split_run_blocks(&fu);
            if !parts2.prose.is_empty() {
                blocks.push(serde_json::json!({
                    "type": "text",
                    "markdown": parts2.prose,
                }));
                if reply_text.is_empty() {
                    reply_text = parts2.prose.clone();
                } else {
                    reply_text = format!("{reply_text}\n\n{}", parts2.prose);
                }
                new_history.push(ChatMessage::assistant(parts2.prose.clone()));
            }
            if parts2.cmds.is_empty() {
                break;
            }
            for cmd in &parts2.cmds {
                let tc = ToolCall {
                    call_id: CallId::new(),
                    tool: "terminal.exec".into(),
                    arguments: serde_json::json!({ "command": cmd }),
                    sandbox: Some(ToolSandboxHint::Default),
                };
                tool_calls.push(serde_json::json!({
                    "call_id": tc.call_id.as_str(),
                    "tool": tc.tool,
                    "arguments": tc.arguments,
                }));
                let result = runtime.execute(&ctx, tc.clone(), sec.clone()).await;
                let result_json =
                    serde_json::to_value(&result).unwrap_or(serde_json::Value::Null);
                tool_results.push(result_json.clone());

                let output_text = summarize_tool_output(&result_json);
                blocks.push(terminal_block(cmd, &result_json));
                new_history.push(ChatMessage::user(format!(
                    "我发出的命令 `{cmd}` 已自动执行，输出：\n{output_text}"
                )));
            }
            if new_history.len() > 40 {
                let drop = new_history.len() - 40;
                new_history.drain(..drop);
            }
            cont = vec![ChatMessage::system(aion::agents::assistant::ASSISTANT_SYSTEM)];
            for m in new_history.iter().rev().take(20).rev() {
                cont.push(m.clone());
            }
            cont.push(ChatMessage::user(
                "若还需要执行命令，可继续用 run 块发出（AION 会自动执行）；否则直接给出最终结论。",
            ));
        }
    }

    if new_history.len() > 40 {
        let drop = new_history.len() - 40;
        new_history.drain(..drop);
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

/// ToolResult 序列化后形如 {"status":{...},"data":{...}}：成功载荷在 data 内，
/// 失败载荷在 status.message。这里取回真正承载结果的 data；error/denied 时把
/// 原因塞进 stderr 兜底，避免上层把“执行失败”误读成“静默无输出”。
fn tool_result_fields(result_json: &serde_json::Value) -> serde_json::Value {
    let data = result_json
        .get("data")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let status = result_json.get("status");
    let status_type = status
        .and_then(|s| s.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if status_type == "error" || status_type == "denied" {
        let msg = status
            .and_then(|s| s.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let mut d = data;
        if !msg.is_empty() {
            match &mut d {
                serde_json::Value::Object(map) => {
                    map.insert(
                        "stderr".into(),
                        serde_json::json!(format!("tool {status_type}: {msg}")),
                    );
                }
                _ => {
                    d = serde_json::json!({ "stderr": format!("tool {status_type}: {msg}") });
                }
            }
        }
        return d;
    }
    data
}

/// 把 ToolResult 的关键字段压缩成一段可回喂模型的文本。
fn summarize_tool_output(result_json: &serde_json::Value) -> String {
    let data = tool_result_fields(result_json);
    let mut parts: Vec<String> = Vec::new();
    if let Some(so) = data.get("stdout").and_then(|v| v.as_str()) {
        if !so.trim().is_empty() {
            parts.push(format!("stdout:\n{so}"));
        }
    }
    if let Some(se) = data.get("stderr").and_then(|v| v.as_str()) {
        if !se.trim().is_empty() {
            parts.push(format!("stderr:\n{se}"));
        }
    }
    if let Some(code) = data.get("exit_code").and_then(|v| v.as_i64()) {
        parts.push(format!("exit_code: {code}"));
    }
    if parts.is_empty() {
        "(无输出)".into()
    } else {
        parts.join("\n")
    }
}

/// 渲染一条 terminal UI 块（命令 + 真实 stdout/stderr/退出码）。
fn terminal_block(command: &str, result_json: &serde_json::Value) -> serde_json::Value {
    let data = tool_result_fields(result_json);
    serde_json::json!({
        "type": "terminal",
        "command": command,
        "output": data.get("stdout").and_then(|v| v.as_str()).unwrap_or_default(),
        "stderr": data.get("stderr").and_then(|v| v.as_str()).unwrap_or_default(),
        "code": data.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(-1),
        "timed_out": data.get("timed_out").and_then(|v| v.as_bool()).unwrap_or(false),
    })
}

/// 拆 `reply` 的产物：正文 + 一组 ```run 命令。
struct RunParts {
    prose: String,
    cmds: Vec<String>,
}

/// 从 markdown reply 里提取所有 ```run\n <cmd> \n``` 代码块，其余并入正文。
fn split_run_blocks(reply: &str) -> RunParts {
    let marker = "```run";
    let mut prose = String::new();
    let mut cmds = Vec::new();
    let mut rest = reply;
    loop {
        match rest.find(marker) {
            None => {
                prose.push_str(rest);
                break;
            }
            Some(idx) => {
                prose.push_str(&rest[..idx]);
                let after = &rest[idx + marker.len()..];
                // 吃掉 ```run 行尾（允许语言后缀），命令自下一行起
                let after = match after.find('\n') {
                    Some(nl) => &after[nl + 1..],
                    None => "",
                };
                match after.find("```") {
                    None => break,
                    Some(end) => {
                        let cmd = after[..end].trim().to_string();
                        if !cmd.is_empty() {
                            cmds.push(cmd);
                        }
                        let tail = &after[end + 3..];
                        rest = tail.strip_prefix('\n').unwrap_or(tail);
                    }
                }
            }
        }
    }
    RunParts {
        prose: prose.trim().to_string(),
        cmds,
    }
}

/// 流式 Chat：SSE 逐 token 输出；reply 中的 ```run 命令自动执行并把真实输出回喂，再流式续答。
async fn api_chat_stream(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Response {
    use tokio::sync::mpsc;
    let (tx, rx) = mpsc::unbounded_channel::<String>();
    let session_id = req
        .session_id
        .clone()
        .unwrap_or_else(|| format!("web-{}", std::process::id()));
    let _ = tx.send(
        serde_json::json!({ "type": "start", "session_id": session_id }).to_string(),
    );

    let st = state.clone();
    let input = req.message.clone();
    let session_key = session_id.clone();
    tokio::spawn(async move {
        let emit = |v: serde_json::Value| {
            let _ = tx.send(v.to_string());
        };

        let history: Vec<ChatMessage> = st
            .sessions
            .lock()
            .await
            .get(&session_key)
            .cloned()
            .unwrap_or_default();
        let sec = aion::agents::developer_sec("web-user", &[]);

        let model = match st.ctx.require::<ModelService>().await {
            Ok(m) => m,
            Err(e) => {
                emit(serde_json::json!({
                    "type": "error",
                    "message": format!("model unavailable: {e}")
                }));
                return;
            }
        };

        // 1) 首个回答：流式增量 → delta 事件（forwarder 把模型文本包成 JSON 事件）
        let (dtx, drx) = mpsc::unbounded_channel::<String>();
        let outer = tx.clone();
        let fwd = tokio::spawn(async move {
            let mut drx = drx;
            while let Some(text) = drx.recv().await {
                let _ = outer.send(
                    serde_json::json!({ "type": "delta", "text": text }).to_string(),
                );
            }
        });
        let msgs = aion::agents::assistant::chat_messages(&history, &input);
        let reply = match model.chat_stream(&sec, None, &msgs, dtx).await {
            Ok(r) => r,
            Err(e) => {
                emit(serde_json::json!({
                    "type": "error",
                    "message": format!("LLM error: {e}")
                }));
                return;
            }
        };
        // dtx 已在 chat_stream 内 drop；等增量转发排空后再封口首段
        let _ = fwd.await;

        let parts = split_run_blocks(&reply);
        emit(serde_json::json!({ "type": "seal", "final": parts.prose }));

        let mut new_history = history;
        new_history.push(ChatMessage::user(input.clone()));
        new_history.push(ChatMessage::assistant(parts.prose.clone()));

        // 2) 逐个执行 ```run 命令，事件实时推给前端
        for cmd in &parts.cmds {
            emit(serde_json::json!({
                "type": "tool",
                "tool": "terminal.exec",
                "command": cmd
            }));
            let tc = ToolCall {
                call_id: CallId::new(),
                tool: "terminal.exec".into(),
                arguments: serde_json::json!({ "command": cmd }),
                sandbox: Some(ToolSandboxHint::Default),
            };
            let result = st
                .tool_runtime
                .execute(&st.ctx, tc.clone(), sec.clone())
                .await;
            let result_json =
                serde_json::to_value(&result).unwrap_or(serde_json::Value::Null);
            let output_text = summarize_tool_output(&result_json);
            emit(terminal_block(cmd, &result_json));
            new_history.push(ChatMessage::user(format!(
                "我发出的命令 `{cmd}` 已自动执行，输出：\n{output_text}"
            )));
        }

        // 3) 有执行过命令 → 把真实输出回喂并进入有界 agentic 循环：
        //    模型后续回复若仍含 ```run 命令则继续自动执行→续答，直到模型不再发命令或到达轮次上限。
        if !parts.cmds.is_empty() {
            let mut cont = vec![ChatMessage::system(
                aion::agents::assistant::ASSISTANT_SYSTEM,
            )];
            for m in new_history.iter().rev().take(20).rev() {
                cont.push(m.clone());
            }
            cont.push(ChatMessage::user(
                "若还需要执行命令，可继续用 run 块发出（AION 会自动执行）；否则直接给出最终结论。",
            ));
            let mut rounds = 0usize;
            loop {
                rounds += 1;
                if rounds > 6 {
                    break;
                }
                let (dtx2, drx2) = mpsc::unbounded_channel::<String>();
                let outer2 = tx.clone();
                let fwd2 = tokio::spawn(async move {
                    let mut drx2 = drx2;
                    while let Some(text) = drx2.recv().await {
                        let _ = outer2.send(
                            serde_json::json!({ "type": "delta", "text": text }).to_string(),
                        );
                    }
                });
                let turn = match model.chat_stream(&sec, None, &cont, dtx2).await {
                    Ok(t) => t,
                    Err(e) => {
                        emit(serde_json::json!({
                            "type": "error",
                            "message": format!("followup error: {e}")
                        }));
                        break;
                    }
                };
                let _ = fwd2.await;

                let parts2 = split_run_blocks(&turn);
                if !parts2.prose.is_empty() {
                    emit(serde_json::json!({
                        "type": "seal",
                        "final": parts2.prose
                    }));
                    new_history.push(ChatMessage::assistant(parts2.prose.clone()));
                }
                if parts2.cmds.is_empty() {
                    break;
                }
                for cmd in &parts2.cmds {
                    emit(serde_json::json!({
                        "type": "tool",
                        "tool": "terminal.exec",
                        "command": cmd
                    }));
                    let tc = ToolCall {
                        call_id: CallId::new(),
                        tool: "terminal.exec".into(),
                        arguments: serde_json::json!({ "command": cmd }),
                        sandbox: Some(ToolSandboxHint::Default),
                    };
                    let result = st
                        .tool_runtime
                        .execute(&st.ctx, tc.clone(), sec.clone())
                        .await;
                    let result_json =
                        serde_json::to_value(&result).unwrap_or(serde_json::Value::Null);
                    let output_text = summarize_tool_output(&result_json);
                    emit(terminal_block(cmd, &result_json));
                    new_history.push(ChatMessage::user(format!(
                        "我发出的命令 `{cmd}` 已自动执行，输出：\n{output_text}"
                    )));
                }
                if new_history.len() > 40 {
                    let drop = new_history.len() - 40;
                    new_history.drain(..drop);
                }
                cont = vec![ChatMessage::system(
                    aion::agents::assistant::ASSISTANT_SYSTEM,
                )];
                for m in new_history.iter().rev().take(20).rev() {
                    cont.push(m.clone());
                }
                cont.push(ChatMessage::user(
                    "若还需要执行命令，可继续用 run 块发出（AION 会自动执行）；否则直接给出最终结论。",
                ));
            }
        }

        if new_history.len() > 40 {
            let drop = new_history.len() - 40;
            new_history.drain(..drop);
        }
        st.sessions.lock().await.insert(session_key, new_history);
        emit(serde_json::json!({ "type": "done", "session_id": session_id }));
    });

    let stream = fstream::unfold(rx, |rx| async move {
        let mut rx = rx;
        match rx.recv().await {
            Some(payload) => Some((Ok::<_, Infallible>(Event::default().data(payload)), rx)),
            None => None,
        }
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
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

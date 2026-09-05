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

use aion_protocol::llm_schema::tool_to_anthropic;
use aion_protocol::prelude::*;

use aion_services::model::{
    ChatMessage, ModelService, ToolResultBlock, ToolUseBlock,
};
use aion_services::security::SecurityContext;
use aion_services::{
    backend_from_provider, LlmProtocol, LlmProvider, LlmProviderStore,
};

/// 一条等待用户确认的挂起命令（登记于 `AppState.pending`）。
struct PendingApproval {
    /// 用户同意后真正执行的 ToolCall（含 command 与 call_id）。
    pub tool_call: ToolCall,
    /// 发起它的会话。
    pub session_id: String,
    /// 原始命令串（用于展示 / 历史回填）。
    pub command: String,
}

/// Web 层共享状态。
pub struct AppState {
    pub ctx: cordis::Context,
    pub tool_runtime: Arc<aion_services::tool::ToolRuntime>,
    /// cc-switch 式 LLM 供应商存储（持久化于 aion.providers.json）。
    pub llm_store: Arc<LlmProviderStore>,
    /// 会话历史（内存态，Phase 3 简化版）。
    pub sessions: tokio::sync::Mutex<HashMap<String, Vec<ChatMessage>>>,
    /// 等待用户确认的挂起动作（key = RequestId，与 UIBlock::Confirmation 配对）。
    pending: tokio::sync::Mutex<HashMap<RequestId, PendingApproval>>,
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
        pending: tokio::sync::Mutex::new(HashMap::new()),
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/logo.png", get(logo_handler))
        .route("/bg-wust.jpg", get(bg_handler))
        .route("/api/health", get(api_health))
        .route("/api/tools", get(api_tools))
        .route("/api/chat", post(api_chat))
        .route("/api/chat/stream", post(api_chat_stream))
        .route("/api/action", post(api_action))
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

// ---------------------------------------------------------------------------
// 静态资源 —— 磁盘优先、内嵌兜底
//
// 前端(static/index.html)不再只靠 include_str! 编译期内嵌:每次改 UI
// 都要重编 Rust 太慢。现改为:若进程环境里有 `AION_STATIC_DIR`(指向可
// 热更新的前端目录),就**每个请求现读磁盘文件** —— 编辑 + git pull 后
// 立即生效,无需重编、甚至无需重启;目录缺失/读失败时回退到内嵌版本,
// 保证二进制脱离源码树也能独立跑。
// ---------------------------------------------------------------------------

/// 编译期内嵌版本(兜底,保证自包含)。
const EMBED_INDEX: &str = include_str!("../static/index.html");
const EMBED_LOGO: &[u8] = include_bytes!("../static/logo.png");
/// 桌面登录背景(武科大校园实景)。磁盘优先:直接换 static/bg-wust.jpg 即热更。
const EMBED_BG: &[u8] = include_bytes!("../static/bg-wust.jpg");

/// 磁盘前端目录:`AION_STATIC_DIR`,未设置返回 None(走内嵌)。
fn static_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("AION_STATIC_DIR").map(std::path::PathBuf::from)
}

/// 读磁盘前端文件;目录未配置或文件不存在返回 None。
async fn disk_static(name: &str) -> Option<Vec<u8>> {
    let dir = static_dir()?;
    match tokio::fs::read(dir.join(name)).await {
        Ok(b) => Some(b),
        Err(_) => None,
    }
}

async fn index_handler() -> Response {
    match disk_static("index.html").await {
        Some(bytes) => {
            let body = String::from_utf8_lossy(&bytes).into_owned();
            Html(body).into_response()
        }
        None => Html(EMBED_INDEX).into_response(),
    }
}

async fn logo_handler() -> Response {
    let bytes = match disk_static("logo.png").await {
        Some(b) => b,
        None => EMBED_LOGO.to_vec(),
    };
    Response::builder()
        .header("Content-Type", "image/png")
        .body(axum::body::Body::from(bytes))
        .unwrap()
}

async fn bg_handler() -> Response {
    let bytes = match disk_static("bg-wust.jpg").await {
        Some(b) => b,
        None => EMBED_BG.to_vec(),
    };
    Response::builder()
        .header("Content-Type", "image/jpeg")
        .body(axum::body::Body::from(bytes))
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

/// 用户对前端 UI 的某个 Action 做出的选择（确认 / 取消）。
/// 与 `aion-protocol` 的 `UIAction` 同构，直接反序列化。
///
/// 不需要 session_id：被确认的待批命令挂在 `AppState.pending` 里，
/// 其归属的 session 由挂起项自带。
#[derive(Deserialize)]
pub struct ActionRequest {
    pub action: UIAction,
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

// ---------------------------------------------------------------------------
// 同意闭环：危险命令在真正执行前弹确认框；用户同意 / 拒绝后精确恢复 / 取消。
// 只读工具直接跑；`terminal.exec` 的高危命令串经 `effective_risk` 判定后挂起。
// ---------------------------------------------------------------------------

/// 结果是否为「等待用户确认」的挂起态。
fn result_is_pending(r: &ToolResult) -> bool {
    matches!(r.status, ResultStatus::Pending { .. })
}

/// 序列化一个 UIBlock。
fn block_value(b: &UIBlock) -> serde_json::Value {
    serde_json::to_value(b).unwrap_or(serde_json::Value::Null)
}

/// terminal 危险命令的确认框（options = 同意[Danger] / 拒绝[Ghost]，默认拒绝）。
fn terminal_confirm_block(request_id: &RequestId, command: &str) -> UIBlock {
    UIBlock::Confirmation(ConfirmationBlock {
        request_id: request_id.clone(),
        title: "确认执行命令".into(),
        description: format!("Agent 请求执行命令：\n\n```sh\n{command}\n```"),
        consequences: vec![
            "检测到高危 / 破坏性命令，执行后可能无法撤销。".to_string(),
            "该命令可能导致数据删除、系统状态改变等不可逆结果。".to_string(),
            "仅在完全信任时才选择「同意执行」。".to_string(),
        ],
        options: vec![
            ConfirmationOption {
                choice: "confirm".into(),
                label: "同意执行".into(),
                description: None,
                style: ConfirmationStyle::Danger,
            },
            ConfirmationOption {
                choice: "cancel".into(),
                label: "拒绝".into(),
                description: None,
                style: ConfirmationStyle::Ghost,
            },
        ],
        default_choice: "cancel".into(),
    })
}

/// 带同意门的单步执行：
/// - 低风险命令 → 直接执行，返回真实结果；
/// - 高风险命令 → 登记挂起（`AppState.pending`），返回 `ResultStatus::Pending`，
///   `events` 携带 Confirmation 块；命令此时**不会执行**。
async fn run_consented(
    state: &AppState,
    ctx: &cordis::Context,
    runtime: &aion_services::tool::ToolRuntime,
    sec: &SecurityContext,
    session_id: &str,
    tc: &ToolCall,
) -> ToolResult {
    if !runtime.effective_risk(tc).requires_confirmation() {
        return runtime.execute(ctx, tc.clone(), sec.clone()).await;
    }
    let command = tc
        .arguments
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let request_id = RequestId::new();
    let block = terminal_confirm_block(&request_id, &command);
    state.pending.lock().await.insert(
        request_id.clone(),
        PendingApproval {
            tool_call: tc.clone(),
            session_id: session_id.to_string(),
            command: command.clone(),
        },
    );
    ToolResult {
        call_id: tc.call_id.clone(),
        status: ResultStatus::Pending {
            request_id,
            summary: format!("等待你确认执行：{command}"),
        },
        data: serde_json::json!({ "command": command }),
        artifacts: Vec::new(),
        events: vec![block],
    }
}

/// 一次有界「续答」循环的结果。
struct AgenticOutcome {
    reply_text: String,
    blocks: Vec<serde_json::Value>,
    tool_calls: Vec<serde_json::Value>,
    tool_results: Vec<serde_json::Value>,
    /// 是否因遇到待确认命令而暂停（此时 blocks 含确认框）。
    paused: bool,
}

/// 组装每轮请求的 system：ASSISTANT_SYSTEM + 当前注册工具清单。
/// 工具细节走原生 `tools` 参数；这里用一句话清单让模型先知道有什么可用。
fn assistant_system_with_tools(runtime: &aion_services::tool::ToolRuntime) -> String {
    let mut list = String::new();
    for d in runtime.registry().list() {
        list.push_str(&format!("- {}：{}\n", d.name, d.description));
    }
    format!(
        "{}\n\n# 可用工具\n\
         本次对话我已把工具以原生 `tools` 形式提供；需要查询/修改本机状态时请直接发起工具调用，\
         不要臆造输出。\n\
         可用工具：\n{list}\n\
         # 回退\n\
         若拿不到 `tools`（纯文本模式），需要执行命令时仍可用 ```run 代码块发出，AION 会自动执行并回喂输出。",
        aion::agents::assistant::ASSISTANT_SYSTEM
    )
}

/// 把注册表里的工具转成 Anthropic `tools` 数组（模型原生调用协议）。
fn build_llm_tools(runtime: &aion_services::tool::ToolRuntime) -> Vec<serde_json::Value> {
    runtime
        .registry()
        .list()
        .iter()
        .map(tool_to_anthropic)
        .collect()
}

/// 结果是否为执行失败（error / denied）。
fn result_is_error(result_json: &serde_json::Value) -> bool {
    let t = result_json
        .pointer("/status/type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    t == "error" || t == "denied"
}

/// 回喂给模型的工具输出文本。
///
/// `terminal.exec` 走 stdout/stderr/exit_code 摘要；结构化工具（file.list /
/// process.list / system.stats / file.read）没有 stdout，直接把 data 压成 JSON，
/// 让模型看得到真实内容（否则会得到空输出而无法继续）。
fn summarize_tool_data(result_json: &serde_json::Value) -> String {
    let data = tool_result_fields(result_json);
    let has_term = data
        .get("stdout")
        .and_then(|v| v.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
        || data
            .get("stderr")
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
    if has_term {
        return summarize_tool_output(result_json);
    }
    if data.is_null()
        || data
            .as_object()
            .map(|m| m.is_empty())
            .unwrap_or(false)
    {
        return "(无输出)".into();
    }
    let s = serde_json::to_string(&data).unwrap_or_default();
    let s: String = s.chars().take(6000).collect();
    s
}

/// 取历史尾部最近 n 条；若开头是一条「只有 tool_result 的 user 消息」——
/// 说明它对应的 assistant tool_use 已被裁掉，会触发 Anthropic 400，整条丢弃。
fn recent_context(history: &[ChatMessage], n: usize) -> Vec<ChatMessage> {
    let mut tail: Vec<&ChatMessage> = history.iter().rev().take(n).collect();
    tail.reverse();
    let mut i = 0;
    while i < tail.len() && tail[i].role == "user" && !tail[i].tool_results.is_empty() {
        i += 1;
    }
    tail[i..].iter().map(|m| (*m).clone()).collect()
}

/// 一次有界「原生工具」循环（三个调用点共用）：
///
/// 每轮用 `model.chat_turn(messages, tools)` 拿到正文 + 一串 `tool_use`，
/// 逐个 `run_consented`（风险门 + 真人同意）执行；结果映射 UIBlock 并回填
/// `tool_result` 进 `new_history`，再进下一轮。上限 6 轮。
///
/// - 无 `tool_use` 且正文含 ```run → 走旧文本回退（模型没原生工具能力时仍能跑命令）。
/// - 无 `tool_use` 且无命令 → 最终答复，收束。
/// - 遇到待确认的高危命令 → 挂起（返回 `paused`），保留确认块等 `/api/action` 恢复。
/// - `tx` 为 `Some`（流式路径）时实时发 delta / seal / tool chip / 块事件；
///   `None`（非流式 / action 恢复）只累积进返回的 `AgenticOutcome`。
async fn run_agentic_loop(
    state: &AppState,
    ctx: &cordis::Context,
    runtime: &aion_services::tool::ToolRuntime,
    sec: &SecurityContext,
    session_id: &str,
    new_history: &mut Vec<ChatMessage>,
    tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
) -> AgenticOutcome {
    let empty = || AgenticOutcome {
        reply_text: String::new(),
        blocks: Vec::new(),
        tool_calls: Vec::new(),
        tool_results: Vec::new(),
        paused: false,
    };

    // 开发命令「渲染演示」：不调 LLM，直接铺出全部渲染器的样例块，供桌面/前端看画布富度
    let last_user = new_history
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.trim())
        .unwrap_or("");
    if last_user == "渲染演示" || last_user == "render demo" {
        let mut out = empty();
        out.blocks = renderer_demo_blocks();
        if let Some(t) = &tx {
            for b in &out.blocks {
                let _ = t.send(b.to_string());
            }
        }
        new_history.push(ChatMessage::assistant("已展示全部渲染器样例。".to_string()));
        return out;
    }
    // 开发命令「弹 <URL>」/「弹一个百度」：绕过模型，确定性走真实工具链
    // （run_consented → runtime.execute → result_to_blocks），跳过模型以做验证。
    // 普通站走 web.fetch（原生重排）；任意/动态/JS 站走 web.read（Moli 无头引擎跑 JS 后出真实正文）。
    if let Some(pop_url) = parse_dev_pop(&last_user) {
        let mut out = empty();
        let tu = ToolUseBlock {
            id: CallId::new().as_str().to_string(),
            name: if pop_url.contains("baidu.com") {
                "web.fetch"
            } else {
                "web.read"
            }
            .into(),
            input: serde_json::json!({ "url": pop_url }),
        };
        let tc = ToolCall {
            call_id: CallId::new(),
            tool: tu.name.clone(),
            arguments: tu.input.clone(),
            sandbox: Some(ToolSandboxHint::Default),
        };
        let result = run_consented(state, ctx, runtime, sec, session_id, &tc).await;
        let rj = serde_json::to_value(&result).unwrap_or(serde_json::Value::Null);
        for b in result_to_blocks(&tu, &rj) {
            out.blocks.push(b.clone());
            if let Some(t) = &tx {
                let _ = t.send(b.to_string());
            }
        }
        new_history.push(ChatMessage::assistant(format!("已读取 {pop_url} 并入画布。")));
        return out;
    }
    let model = match ctx.require::<ModelService>().await {
        Ok(m) => m,
        Err(e) => {
            if let Some(t) = &tx {
                let _ = t.send(
                    serde_json::json!({ "type": "error", "message": format!("model unavailable: {e}") })
                        .to_string(),
                );
            } else {
                eprintln!("model unavailable: {e}");
            }
            return empty();
        }
    };
    let tools = build_llm_tools(runtime);
    let mut out = empty();
    let mut rounds = 0usize;
    loop {
        rounds += 1;
        if rounds > 6 {
            break;
        }
        let system = ChatMessage::system(assistant_system_with_tools(runtime));
        let recent = recent_context(new_history, 20);
        let mut cont = Vec::with_capacity(recent.len() + 1);
        cont.push(system);
        cont.extend(recent);

        // 文本增量转发（仅流式）
        let (dtx, drx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let fwd = tx.as_ref().map(|outer| {
            let outer = outer.clone();
            tokio::spawn(async move {
                let mut drx = drx;
                while let Some(text) = drx.recv().await {
                    let _ = outer.send(
                        serde_json::json!({ "type": "delta", "text": text }).to_string(),
                    );
                }
            })
        });

        let turn = match model.chat_turn(sec, None, &cont, &tools, dtx).await {
            Ok(t) => t,
            Err(e) => {
                eprintln!("agent turn error: {e}");
                if let Some(t) = &tx {
                    let _ = t.send(
                        serde_json::json!({ "type": "error", "message": format!("LLM error: {e}") })
                            .to_string(),
                    );
                } else {
                    out.blocks.push(serde_json::json!({
                        "type": "text",
                        "markdown": format!("⚠️ LLM error: {e}"),
                    }));
                }
                break;
            }
        };
        if let Some(f) = fwd {
            let _ = f.await;
        }

        // 展示 / 存储正文 = 剥掉 ```run 的 prose
        let rp = split_run_blocks(&turn.text);
        let prose = rp.prose;
        if !prose.is_empty() {
            if let Some(t) = &tx {
                let _ = t.send(serde_json::json!({ "type": "seal", "final": prose }).to_string());
            }
            out.blocks.push(serde_json::json!({ "type": "text", "markdown": prose }));
            if out.reply_text.is_empty() {
                out.reply_text = prose.clone();
            } else {
                out.reply_text = format!("{}\n\n{}", out.reply_text, prose);
            }
        }

        // 原生 tool_use 优先；原生为空但正文里带 ```run → 文本回退
        let mut tuses: Vec<ToolUseBlock> = turn.tool_uses;
        if tuses.is_empty() && !rp.cmds.is_empty() {
            tuses = rp
                .cmds
                .into_iter()
                .map(|cmd| ToolUseBlock {
                    id: CallId::new().as_str().to_string(),
                    name: "terminal.exec".into(),
                    input: serde_json::json!({ "command": cmd }),
                })
                .collect();
        }
        if tuses.is_empty() {
            // 最终答复：无工具可执行
            if !prose.is_empty() {
                new_history.push(ChatMessage::assistant(prose));
            }
            break;
        }

        // 逐个执行；遇待确认 → 挂起本轮（后续 tool_use 丢弃，等用户决定）
        let mut executed: Vec<ToolUseBlock> = Vec::new();
        let mut results: Vec<ToolResultBlock> = Vec::new();
        let mut paused = false;
        for tu in tuses {
            let cmd = tu.input.get("command").and_then(|v| v.as_str()).unwrap_or_default();
            if let Some(t) = &tx {
                let _ = t.send(
                    serde_json::json!({ "type": "tool", "tool": tu.name, "command": cmd })
                        .to_string(),
                );
            }
            let tc = ToolCall {
                call_id: CallId::new(),
                tool: tu.name.clone(),
                arguments: tu.input.clone(),
                sandbox: Some(ToolSandboxHint::Default),
            };
            out.tool_calls.push(serde_json::json!({
                "call_id": tc.call_id.as_str(),
                "tool": tc.tool.clone(),
                "arguments": tc.arguments.clone(),
            }));
            let result = run_consented(state, ctx, runtime, sec, session_id, &tc).await;
            if result_is_pending(&result) {
                paused = true;
                for b in &result.events {
                    let bv = block_value(b);
                    out.blocks.push(bv.clone());
                    if let Some(t) = &tx {
                        let _ = t.send(bv.to_string());
                    }
                }
                break;
            }
            executed.push(tu.clone());
            let result_json = serde_json::to_value(&result).unwrap_or(serde_json::Value::Null);
            out.tool_results.push(result_json.clone());
            for b in result_to_blocks(&tu, &result_json) {
                out.blocks.push(b.clone());
                if let Some(t) = &tx {
                    let _ = t.send(b.to_string());
                }
            }
            results.push(ToolResultBlock {
                tool_use_id: tu.id.clone(),
                content: summarize_tool_data(&result_json),
                is_error: result_is_error(&result_json),
            });
        }

        // 历史回填：assistant(正文 + 已执行 tool_use) → user(tool_result / 挂起说明)
        let executed_empty = executed.is_empty();
        if !executed_empty || !prose.is_empty() {
            new_history.push(ChatMessage {
                role: "assistant".into(),
                content: prose.clone(),
                tool_uses: executed,
                tool_results: Vec::new(),
            });
        }
        if paused {
            new_history.push(ChatMessage {
                role: "user".into(),
                content: "有一个操作需要你确认后才能执行，已暂停。".into(),
                tool_uses: Vec::new(),
                tool_results: results,
            });
            out.paused = true;
            break;
        }
        if !results.is_empty() {
            new_history.push(ChatMessage {
                role: "user".into(),
                content: String::new(),
                tool_uses: Vec::new(),
                tool_results: results,
            });
        } else if executed_empty {
            // 既没执行任何工具也没结果可回填——收束，避免死循环
            break;
        }
        if new_history.len() > 40 {
            let drop = new_history.len() - 40;
            new_history.drain(..drop);
        }
    }
    out
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = bytes as f64;
    let mut i = 0usize;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1}{}", UNITS[i])
    }
}

/// ToolResult → 前端 UIBlock。terminal 归 terminal 块；file.list / process.list
/// 出 Table；file.read / system.stats 出 text；错误给文本提示。
fn result_to_blocks(tu: &ToolUseBlock, result_json: &serde_json::Value) -> Vec<serde_json::Value> {
    if result_is_error(result_json) {
        return vec![serde_json::json!({
            "type": "text",
            "markdown": format!("⚠️ `{}` 执行失败：{}", tu.name, summarize_tool_output(result_json)),
        })];
    }
    let data = tool_result_fields(result_json);
    match tu.name.as_str() {
        "terminal.exec" => {
            let command = tu
                .input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            vec![terminal_block(command, result_json)]
        }
        "file.list" => {
            let entries = data
                .get("entries")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let headers: Vec<String> =
                ["名称", "类型", "大小"].iter().map(|s| s.to_string()).collect();
            let rows: Vec<serde_json::Value> = entries
                .iter()
                .map(|e| {
                    let is_dir = e.get("is_dir").and_then(|v| v.as_bool()).unwrap_or(false);
                    let size = if is_dir {
                        "-".to_string()
                    } else {
                        e.get("size")
                            .and_then(|v| v.as_u64())
                            .map(human_size)
                            .unwrap_or_else(|| "-".into())
                    };
                    serde_json::json!([
                        e.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        if is_dir { "dir" } else { "file" },
                        size,
                    ])
                })
                .collect();
            let path = data.get("path").and_then(|v| v.as_str()).unwrap_or("");
            vec![serde_json::json!({
                "type": "table",
                "headers": headers,
                "rows": rows,
                "caption": format!("{path} · {} 项", entries.len()),
            })]
        }
        "process.list" => {
            let procs = data
                .get("processes")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let headers: Vec<String> = ["ticket_id", "pid", "sandboxed", "cgroup"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            let rows: Vec<serde_json::Value> = procs
                .iter()
                .map(|p| {
                    serde_json::json!([
                        p.get("ticket_id").and_then(|v| v.as_str()).unwrap_or(""),
                        p.get("pid").and_then(|v| v.as_i64()).unwrap_or(0),
                        p.get("sandboxed").and_then(|v| v.as_bool()).unwrap_or(false),
                        p.get("cgroup").and_then(|v| v.as_str()).unwrap_or(""),
                    ])
                })
                .collect();
            vec![serde_json::json!({
                "type": "table",
                "headers": headers,
                "rows": rows,
                "caption": format!("{} 个进程", procs.len()),
            })]
        }
        "file.read" => {
            let content = data
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let truncated = data
                .get("truncated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let path = data.get("path").and_then(|v| v.as_str()).unwrap_or("文件");
            let md = if content.trim().is_empty() {
                format!("📄 `{path}`：空文件（或不可读）")
            } else {
                let body: String = content.chars().take(6000).collect();
                let mark = if truncated || content.chars().count() > 6000 {
                    "\n…[内容已截断]"
                } else {
                    ""
                };
                format!("📄 `{path}`\n\n```\n{body}{mark}\n```")
            };
            vec![serde_json::json!({ "type": "text", "markdown": md })]
        }
        "system.stats" => {
            // 富 stats 网格块：前端 renderStats 把 cpu/memory/load/uptime 画成实时面板
            vec![serde_json::json!({
                "type": "stats",
                "data": data,
            })]
        }
        "web.fetch" => {
            // 网页 → AION 自己画的「网页感」原生卡片（站点感知重建）：
            // 前端 renderWebpage 按结构布局——顶栏站名/导航、hero logo/标语、
            // 搜索框、页脚。素材(logo/导航/页脚)与主色来自服务端提取的真实页面。
            let url = data.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let title = data
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let tagline = data
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let nav = data.get("nav").cloned().unwrap_or(serde_json::json!([]));
            let footer = data.get("footer").cloned().unwrap_or(serde_json::json!([]));
            let search = data.get("search").cloned().unwrap_or(serde_json::Value::Null);
            let status = data.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
            let bytes = data.get("bytes").and_then(|v| v.as_u64()).unwrap_or(0);
            vec![serde_json::json!({
                "type": "webpage",
                "url": url,
                "title": if title.is_empty() { "（页面无标题）" } else { title.as_str() },
                "tagline": tagline,
                "brand": data.get("brand_name").and_then(|v| v.as_str()).unwrap_or(""),
                "color": data.get("brand_color").and_then(|v| v.as_str()).unwrap_or(""),
                "logo": data.get("logo").and_then(|v| v.as_str()).unwrap_or(""),
                "nav": nav,
                "search": search,
                "footer": footer,
                "meta": format!("HTTP {status} · {}", human_size(bytes)),
            })]
        }
        "web.read" => {
            // Moli 无头引擎读回来的正文（跑完 JS 的真实内容）→ markdown 块。
            let url = data.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let content = data.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let empty = data.get("empty").and_then(|v| v.as_bool()).unwrap_or(false);
            if empty {
                return vec![serde_json::json!({
                    "type": "text",
                    "markdown": format!(
                        "🌐 `{url}`\n\n{}",
                        data.get("hint")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Moli 读到空内容")
                    ),
                })];
            }
            let chars = data.get("chars").and_then(|v| v.as_u64()).unwrap_or(0);
            let body: String = content.chars().take(6000).collect();
            let clipped = body.chars().count() < content.chars().count();
            let meta = if clipped {
                format!("`Moli 渲染 · 全文 {chars} 字，以下为节选`")
            } else {
                format!("`Moli 渲染 · {chars} 字`")
            };
            vec![serde_json::json!({
                "type": "markdown",
                "markdown": format!("{meta}\n\n{body}"),
            })]
        }
        _ => {
            let pretty = serde_json::to_string_pretty(&data).unwrap_or_default();
            let body: String = pretty.chars().take(3000).collect();
            vec![serde_json::json!({
                "type": "text",
                "markdown": format!("`{}`\n```json\n{body}\n```", tu.name),
            })]
        }
    }
}

/// 开发命令「弹 …」的 URL 解析。识别：`弹一个百度`/`打开百度`/`baidu demo`，
/// 以及 `弹 https://…` 这类带显式 URL 的输入；非弹站命令返回 None（走正常 LLM 流程）。
fn parse_dev_pop(msg: &str) -> Option<String> {
    let msg = msg.trim();
    if msg == "弹一个百度" || msg == "打开百度" || msg == "baidu demo" {
        return Some("https://www.baidu.com".to_string());
    }
    let rest = msg.strip_prefix("弹")?.trim();
    let rest = rest
        .trim_start_matches(|c| c == ':' || c == '：')
        .trim();
    if rest.starts_with("http://") || rest.starts_with("https://") {
        Some(rest.to_string())
    } else {
        None
    }
}

/// 「渲染演示」用的样例块：每种 UIBlock 渲染器各出一块，前端一次铺开看画布富度。
fn renderer_demo_blocks() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({ "type": "text", "markdown": "**文本块** · 内联 markdown，`等宽` 也行" }),
        serde_json::json!({
            "type": "markdown",
            "source": "# 一级标题\n\n正文段落，支持 **加粗**、*斜体*、`行内码`。\n\n- 列表项 A\n- 列表项 B\n- 列表项 C\n\n> 引用：这是网页正文经 AION 原生重排后的样子。\n\n[链接 → 武科大官网](https://www.wust.edu.cn)"
        }),
        serde_json::json!({
            "type": "table",
            "headers": ["名称", "类型", "大小"],
            "rows": [
                ["index.html", "file", "28.6K"],
                ["bg-wust.jpg", "file", "459.1K"],
                ["assets", "dir", "-"],
                ["src", "dir", "-"],
            ],
            "caption": "文件列表 · 4 项",
        }),
        serde_json::json!({
            "type": "terminal",
            "command": "uname -a",
            "output": "Linux aion-host 6.1.0-20-amd64 #1 SMP x86_64 GNU/Linux",
            "code": 0,
        }),
        serde_json::json!({
            "type": "chart",
            "kind": "line",
            "title": "CPU 负载趋势",
            "series": [
                { "name": "user", "points": [{ "x": 0, "y": 12 }, { "x": 1, "y": 25 }, { "x": 2, "y": 18 }, { "x": 3, "y": 40 }, { "x": 4, "y": 33 }] },
                { "name": "system", "points": [{ "x": 0, "y": 5 }, { "x": 1, "y": 8 }, { "x": 2, "y": 15 }, { "x": 3, "y": 12 }, { "x": 4, "y": 20 }] }
            ],
        }),
        serde_json::json!({
            "type": "chart",
            "kind": "bar",
            "title": "内存占用（GB）",
            "bars": [
                { "label": "已用", "value": 7.2 },
                { "label": "缓存", "value": 3.1 },
                { "label": "可用", "value": 4.5 },
                { "label": "Swap", "value": 1.2 }
            ],
        }),
        serde_json::json!({
            "type": "chart",
            "kind": "points",
            "title": "采样散点",
            "series": [
                { "name": "cpu vs io", "points": [{ "x": 1, "y": 2 }, { "x": 2, "y": 5 }, { "x": 3, "y": 3 }, { "x": 4, "y": 8 }, { "x": 5, "y": 6 }] }
            ],
        }),
        serde_json::json!({
            "type": "image",
            "src": "/logo.png",
            "alt": "AION 徽标（本地资源示例）",
            "width": 200,
        }),
        serde_json::json!({
            "type": "file",
            "path": "/home/wust_1/AION/README.md",
            "kind": "source",
            "mime": "text/markdown",
            "size": 2048,
        }),
        serde_json::json!({
            "type": "stats",
            "data": {
                "cpu": { "user": 8, "nice": 0, "system": 3, "idle": 85, "iowait": 0 },
                "memory": { "total_bytes": 17_179_869_184_i64, "available_bytes": 8_388_608_000_i64 },
                "load": { "load1": 0.42, "load5": 0.35, "load15": 0.3 },
                "uptime_seconds": 3723.0
            },
        }),
        serde_json::json!({
            "type": "process",
            "data": {
                "processes": [
                    { "pid": 1, "name": "systemd", "state": "running" },
                    { "pid": 1200, "name": "aion-web", "state": "running" },
                    { "pid": 1242, "name": "aion-desktop", "state": "running" }
                ]
            },
        }),
    ]
}

async fn api_chat(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, StatusCode> {
    let ctx = state.ctx.clone();
    let runtime = state.tool_runtime.clone();
    let sec = aion::agents::developer_sec("web-user", &[]);
    let session_id = req
        .session_id
        .clone()
        .unwrap_or_else(|| format!("web-{}", std::process::id()));

    // 会话历史 + 本次输入，进统一工具循环
    let mut new_history: Vec<ChatMessage> = state
        .sessions
        .lock()
        .await
        .get(&session_id)
        .cloned()
        .unwrap_or_default();
    new_history.push(ChatMessage::user(req.message.clone()));

    let out = run_agentic_loop(
        &state,
        &ctx,
        &runtime,
        &sec,
        &session_id,
        &mut new_history,
        None,
    )
    .await;

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
        reply_text: out.reply_text,
        blocks: out.blocks,
        actions: vec![],
        tool_calls: out.tool_calls,
        tool_results: out.tool_results,
    }))
}

/// 用户对确认框做出选择（`POST /api/action`）——同意闭环的「恢复」半边。
///
/// 从 `AppState.pending` 取出挂起的待批命令：
/// - `Confirm{choice:"confirm"}`：**真正执行**那条命令，把结果写进该 session 历史，
///   再进入续答循环让 agent 接着往下做；
/// - `Confirm{choice:其它}` / `Cancel`：丢弃该命令，仅记「用户拒绝了操作」。
///
/// 返回与 `/api/chat` 相同的 `ChatResponse` 形状，前端可复用同一渲染逻辑。
async fn api_action(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ActionRequest>,
) -> Result<Json<ChatResponse>, StatusCode> {
    // 只有确认框相关的 Action 才允许在挂起表里寻址
    let request_id = match &req.action {
        UIAction::Confirm { request_id, .. } => request_id.clone(),
        UIAction::Cancel { request_id } => request_id.clone(),
        UIAction::Invoke { .. } => return Err(StatusCode::BAD_REQUEST),
    };

    // 取走挂起项：不存在 = 已处理 / 已过期
    let approval = state
        .pending
        .lock()
        .await
        .remove(&request_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    // Confirm 且选择 "confirm" 才算真正同意；其它（拒绝 / cancel）都丢弃
    let approved = matches!(&req.action, UIAction::Confirm { choice, .. } if choice == "confirm");
    let session_id = approval.session_id.clone();

    let ctx = state.ctx.clone();
    let runtime = state.tool_runtime.clone();
    let sec = aion::agents::developer_sec("web-user", &[]);

    let mut history: Vec<ChatMessage> = state
        .sessions
        .lock()
        .await
        .get(&session_id)
        .cloned()
        .unwrap_or_default();

    let mut reply_text = String::new();
    let mut blocks: Vec<serde_json::Value> = Vec::new();
    let mut tool_calls = Vec::new();
    let mut tool_results = Vec::new();

    if approved {
        let command = approval.command.clone();
        let tc = approval.tool_call.clone();
        tool_calls.push(serde_json::json!({
            "call_id": tc.call_id.as_str(),
            "tool": tc.tool.clone(),
            "arguments": tc.arguments.clone(),
        }));
        let result = runtime.execute(&ctx, tc, sec.clone()).await;
        let result_json = serde_json::to_value(&result).unwrap_or(serde_json::Value::Null);
        tool_results.push(result_json.clone());

        blocks.push(terminal_block(&command, &result_json));
        let output_text = summarize_tool_output(&result_json);
        history.push(ChatMessage::user(format!(
            "用户已同意执行 `{command}`，输出：\n{output_text}"
        )));

        // 同意后让 agent 接着用原生工具循环续答（可能继续调工具，也可能收尾）
        let cont = run_agentic_loop(
            &state,
            &ctx,
            &runtime,
            &sec,
            &session_id,
            &mut history,
            None,
        )
        .await;
        reply_text = cont.reply_text;
        blocks.extend(cont.blocks);
        tool_calls.extend(cont.tool_calls);
        tool_results.extend(cont.tool_results);
    } else {
        history.push(ChatMessage::user("用户拒绝了该操作，未执行。".to_string()));
        blocks.push(serde_json::json!({
            "type": "text",
            "markdown": "已拒绝该操作，未执行任何命令。",
        }));
    }

    if history.len() > 40 {
        let drop = history.len() - 40;
        history.drain(..drop);
    }
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), history);

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

        // 统一原生工具循环：内部走 chat_turn(tools)，delta / seal / tool chip / 块事件实时推给前端
        let mut new_history = history;
        new_history.push(ChatMessage::user(input.clone()));
        run_agentic_loop(
            &st,
            &st.ctx,
            &st.tool_runtime,
            &sec,
            &session_key,
            &mut new_history,
            Some(tx.clone()),
        )
        .await;

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

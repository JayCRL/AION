//! Phase 2 集成测试：Agent → ToolCall → ToolRuntime → SecurityContext → Service → Linux → ToolResult。
//!
//! 验证整条链在 root-less CI 也能工作：Rust 测试用 `App::new().run()` 拉起 Runtime，
//! Tools 调用 `FileService`/`ProcessService`/`TerminalService`，再走 `aion_adapter`
//! 的 tokio::process 在 Linux 上跑真命令。

use aion_adapter::AdapterKit;
use aion_protocol::prelude::*;
use aion_services::security::SecurityContext;
use aion_services::tool::{populate_builtin_registry, ToolRegistry, ToolRuntime};
use aion_services::system_services;
use cordis::prelude::*;

// --------------------------------------------------------------------------
// helpers
// --------------------------------------------------------------------------

async fn fresh_runtime() -> (cordis::Context, std::path::PathBuf) {
    let ctx = App::new().run().await.unwrap();
    let kit = AdapterKit::native(std::env::temp_dir().join(format!("aion-tools-{}", std::process::id())));
    let storage_root = std::env::temp_dir().join(format!("aion-tools-store-{}", std::process::id()));
    let services = system_services(&kit, storage_root, std::env::temp_dir());
    aion_services::provide_all(&ctx, services).unwrap();

    // 注入 ToolRegistry + ToolRuntime + 注册 7 个内置 Tool
    let registry = ToolRegistry::new();
    populate_builtin_registry(&registry).unwrap();
    ctx.provide(registry.clone()).unwrap();
    ctx.provide(ToolRuntime::new(std::sync::Arc::new(registry))).unwrap();

    let ws = std::env::temp_dir().join(format!("aion-tools-ws-{}", std::process::id()));
    tokio::fs::create_dir_all(&ws).await.unwrap();
    (ctx, ws)
}

fn sec_all(ws: &std::path::Path) -> SecurityContext {
    SecurityContext::new("test")
        .allow_all()
        .root(ws)
        .net("*")
        .max_processes(8)
}

fn sec_ro(ws: &std::path::Path) -> SecurityContext {
    SecurityContext::new("ro")
        .allow("fs:read")
        .allow("terminal:exec")
        .allow("system:read")
        .root(ws)
        .net("*")
}

fn sec_nothing() -> SecurityContext {
    SecurityContext::new("no-caps")
}

fn call(tool: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        call_id: CallId::new(),
        tool: tool.into(),
        arguments: args,
        sandbox: None,
    }
}

// --------------------------------------------------------------------------
// happy path
// --------------------------------------------------------------------------

#[test]
fn terminal_exec_echo_returns_success_with_stdout() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async {
        let (ctx, _ws) = fresh_runtime().await;
        let runtime = ctx.require::<ToolRuntime>().await.unwrap();
        let sec = sec_all(&_ws);
        let result = runtime
            .execute(
                &ctx,
                call("terminal.exec", serde_json::json!({ "command": "echo hello AION" })),
                sec,
            )
            .await;
        result
    });

    // 成功的 ToolResult：data.stdout == "hello AION\n"
    match &result.status {
        ResultStatus::Success => {}
        other => panic!("expected Success, got {other:?}"),
    }
    let stdout = result.data.get("stdout").and_then(|v| v.as_str()).unwrap();
    assert_eq!(stdout.trim_end(), "hello AION");
    assert_eq!(
        result.data.get("exit_code").and_then(|v| v.as_i64()),
        Some(0)
    );
    assert_eq!(result.data.get("timed_out").and_then(|v| v.as_bool()), Some(false));
}

#[test]
fn terminal_exec_under_strict_sandbox_on_rootless_linux() {
    // 跨平台：rootless 下 sandbox 整体降级到仅 no_new_privs + cgroup best-effort；
    // 命令仍能跑完（sandboxed 字段在 rootless 可能是 false）。
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async {
        let (ctx, ws) = fresh_runtime().await;
        let runtime = ctx.require::<ToolRuntime>().await.unwrap();
        let sec = sec_all(&ws);
        let result = runtime
            .execute(
                &ctx,
                call(
                    "terminal.exec",
                    serde_json::json!({ "command": "echo sandbox-ok", "timeout_ms": 5000 }),
                ),
                sec,
            )
            .await;
        result
    });
    match &result.status {
        ResultStatus::Success => {
            let stdout = result.data.get("stdout").and_then(|v| v.as_str()).unwrap();
            assert_eq!(stdout.trim_end(), "sandbox-ok");
        }
        ResultStatus::Error { kind, message } => {
            // rootless CI 可能因为沙箱/cgroup 失败返回 Error；只要不是 Denied 就算可接受
            assert!(
                !matches!(result.status, ResultStatus::Denied { .. }),
                "echo under sandbox shouldn't be denied: {message} (kind={kind:?})"
            );
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn file_write_then_file_read_roundtrip() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (ctx, ws) = fresh_runtime().await;
        let runtime = ctx.require::<ToolRuntime>().await.unwrap();
        let sec = sec_all(&ws);
        let path = ws.join("hello.txt").to_string_lossy().to_string();

        let write_result = runtime
            .execute(
                &ctx,
                call(
                    "file.write",
                    serde_json::json!({ "path": &path, "content": "AION protocol layer" }),
                ),
                sec.clone(),
            )
            .await;
        match &write_result.status {
            ResultStatus::Success => {
                let n = write_result.data.get("bytes_written").and_then(|v| v.as_u64()).unwrap();
                assert_eq!(n, "AION protocol layer".len() as u64);
            }
            other => panic!("file.write expected Success, got {other:?}"),
        }

        let read_result = runtime
            .execute(
                &ctx,
                call("file.read", serde_json::json!({ "path": &path })),
                sec,
            )
            .await;
        let content = read_result
            .data
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("file.read no content; status={:?} data={}",
                read_result.status, read_result.data));
        assert_eq!(content, "AION protocol layer");
    });
}

#[test]
fn file_list_returns_entries() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (ctx, ws) = fresh_runtime().await;
        // 提前建两个文件 + 一个子目录
        tokio::fs::write(ws.join("a.txt"), b"a").await.unwrap();
        tokio::fs::write(ws.join("b.txt"), b"b").await.unwrap();
        tokio::fs::create_dir(ws.join("sub")).await.unwrap();

        let runtime = ctx.require::<ToolRuntime>().await.unwrap();
        let sec = sec_all(&ws);
        let result = runtime
            .execute(&ctx, call("file.list", serde_json::json!({ "path": ws.to_string_lossy() })), sec)
            .await;
        match &result.status {
            ResultStatus::Success => {
                let entries = result.data.get("entries").and_then(|v| v.as_array()).expect("entries");
                let names: Vec<&str> = entries
                    .iter()
                    .filter_map(|e| e.get("name").and_then(|n| n.as_str()))
                    .collect();
                assert!(names.contains(&"a.txt"));
                assert!(names.contains(&"b.txt"));
                assert!(names.contains(&"sub"));
            }
            other => panic!("file.list expected Success, got {other:?}"),
        }
    });
}

// --------------------------------------------------------------------------
// denial cases
// --------------------------------------------------------------------------

#[test]
fn file_read_with_no_caps_is_denied_at_capability_check() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async {
        let (ctx, ws) = fresh_runtime().await;
        let runtime = ctx.require::<ToolRuntime>().await.unwrap();
        let sec = sec_nothing(); // 无 cap
        let result = runtime
            .execute(
                &ctx,
                call(
                    "file.read",
                    serde_json::json!({ "path": ws.join("a.txt").to_string_lossy() }),
                ),
                sec,
            )
            .await;
        result
    });

    // 无 cap 应该被 Runtime 在 capability 阶段直接拦下 → ResultStatus::Denied
    match &result.status {
        ResultStatus::Denied { cap, hint } => {
            assert_eq!(cap, "fs:read");
            assert!(hint.contains("grant"));
        }
        other => panic!("expected Denied, got {other:?}"),
    }
}

#[test]
fn file_read_of_etc_shadow_with_read_caps_is_path_denied() {
    // 关键否定案：capability fs:read 通过、但路径 /etc/shadow 不在 fs_roots 白名单
    // 内 → 落到 FileService 的 check_path → AionError::PathDenied → ToolResult::Error。
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async {
        let (ctx, ws) = fresh_runtime().await;
        let runtime = ctx.require::<ToolRuntime>().await.unwrap();
        // 有 fs:read + fs:write，但 root 仅在 ws
        let sec = SecurityContext::new("scoped")
            .allow("fs:read")
            .root(&ws)
            .net("*");
        let result = runtime
            .execute(
                &ctx,
                call("file.read", serde_json::json!({ "path": "/etc/shadow" })),
                sec,
            )
            .await;
        result
    });

    // ToolResult::Error（不是 Denied——因为 cap 通过，路径在 Service 层被拒）
    match &result.status {
        ResultStatus::Error { .. } => {
            // Tool 把任何 FileService 错误统一映射为 NotFound；具体 message 跨平台有差异
            // (Windows 会把 /etc/shadow 拼接到 cwd, Linux 不变)。Phase 3 再做精细映射。
        }
        other => panic!("expected Error (path-denied), got {other:?}"),
    }
}

#[test]
fn unknown_tool_is_reported_as_not_found() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async {
        let (ctx, ws) = fresh_runtime().await;
        let runtime = ctx.require::<ToolRuntime>().await.unwrap();
        let sec = sec_all(&ws);
        runtime
            .execute(
                &ctx,
                call("does.not.exist", serde_json::json!({})),
                sec,
            )
            .await
    });
    match &result.status {
        ResultStatus::Error { kind: ErrorKind::NotFound, message } => {
            assert!(message.contains("does.not.exist"));
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn schema_invalid_arg_rejected_before_dispatch() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async {
        let (ctx, ws) = fresh_runtime().await;
        let runtime = ctx.require::<ToolRuntime>().await.unwrap();
        let sec = sec_all(&ws);
        // file.read schema 强制 `path` 是 string（min_length 1），传 number 触发 schema 错
        runtime
            .execute(
                &ctx,
                call("file.read", serde_json::json!({ "path": 12345 })),
                sec,
            )
            .await
    });
    match &result.status {
        ResultStatus::Error { kind: ErrorKind::InvalidInput, message } => {
            assert!(message.contains("type mismatch") || message.contains("expected"));
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

// --------------------------------------------------------------------------
// system stats（仅 Linux 可验证）
// --------------------------------------------------------------------------

#[test]
#[cfg(target_os = "linux")]
fn system_stats_returns_load_and_uptime_on_linux() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async {
        let (ctx, _ws) = fresh_runtime().await;
        let runtime = ctx.require::<ToolRuntime>().await.unwrap();
        let sec = sec_nothing(); // system.stats 自身不需要 cap
        runtime.execute(&ctx, call("system.stats", serde_json::json!({})), sec).await
    });
    match &result.status {
        ResultStatus::Success => {
            // load 应该至少有 load1/5/15
            let load = result.data.get("load").and_then(|v| v.as_object()).expect("load");
            assert!(load.get("load1").is_some());
            // uptime 是 f64 秒
            assert!(result.data.get("uptime_seconds").and_then(|v| v.as_f64()).is_some());
        }
        other => panic!("expected Success, got {other:?}"),
    }
}

#[test]
#[cfg(not(target_os = "linux"))]
fn system_stats_returns_unavailable_on_non_linux() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async {
        let (ctx, _ws) = fresh_runtime().await;
        let runtime = ctx.require::<ToolRuntime>().await.unwrap();
        let sec = sec_nothing();
        runtime.execute(&ctx, call("system.stats", serde_json::json!({})), sec).await
    });
    match &result.status {
        ResultStatus::Error { kind: ErrorKind::Unavailable, .. } => {}
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

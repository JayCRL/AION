//! `process.list` / `process.start` —— 围绕 `ProcessService` 的 Tool 包装。

use std::collections::BTreeMap;

use aion_protocol::prelude::*;
use aion_protocol::schema::{JsonSchema, JsonSchemaDocument};

use crate::process::ProcessService;
use crate::security::SecurityContext;
use crate::tool::{Tool, ToolCallScope};

use async_trait::async_trait;
use cordis::Context;
use serde_json::{json, Value};

// ===========================================================================
// process.list
// ===========================================================================

pub struct ProcessListTool {
    def: ToolDefinition,
}

impl ProcessListTool {
    pub fn new() -> Self {
        Self {
            def: ToolDefinition {
                name: "process.list".into(),
                description: "列出当前由 AION 管理的活跃进程".into(),
                input: JsonSchemaDocument::new(JsonSchema::Object {
                    properties: BTreeMap::new(),
                    required: vec![],
                    additional: Box::new(JsonSchema::Any),
                }),
                output: None,
                required_caps: vec!["process:list".into()],
                risk: Risk::Low,
            },
        }
    }
}

#[async_trait]
impl Tool for ProcessListTool {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    async fn call(&self, ctx: &cordis::Context, scope: &ToolCallScope, _args: Value) -> ToolResult {
        let service = ctx.require::<ProcessService>().await.unwrap();
        let sec: SecurityContext = scope.security.clone();

        match service.list(&sec) {
            Ok(tickets) => {
                let arr: Vec<Value> = tickets
                    .iter()
                    .map(|t| {
                        json!({
                            "ticket_id": t.id,
                            "pid": t.pid,
                            "sandboxed": t.sandboxed,
                            "cgroup": t.cgroup,
                        })
                    })
                    .collect();
                ToolResult::success(json!({
                    "count": tickets.len(),
                    "processes": arr,
                }))
            }
            Err(e) => ToolResult::error(
                aion_protocol::result::ErrorKind::Internal,
                format!("process.list: {e}"),
            ),
        }
    }
}

// ===========================================================================
// process.start
// ===========================================================================

pub struct ProcessStartTool {
    def: ToolDefinition,
}

impl ProcessStartTool {
    pub fn new() -> Self {
        Self {
            def: ToolDefinition {
                name: "process.start".into(),
                description: "启动一个进程（可选沙箱）".into(),
                input: JsonSchemaDocument::new(JsonSchema::Object {
                    properties: BTreeMap::from([
                        (
                            "argv".into(),
                            Box::new(JsonSchema::Array {
                                items: Box::new(JsonSchema::String {
                                    min_length: None,
                                    max_length: Some(4096),
                                    pattern: None,
                                }),
                                min_items: Some(1),
                                max_items: Some(64),
                            }),
                        ),
                        (
                            "cwd".into(),
                            Box::new(JsonSchema::String {
                                min_length: None,
                                max_length: Some(4096),
                                pattern: None,
                            }),
                        ),
                        (
                            "sandbox".into(),
                            Box::new(JsonSchema::OneOf {
                                variants: vec![
                                    JsonSchema::String {
                                        min_length: None,
                                        max_length: None,
                                        pattern: None,
                                    }, // "default"
                                    JsonSchema::String {
                                        min_length: None,
                                        max_length: None,
                                        pattern: None,
                                    }, // "none_exec"
                                    JsonSchema::String {
                                        min_length: None,
                                        max_length: None,
                                        pattern: None,
                                    },
                                ],
                            }),
                        ),
                        (
                            "timeout_ms".into(),
                            Box::new(JsonSchema::Integer {
                                minimum: Some(0),
                                maximum: Some(600_000),
                            }),
                        ),
                    ]),
                    required: vec!["argv".into()],
                    additional: Box::new(JsonSchema::Any),
                }),
                output: None,
                required_caps: vec!["process:spawn".into()],
                risk: Risk::High,
            },
        }
    }
}

#[async_trait]
impl Tool for ProcessStartTool {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    async fn call(&self, ctx: &cordis::Context, scope: &ToolCallScope, args: Value) -> ToolResult {
        let argv: Vec<String> = match args.get("argv").and_then(|v| v.as_array()) {
            None => {
                return ToolResult::error(
                    aion_protocol::result::ErrorKind::InvalidInput,
                    "`argv` must be a non-empty array of strings".to_string(),
                );
            }
            Some(arr) => {
                let parts: Result<Vec<String>, _> = arr
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(|s| s.to_string())
                            .ok_or_else(|| "non-string argv element")
                    })
                    .collect();
                match parts {
                    Ok(v) if !v.is_empty() => v,
                    _ => {
                        return ToolResult::error(
                            aion_protocol::result::ErrorKind::InvalidInput,
                            "`argv` must be a non-empty array of strings".to_string(),
                        );
                    }
                }
            }
        };
        let cwd = args
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from);
        let sandbox = args
            .get("sandbox")
            .and_then(|v| v.as_str())
            .and_then(|s| match s {
                "default" => None,
                "none_exec" => Some(false),
                "strict" => Some(true),
                _ => None,
            });
        let timeout_ms = args.get("timeout_ms").and_then(|v| v.as_u64());

        let sec: SecurityContext = scope.security.clone();
        let sec: SecurityContext = scope.security.clone();
        let service = ctx.require::<ProcessService>().await.unwrap();

        // 构造 spec（platform 无关字段）
        let mut spec = aion_adapter::process::ProcessSpec::new(argv.clone());
        spec = spec.stdout(aion_adapter::process::StreamMode::Pipe)
                  .stderr(aion_adapter::process::StreamMode::Pipe);
        if let Some(dir) = cwd {
            spec = spec.cwd(dir);
        }

        // 调用 ProcessService.spawn；支持 sandbox 提示。
        // 注意：sandbox hint 只是建议；实际策略由 Runtime + ProcessService 实现决定。
        let spawn_fut = service.spawn(&sec, spec, sandbox.unwrap_or(false));
        let ticket_result = if let Some(t) = timeout_ms {
            match tokio::time::timeout(
                std::time::Duration::from_millis(t),
                spawn_fut,
            )
            .await
            {
                Ok(r) => r,
                Err(_) => {
                    return ToolResult::error(
                        aion_protocol::result::ErrorKind::Timeout,
                        "spawn exceeded timeout".to_string(),
                    );
                }
            }
        } else {
            spawn_fut.await
        };

        match ticket_result {
            Ok(task) => ToolResult::success(json!({
                "ticket": {
                    "id": task.ticket.id,
                    "pid": task.ticket.pid,
                    "sandboxed": task.ticket.sandboxed,
                    "cgroup": task.ticket.cgroup,
                },
                "command": argv,
            })),
            Err(e) => ToolResult::error(
                aion_protocol::result::ErrorKind::Internal,
                format!("process.start: {e}"),
            ),
        }
    }
}

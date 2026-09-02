//! `terminal.exec` —— 直接走 `TerminalService::echo`（跨平台、简单）。

use std::collections::BTreeMap;
use std::time::Duration;

use aion_protocol::prelude::*;
use aion_protocol::schema::{JsonSchema, JsonSchemaDocument};

use crate::security::SecurityContext;
use crate::terminal::TerminalService;
use crate::tool::{Tool, ToolCallScope};

use async_trait::async_trait;
use cordis::Context;
use serde_json::{json, Value};

pub struct TerminalExecTool {
    def: ToolDefinition,
}

impl TerminalExecTool {
    pub fn new() -> Self {
        Self {
            def: ToolDefinition {
                name: "terminal.exec".into(),
                description: "通过系统 shell 执行一条命令并返回 stdout/stderr/exit code".into(),
                input: JsonSchemaDocument::new(JsonSchema::Object {
                    properties: BTreeMap::from([
                        (
                            "command".into(),
                            Box::new(JsonSchema::String {
                                min_length: Some(1),
                                max_length: Some(8192),
                                pattern: None,
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
                    required: vec!["command".into()],
                    additional: Box::new(JsonSchema::Any),
                }),
                output: None,
                required_caps: vec!["terminal:exec".into()],
                risk: Risk::High,
            },
        }
    }
}

#[async_trait]
impl Tool for TerminalExecTool {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    async fn call(&self, ctx: &cordis::Context, scope: &ToolCallScope, args: Value) -> ToolResult {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let command = match command {
            Some(c) if !c.is_empty() => c,
            _ => {
                return ToolResult::error(
                    aion_protocol::result::ErrorKind::InvalidInput,
                    "`command` must be a non-empty string".to_string(),
                );
            }
        };
        let timeout_ms = args.get("timeout_ms").and_then(|v| v.as_u64());

        let service = ctx.require::<TerminalService>().await.unwrap();
        let sec: SecurityContext = scope.security.clone();

        // 跨平台：把整条命令交给系统 shell 执行（sh -c / cmd /C）。
        // 不要用 `service.echo(&command)`——它会把 command 当作 echo 程序的参数。
        #[cfg(target_os = "windows")]
        let (program, args): (&str, Vec<String>) =
            ("cmd", vec!["/C".into(), command.clone()]);
        #[cfg(not(target_os = "windows"))]
        let (program, args): (&str, Vec<String>) =
            ("sh", vec!["-c".into(), command.clone()]);

        let exec_fut = service.run_command(&sec, program, &args, None, Duration::from_secs(15));
        let outcome = if let Some(t) = timeout_ms {
            match tokio::time::timeout(Duration::from_millis(t), exec_fut).await {
                Ok(r) => r,
                Err(_) => {
                    return ToolResult::error(
                        aion_protocol::result::ErrorKind::Timeout,
                        format!("terminal.exec exceeded {t}ms"),
                    );
                }
            }
        } else {
            exec_fut.await
        };

        match outcome {
            Ok(o) => ToolResult::success(json!({
                "command": command,
                "exit_code": o.code,
                "stdout": o.stdout,
                "stderr": o.stderr,
                "duration_ms": o.duration_ms as u64,
                "sandboxed": o.sandboxed,
                "timed_out": o.timed_out,
            })),
            Err(e) => ToolResult::error(
                aion_protocol::result::ErrorKind::Internal,
                format!("terminal.exec: {e}"),
            ),
        }
    }
}

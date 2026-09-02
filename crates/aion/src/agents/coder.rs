//! Coder Agent：在沙箱内写文件 / 跑命令 / 组合流水线。

use async_trait::async_trait;
use cordis::Context;
use std::time::Duration;

use aion_services::SecurityContext;
use aion_services::terminal::TerminalService;
use aion_services::fs::FileService;

use crate::agents::{shell_args, Agent, AgentTask};

pub struct CoderAgent;

const TIMEOUT: Duration = Duration::from_secs(30);

#[async_trait]
impl Agent for CoderAgent {
    fn name(&self) -> &'static str {
        "coder"
    }

    fn description(&self) -> &'static str {
        "Coder Agent — 沙箱内写代码 / 跑命令"
    }

    fn default_caps(&self) -> Vec<&'static str> {
        vec![
            "fs:read",
            "fs:write",
            "terminal:exec",
            "process:spawn",
            "process:list",
        ]
    }

    async fn handle(
        &self,
        ctx: &Context,
        sec: &SecurityContext,
        task: &AgentTask,
    ) -> anyhow::Result<String> {
        match task.kind.as_str() {
            "write" => {
                let file = ctx.require::<FileService>().await?;
                let path = task
                    .params
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("params.path required"))?;
                let content = task
                    .params
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&task.input);
                file.write(sec, path, content.as_bytes()).await?;
                Ok(format!("✓ 已写入 {path}（{} 字节）", content.len()))
            }
            "pipeline" => {
                let file = ctx.require::<FileService>().await?;
                let terminal = ctx.require::<TerminalService>().await?;
                let path = task
                    .params
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("main.txt");
                let code = task
                    .params
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&task.input);
                file.write(sec, path, code.as_bytes()).await?;
                let build_cmd = task
                    .params
                    .get("build")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("params.build required"))?;
                let (program, args) = shell_args(build_cmd);
                let out = terminal
                    .run_command(sec, program, &args, None, TIMEOUT)
                    .await?;
                Ok(format!(
                    "✓ 流水线完成\n  写入: {path}\n  构建: exit {}\n  输出: {}",
                    out.code,
                    out.stdout.trim()
                ))
            }
            // 默认：直接执行命令
            _ => {
                let terminal = ctx.require::<TerminalService>().await?;
                let (program, args) = shell_args(&task.input);
                let out = terminal
                    .run_command(sec, program, &args, None, TIMEOUT)
                    .await?;
                Ok(format!(
                    "exit={} | sandboxed={} | {}ms\n{}{}",
                    out.code,
                    out.sandboxed,
                    out.duration_ms,
                    out.stdout.trim_end(),
                    if out.stderr.trim().is_empty() {
                        String::new()
                    } else {
                        format!("\n[stderr] {}", out.stderr.trim_end())
                    }
                ))
            }
        }
    }
}

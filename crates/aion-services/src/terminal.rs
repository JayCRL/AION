//! TerminalService：终端 / IO。沙箱内执行命令，捕获输出并做超时控制。
//!
//! 依赖 ProcessService（通过 `Service::inject` 声明 DI 关系）。

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aion_adapter::process::{ProcessSpec, StreamMode};
use async_trait::async_trait;
use tokio::io::AsyncReadExt;

use crate::error::{AionError, AionResult};
use crate::process::ProcessService;
use crate::security::SecurityContext;

/// 命令执行结果。
#[derive(Debug, Clone)]
pub struct TerminalOutcome {
    pub id: u64,
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    /// 沙箱是否真实强制执行。
    pub sandboxed: bool,
    pub duration_ms: u128,
    pub timed_out: bool,
}

/// 终端 / IO 服务。
pub struct TerminalService {
    dep: Mutex<Option<std::sync::Arc<ProcessService>>>,
    ctx: Mutex<Option<cordis::Context>>,
}

impl TerminalService {
    pub fn new() -> Self {
        TerminalService {
            dep: Mutex::new(None),
            ctx: Mutex::new(None),
        }
    }

    async fn process(&self) -> AionResult<std::sync::Arc<ProcessService>> {
        let cached = self.dep.lock().expect("dep poisoned").clone();
        if let Some(p) = cached {
            return Ok(p);
        }
        let ctx = self
            .ctx
            .lock()
            .expect("ctx poisoned")
            .clone()
            .ok_or_else(|| {
                AionError::Unavailable("terminal".into(), "service not started".into())
            })?;
        let proc = ctx.require::<ProcessService>().await?;
        *self.dep.lock().expect("dep poisoned") = Some(proc.clone());
        Ok(proc)
    }

    /// 在沙箱内执行一条命令。
    pub async fn run_command(
        &self,
        sec: &SecurityContext,
        program: &str,
        args: &[String],
        cwd: Option<PathBuf>,
        timeout: Duration,
    ) -> AionResult<TerminalOutcome> {
        sec.check_cap("terminal:exec")?;
        let proc = self.process().await?;

        let mut spec = ProcessSpec::new(
            std::iter::once(program.to_string()).chain(args.iter().cloned()),
        )
        .stdout(StreamMode::Pipe)
        .stderr(StreamMode::Pipe);
        if let Some(dir) = cwd {
            spec = spec.cwd(dir);
        }

        let task = proc.spawn(sec, spec, true).await?;
        let id = task.ticket.id;
        let sandboxed = task.ticket.sandboxed;
        let wait_fut = task.wait();
        let SpawnedTaskParts { stdout, stderr } = SpawnedTaskParts {
            stdout: task.stdout,
            stderr: task.stderr,
        };

        let start = Instant::now();
        let run = async move {
            let mut out_buf = Vec::new();
            let mut err_buf = Vec::new();
            let f1 = async {
                if let Some(mut r) = stdout {
                    let _ = r.read_to_end(&mut out_buf).await;
                }
            };
            let f2 = async {
                if let Some(mut r) = stderr {
                    let _ = r.read_to_end(&mut err_buf).await;
                }
            };
            let (_, _) = tokio::join!(f1, f2);
            let code = wait_fut.await;
            (out_buf, err_buf, code)
        };

        match tokio::time::timeout(timeout, run).await {
            Ok((out_buf, err_buf, code)) => Ok(TerminalOutcome {
                id,
                code,
                stdout: String::from_utf8_lossy(&out_buf).into_owned(),
                stderr: String::from_utf8_lossy(&err_buf).into_owned(),
                sandboxed,
                duration_ms: start.elapsed().as_millis(),
                timed_out: false,
            }),
            Err(_) => {
                // 超时：终止进程
                let _ = proc.kill(sec, id, 9).await;
                Ok(TerminalOutcome {
                    id,
                    code: -1,
                    stdout: String::new(),
                    stderr: String::new(),
                    sandboxed,
                    duration_ms: start.elapsed().as_millis(),
                    timed_out: true,
                })
            }
        }
    }

    /// 便捷方法：跨平台执行 `echo`。
    pub async fn echo(&self, sec: &SecurityContext, text: &str) -> AionResult<TerminalOutcome> {
        #[cfg(target_os = "windows")]
        let (program, args): (&str, Vec<String>) =
            ("cmd", vec!["/C".into(), "echo".into(), text.to_string()]);
        #[cfg(not(target_os = "windows"))]
        let (program, args): (&str, Vec<String>) = ("echo", vec![text.to_string()]);
        self.run_command(sec, program, &args, None, Duration::from_secs(15))
            .await
    }
}

struct SpawnedTaskParts {
    stdout: Option<Box<dyn tokio::io::AsyncRead + Send + Unpin>>,
    stderr: Option<Box<dyn tokio::io::AsyncRead + Send + Unpin>>,
}

#[async_trait]
impl cordis::Service for TerminalService {
    fn name(&self) -> &'static str {
        "terminal"
    }

    fn description(&self) -> &'static str {
        "终端 / IO"
    }

    fn inject(&self) -> Vec<&'static str> {
        vec!["process"]
    }

    async fn start(&self, ctx: &cordis::Context) -> cordis::CordisResult<()> {
        *self.ctx.lock().expect("ctx poisoned") = Some(ctx.clone());
        // 预热依赖（展示 DI：terminal → process）
        let _ = self
            .process()
            .await
            .map_err(|e| cordis::CordisError::Custom(e.to_string()))?;
        ctx.info("TerminalService ready (deps: process)");
        Ok(())
    }
}

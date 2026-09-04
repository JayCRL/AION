//! Process 适配器：进程创建与控制（clone / execve / wait 语义封装）。
//!
//! Linux 上通过 `std::os::unix::process::CommandExt::pre_exec` 在 fork 后、
//! exec 前的窗口内依次应用沙箱：unshare → cgroup → no_new_privs → seccomp →
//! capability 收缩；其他平台使用 tokio 直接创建进程，沙箱不强制执行。

use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::process::Stdio;
use std::pin::Pin;

use async_trait::async_trait;
use tokio::io::AsyncRead;
use tokio::process::Command;

use crate::sandbox::SandboxProfile;
use crate::{AdapterError, AdapterResult};

/// 输出流模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamMode {
    Pipe,
    Inherit,
}

/// 进程启动规格。
#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// 是否清空继承的环境变量。
    pub clear_env: bool,
    pub stdout: StreamMode,
    pub stderr: StreamMode,
    /// 沙箱配置（Linux 上在 exec 前强制应用）。
    pub sandbox: Option<SandboxProfile>,
    /// 预先创建好的 cgroup 目录（配合 `SandboxProfile::cgroup` 使用）。
    pub cgroup_path: Option<PathBuf>,
}

impl ProcessSpec {
    /// 以命令行构造规格（默认管道捕获 stdout/stderr）。
    pub fn new<I, S>(argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        ProcessSpec {
            argv: argv.into_iter().map(Into::into).collect(),
            cwd: None,
            env: Vec::new(),
            clear_env: false,
            stdout: StreamMode::Pipe,
            stderr: StreamMode::Pipe,
            sandbox: None,
            cgroup_path: None,
        }
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn clear_env(mut self) -> Self {
        self.clear_env = true;
        self
    }

    pub fn stdout(mut self, mode: StreamMode) -> Self {
        self.stdout = mode;
        self
    }

    pub fn stderr(mut self, mode: StreamMode) -> Self {
        self.stderr = mode;
        self
    }

    pub fn sandbox(mut self, profile: SandboxProfile) -> Self {
        self.sandbox = Some(profile);
        self
    }

    pub fn cgroup_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.cgroup_path = Some(path.into());
        self
    }

    pub fn program(&self) -> &str {
        self.argv.first().map(String::as_str).unwrap_or("")
    }

    pub fn args(&self) -> &[String] {
        &self.argv[1.min(self.argv.len())..]
    }
}

/// 已创建的进程。
pub struct SpawnedProcess {
    pub pid: Option<u32>,
    /// 沙箱是否真实强制执行（仅 Linux 为 true）。
    pub sandboxed: bool,
    pub stdout: Option<Box<dyn AsyncRead + Send + Unpin>>,
    pub stderr: Option<Box<dyn AsyncRead + Send + Unpin>>,
    /// 等待进程退出，返回退出码（异常终止为 -1）。
    pub wait: crate::BoxFut<i32>,
}

/// Process 适配器 trait。
#[async_trait]
pub trait ProcessAdapter: Send + Sync {
    async fn spawn(&self, spec: ProcessSpec) -> AdapterResult<SpawnedProcess>;

    /// 向进程发送信号（Linux）/ 终止进程（其他平台）。
    async fn kill(&self, pid: u32, signal: i32) -> AdapterResult<()>;
}

/// 平台原生实现。
pub struct NativeProcessAdapter;

#[async_trait]
impl ProcessAdapter for NativeProcessAdapter {
    async fn spawn(&self, spec: ProcessSpec) -> AdapterResult<SpawnedProcess> {
        if spec.argv.is_empty() {
            return Err(AdapterError::Other("argv is empty".into()));
        }
        let mut cmd = Command::new(&spec.argv[0]);
        cmd.args(&spec.argv[1..]);
        cmd.stdin(Stdio::null());
        cmd.stdout(match spec.stdout {
            StreamMode::Pipe => Stdio::piped(),
            StreamMode::Inherit => Stdio::inherit(),
        });
        cmd.stderr(match spec.stderr {
            StreamMode::Pipe => Stdio::piped(),
            StreamMode::Inherit => Stdio::inherit(),
        });
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
        }
        if spec.clear_env {
            cmd.env_clear();
        }
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        // Linux：fork 后 exec 前应用沙箱；其他平台无法强制执行
        let sandbox_enforced = spec.sandbox.is_some() && cfg!(target_os = "linux");
        #[cfg(target_os = "linux")]
        if let Some(profile) = spec.sandbox.clone() {
            let cgroup_path = spec.cgroup_path.clone();
            unsafe {
                cmd.pre_exec(move || {
                    apply_sandbox_pre_exec(&profile, cgroup_path.as_deref())
                });
            }
        }

        let mut child = cmd.spawn()?;
        let pid = child.id();
        let stdout = child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn AsyncRead + Send + Unpin>);
        let stderr = child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn AsyncRead + Send + Unpin>);

        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let code = match child.wait().await {
                Ok(status) => status.code().unwrap_or(-1),
                Err(_) => -1,
            };
            let _ = tx.send(code);
        });
        let wait: Pin<Box<dyn std::future::Future<Output = i32> + Send>> =
            Box::pin(async move { rx.await.unwrap_or(-1) });

        Ok(SpawnedProcess {
            pid,
            sandboxed: sandbox_enforced,
            stdout,
            stderr,
            wait,
        })
    }

    async fn kill(&self, pid: u32, signal: i32) -> AdapterResult<()> {
        #[cfg(unix)]
        {
            // SAFETY: kill 是标准信号接口；信号合法性由调用方保证。
            let rc = unsafe { libc::kill(pid as libc::pid_t, signal) };
            if rc != 0 {
                return Err(AdapterError::Io(std::io::Error::last_os_error()));
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = signal;
            let out = Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .output()
                .await?;
            if out.status.success() {
                Ok(())
            } else {
                Err(AdapterError::Other(format!(
                    "taskkill failed for pid {pid}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )))
            }
        }
    }
}

/// fork 后、exec 前的沙箱应用顺序。
#[cfg(target_os = "linux")]
fn apply_sandbox_pre_exec(
    profile: &SandboxProfile,
    cgroup_path: Option<&Path>,
) -> std::io::Result<()> {
    apply_sandbox_pre_exec_impl(profile, cgroup_path)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
}

#[cfg(target_os = "linux")]
fn apply_sandbox_pre_exec_impl(
    profile: &SandboxProfile,
    cgroup_path: Option<&Path>,
) -> crate::AdapterResult<()> {
    // 0. 恢复 SIGPIPE 为默认处置。Rust 运行时对自身忽略 SIGPIPE，子进程经 exec
    //    继承“忽略”，于是 `ps aux | head -6` 这类管道里，head 退出后生产者继续
    //    写一个已无读者的管道只会得到 EPIPE 而不会死掉，进而无限阻塞到超时。
    //    这里在 fork 后、exec 前恢复默认（终止进程），让管道行为符合 shell 语义。
    // SAFETY: signal(SIGPIPE, SIG_DFL) 是常规信号配置，进程单线程，无竞态。
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // 1. namespace 隔离
    crate::namespace::unshare_raw(profile.namespaces.flags())?;

    // 2. 附加到预创建的 cgroup（写入 "0" 表示当前进程）
    if let Some(path) = cgroup_path {
        std::fs::write(path.join("cgroup.procs"), b"0").map_err(|e| {
            crate::AdapterError::Other(format!("attach cgroup {}: {e}", path.display()))
        })?;
    }

    // 3. no_new_privs（seccomp 的前置条件，同时阻止通过 exec 提权）
    if profile.no_new_privs {
        // SAFETY: prctl 常规调用。
        let rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        if rc != 0 {
            return Err(AdapterError::Io(std::io::Error::last_os_error()));
        }
    }

    // 4. seccomp 系统调用过滤
    if let Some(policy) = &profile.seccomp {
        crate::seccomp::install(policy)?;
    }

    // 5. capability 收缩（最小权限）
    crate::capability::restrict_bounding(profile.capabilities)?;

    Ok(())
}

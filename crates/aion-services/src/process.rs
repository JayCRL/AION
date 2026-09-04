//! ProcessService：进程管理。沙箱启动、退出事件、信号控制。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use aion_adapter::cgroup::CgroupHandle;
use aion_adapter::process::{ProcessSpec, SpawnedProcess};
use aion_adapter::{AdapterKit, BoxFut};
use async_trait::async_trait;
use cordis::event::Payload;
use tokio::io::AsyncRead;

use crate::error::{AionError, AionResult};
use crate::security::SecurityContext;
use crate::sandbox;

/// 进程退出事件负载。
#[derive(Debug, Clone)]
pub struct ProcessExitEvent {
    pub id: u64,
    pub pid: Option<u32>,
    pub code: i32,
}

/// 进程凭据（对上层暴露的句柄）。
#[derive(Debug, Clone)]
pub struct ProcessTicket {
    pub id: u64,
    pub pid: Option<u32>,
    pub sandboxed: bool,
    pub cgroup: Option<String>,
}

/// 一次进程启动的结果：凭据 + 输出流 + 等待退出码。
pub struct SpawnedTask {
    pub ticket: ProcessTicket,
    pub stdout: Option<Box<dyn AsyncRead + Send + Unpin>>,
    pub stderr: Option<Box<dyn AsyncRead + Send + Unpin>>,
    /// watch 通道：值为 None 表示仍在运行，进程退出后写入 Some(code)。
    /// watch 保留最近值，晚订阅的调用方也能立刻读到退出码（broadcast 会丢）。
    exit_tx: tokio::sync::watch::Sender<Option<i32>>,
}

impl SpawnedTask {
    /// 等待退出码（多播：可多次调用）。
    pub fn wait(&self) -> BoxFut<i32> {
        let mut rx = self.exit_tx.subscribe();
        Box::pin(async move {
            loop {
                if let Some(code) = *rx.borrow() {
                    return code;
                }
                if rx.changed().await.is_err() {
                    return -1;
                }
            }
        })
    }
}

struct LiveEntry {
    pid: Option<u32>,
    cgroup: Option<CgroupHandle>,
}

struct Inner {
    kit: AdapterKit,
    live: Mutex<HashMap<u64, LiveEntry>>,
    next_id: AtomicU64,
    handle: Mutex<Option<cordis::Context>>,
}

/// 进程管理服务。
#[derive(Clone)]
pub struct ProcessService {
    inner: Arc<Inner>,
}

impl ProcessService {
    pub fn new(kit: AdapterKit, _cgroup_root: PathBuf) -> Self {
        ProcessService {
            inner: Arc::new(Inner {
                kit,
                live: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                handle: Mutex::new(None),
            }),
        }
    }

    /// 发出沙箱能力降级警告事件（可观测性）。
    async fn warn_event(&self, message: String) {
        let ctx_opt = self.inner.handle.lock().expect("handle poisoned").clone();
        if let Some(ctx) = ctx_opt {
            let payload: Payload = Arc::new(message);
            let _ = ctx.emit_with("aion:sandbox:unenforced", payload).await;
        }
    }

    /// 启动进程；`sandbox` 为 true 时应用沙箱档案。
    pub async fn spawn(
        &self,
        sec: &SecurityContext,
        spec: ProcessSpec,
        sandbox_enabled: bool,
    ) -> AionResult<SpawnedTask> {
        sec.check_cap("process:spawn")?;

        {
            let live = self.inner.live.lock().expect("live map poisoned");
            if sec.max_processes > 0 && live.len() as u32 >= sec.max_processes {
                return Err(AionError::Limit(format!(
                    "process limit reached ({})",
                    sec.max_processes
                )));
            }
        }

        let mut spec = spec;
        let mut cgroup_handle: Option<CgroupHandle> = None;

        if sandbox_enabled {
            let mut profile = sandbox::build_profile();
            // 完整沙箱（namespace + seccomp + capability 收缩）需要 root：非 user
            // namespace 的 unshare 需要 CAP_SYS_ADMIN。无特权环境优雅降级 ——
            // 仅保留 no_new_privs + cgroup 尽力而为，避免 allowlist 兼容性问题。
            let full_sandbox = self.inner.kit.namespace.supported()
                && aion_adapter::namespace::can_create_namespaces();
            if !full_sandbox {
                profile.namespaces = aion_adapter::NamespaceSet::default();
                profile.seccomp = None;
                profile.capabilities = aion_adapter::CapabilitySet::all();
            }
            // cgroup: 尽力而为；失败（如未挂载/无权限）不阻塞启动，发警告事件
            if profile.cgroup.is_some() || !self.inner.kit.cgroup.is_emulated() {
                let id = self.inner.next_id.load(Ordering::Relaxed);
                let name = format!("task-{id}");
                let default_limits = aion_adapter::CgroupLimits::new();
                let limits = profile.cgroup.as_ref().unwrap_or(&default_limits);
                match self.inner.kit.cgroup.create(&name, limits).await {
                    Ok(h) => {
                        if !h.emulated {
                            spec.cgroup_path = Some(h.path.clone());
                        }
                        cgroup_handle = Some(h);
                    }
                    Err(e) => {
                        self.warn_event(format!(
                            "cgroup create failed, resource limits disabled: {e}"
                        ))
                        .await;
                        cgroup_handle = None;
                    }
                }
            }
            spec.sandbox = Some(profile);
        }

        let spawned = self.inner.kit.process.spawn(spec).await?;
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let SpawnedProcess {
            pid,
            sandboxed,
            stdout,
            stderr,
            wait,
        } = spawned;
        self.inner
            .live
            .lock()
            .expect("live map poisoned")
            .insert(id, LiveEntry { pid, cgroup: cgroup_handle.clone() });

        // watch 初始 None（运行中）；进程退出后 send_replace(Some(code)) 保留最近值。
        let (exit_tx, _) = tokio::sync::watch::channel(None);
        let ticket = ProcessTicket {
            id,
            pid,
            sandboxed,
            cgroup: cgroup_handle.as_ref().map(|c| c.name.clone()),
        };

        // 退出监视 Fiber：等待退出 → 记录退出码 → 清理 cgroup → 发事件
        if let Some(ctx) = self.inner.handle.lock().expect("handle poisoned").clone() {
            let inner = self.inner.clone();
            let exit_tx2 = exit_tx.clone();
            ctx.spawn(format!("process:watch:{id}"), async move {
                let code = wait.await;
                let _ = exit_tx2.send_replace(Some(code));
                if let Some(cg) = cgroup_handle {
                    let _ = inner.kit.cgroup.destroy(&cg).await;
                }
                inner.live.lock().expect("live map poisoned").remove(&id);
                let ctx_opt = inner.handle.lock().expect("handle poisoned").clone();
                if let Some(ctx) = ctx_opt {
                    let payload: Payload = Arc::new(ProcessExitEvent { id, pid, code });
                    let _ = ctx.emit_with("aion:process:exit", payload).await;
                }
                code
            });
        }

        Ok(SpawnedTask {
            ticket,
            stdout,
            stderr,
            exit_tx,
        })
    }

    /// 终止进程。
    pub async fn kill(&self, sec: &SecurityContext, id: u64, signal: i32) -> AionResult<()> {
        sec.check_cap("process:spawn")?;
        let pid = {
            let live = self.inner.live.lock().expect("live map poisoned");
            live.get(&id).and_then(|e| e.pid)
        };
        match pid {
            Some(pid) => Ok(self.inner.kit.process.kill(pid, signal).await?),
            None => Err(AionError::Other(format!("process {id} not found"))),
        }
    }

    /// 当前存活进程列表。
    pub fn list(&self, sec: &SecurityContext) -> AionResult<Vec<ProcessTicket>> {
        sec.check_cap("process:list")?;
        let live = self.inner.live.lock().expect("live map poisoned");
        Ok(live
            .iter()
            .map(|(id, e)| ProcessTicket {
                id: *id,
                pid: e.pid,
                sandboxed: self.inner.kit.namespace.supported(),
                cgroup: e.cgroup.as_ref().map(|c| c.name.clone()),
            })
            .collect())
    }

    /// 存活进程数。
    pub fn live_count(&self) -> usize {
        self.inner.live.lock().expect("live map poisoned").len()
    }
}

#[async_trait]
impl cordis::Service for ProcessService {
    fn name(&self) -> &'static str {
        "process"
    }

    fn description(&self) -> &'static str {
        "进程管理"
    }

    async fn start(&self, ctx: &cordis::Context) -> cordis::CordisResult<()> {
        *self.inner.handle.lock().expect("handle poisoned") = Some(ctx.clone());
        ctx.info("ProcessService ready (sandbox enforcement depends on platform)");
        Ok(())
    }

    async fn stop(&self, ctx: &cordis::Context) -> cordis::CordisResult<()> {
        // 尽力终止残留进程并清理 cgroup
        let entries: Vec<(u64, Option<u32>, Option<CgroupHandle>)> = {
            let mut live = self.inner.live.lock().expect("live map poisoned");
            live.drain()
                .map(|(id, e)| (id, e.pid, e.cgroup))
                .collect()
        };
        for (id, pid, cg) in entries {
            if let Some(pid) = pid {
                let _ = self.inner.kit.process.kill(pid, 9).await;
            }
            if let Some(cg) = cg {
                let _ = self.inner.kit.cgroup.destroy(&cg).await;
            }
            ctx.debug(format!("process {id} cleaned up"));
        }
        ctx.info("ProcessService stopped");
        Ok(())
    }
}

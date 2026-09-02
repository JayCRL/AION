//! 「调用流程示例」完整演示：
//! Agent 发起请求 → Cordis Context 获取 Service → AION Service 权限检查 →
//! Linux Adapter 执行系统调用 → Linux Kernel 执行操作 → 资源返回（事件 / Effect）。

use std::path::PathBuf;

use aion_services::device::DeviceService;
use aion_services::fs::FileService;
use aion_services::model::{ChatMessage, ModelService};
use aion_services::sandbox::SandboxService;
use aion_services::security::SecurityContext;
use aion_services::terminal::TerminalService;
use aion_services::{AionError, SandboxRequest};
use cordis::event::Event;
use tokio::sync::broadcast;

use crate::boot;

const LOGO: &str = r#"
    ▄▄▄
   ██████    AION — Agent OS Runtime
  ██  ████   Cordis-RS · System Services · Linux Adapter
   ▀████▀
"#;

fn stage(n: usize, title: &str, detail: &str) {
    println!("\n\x1b[1;36m[{n}/6]\x1b[0m \x1b[1m{title}\x1b[0m");
    if !detail.is_empty() {
        println!("     {detail}");
    }
}

fn ok(msg: &str) {
    println!("     \x1b[32m✓\x1b[0m {msg}");
}

fn fail(msg: &str) {
    println!("     \x1b[31m✗\x1b[0m {msg}");
}

fn drain(monitor: &mut broadcast::Receiver<Event>) -> Vec<Event> {
    let mut out = Vec::new();
    while let Ok(ev) = monitor.try_recv() {
        out.push(ev);
    }
    out
}

pub async fn run() -> anyhow::Result<()> {
    println!("{}", LOGO);
    println!("演示平台: {}（Linux 上为完整沙箱；其他平台为宿主模拟）\n", std::env::consts::OS);

    // ── 启动运行时 ──────────────────────────────────────────────
    let ctx = boot::boot(boot::load_config()).await?;
    let mut monitor = ctx.monitor();

    // 演示工作区
    let ws: PathBuf = std::env::temp_dir().join("aion-demo-ws");
    std::fs::create_dir_all(&ws)?;

    // Agent 安全上下文（最小权限示例）
    let sec = SecurityContext::new("coder-agent")
        .allow_list(&[
            "fs:read",
            "fs:write",
            "terminal:exec",
            "process:spawn",
            "process:list",
            "model:use",
            "device:list",
            "sandbox:create",
            "sandbox:inspect",
            "storage:allocate",
            "storage:write",
            "storage:use",
        ])
        .root(&ws)
        .net("*")
        .max_processes(8);
    // 受限 Agent：只有 fs:read
    let restricted = SecurityContext::new("restricted-agent").allow("fs:read").root(&ws);

    // ── 1. Agent 发起请求 ──────────────────────────────────────
    stage(1, "Agent 发起请求", "CoderAgent 请求：在沙箱内执行 `echo hello AION`");

    // ── 2. Cordis Context 获取 Service ─────────────────────────
    stage(
        2,
        "Cordis Context 获取 Service",
        "ctx.require::<TerminalService>() —— 惰性启动 + 依赖注入（terminal → process）",
    );
    let terminal = ctx.require::<TerminalService>().await?;
    let _process = ctx.require::<aion_services::process::ProcessService>().await?;
    for svc in ctx.list_services() {
        println!(
            "     · {:<10} {:<16} deps={:?} state={}",
            svc.name, svc.type_name, svc.deps, svc.state.as_str()
        );
    }

    // ── 3. AION Service 权限检查 ───────────────────────────────
    stage(3, "AION Service 权限检查", "Capability / 路径 / 网络白名单");
    match sec.check_cap("terminal:exec") {
        Ok(()) => ok("coder-agent 持有 terminal:exec —— 放行"),
        Err(e) => fail(&e.to_string()),
    }
    match restricted.check_cap("terminal:exec") {
        Ok(()) => fail("restricted-agent 不应通过"),
        Err(e) => fail(&format!("restricted-agent 缺少 terminal:exec —— 已拒绝（{e}）")),
    }
    match sec.check_path(&ws.join("ok.txt"), true) {
        Ok(p) => ok(&format!("路径 {} 在白名单内", p.display())),
        Err(e) => fail(&e.to_string()),
    }
    let outside = if cfg!(target_os = "windows") {
        PathBuf::from("C:\\Windows\\System32\\config\\SAM")
    } else {
        PathBuf::from("/etc/shadow")
    };
    match sec.check_path(&outside, false) {
        Ok(_) => fail("白名单外路径不应通过"),
        Err(e) => fail(&format!("{} 已拒绝（{e}）", outside.display())),
    }

    // ── 4. Linux Adapter 执行系统调用 ───────────────────────────
    stage(
        4,
        "Linux Adapter 执行系统调用",
        "sandbox → unshare(namespaces) + cgroup v2 + seccomp-BPF + capability 收缩",
    );
    let sandbox = ctx.require::<SandboxService>().await?;
    let profile = sandbox
        .create_profile(&sec, &SandboxRequest::default())
        .await?;
    let support = sandbox.inspect(&sec).await?;
    println!(
        "     · 平台沙箱能力: {}",
        support.summary()
    );
    println!(
        "     · 本档案: namespaces={:?} cgroup={:?} seccomp 白名单={} 条 caps={:?} no_new_privs={}",
        profile.namespaces.names(),
        profile.cgroup.as_ref().map(|c| (c.memory_max_bytes, c.pids_max)),
        profile.seccomp.as_ref().map(|p| p.allow.len()).unwrap_or(0),
        profile.capabilities.to_names(),
        profile.no_new_privs
    );
    let outcome = terminal.echo(&sec, "hello AION").await?;
    println!(
        "     · spawn 完成: task-id={} sandboxed={}",
        outcome.id, outcome.sandboxed
    );

    // ── 5. Linux Kernel 执行操作 ────────────────────────────────
    stage(5, "Linux Kernel 执行操作", "进程执行完毕，内核返回退出码");
    ok(&format!(
        "exit code = {}（{}ms）stdout: {}",
        outcome.code, outcome.duration_ms, outcome.stdout.trim()
    ));

    // ── 6. 资源返回（事件 / Effect） ─────────────────────────────
    stage(6, "资源返回（事件 / Effect）", "退出事件 + cgroup 清理 + Fiber 回收");
    // 给监视 Fiber 一点时间完成清理
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let events = drain(&mut monitor);
    let mut shown = 0;
    for ev in &events {
        if ev.name.starts_with("aion:process:exit") {
            if let Some(exit) = ev.payload::<aion_services::process::ProcessExitEvent>() {
                ok(&format!(
                    "事件 aion:process:exit: id={} pid={:?} code={}",
                    exit.id, exit.pid, exit.code
                ));
                shown += 1;
            }
        }
    }
    if shown == 0 {
        ok(&format!("事件流（共 {} 条，含 lifecycle/service 事件）已回放", events.len()));
    }

    // ── 更多服务速览 ────────────────────────────────────────────
    println!("\n\x1b[1m── 更多服务演示 ──\x1b[0m");

    // FileService
    let file = ctx.require::<FileService>().await?;
    let demo_file = ws.join("demo.txt");
    file.write(&sec, &demo_file, b"AION file service".as_slice()).await?;
    let content = file.read(&sec, &demo_file).await?;
    ok(&format!(
        "FileService: 写读 {} → {:?}",
        demo_file.display(),
        String::from_utf8_lossy(&content)
    ));

    // ModelService
    let model = ctx.require::<ModelService>().await?;
    let reply = model
        .chat(
            &sec,
            None,
            &[
                ChatMessage::system("你是 AION 上的助手"),
                ChatMessage::user("用一句话介绍 AION"),
            ],
        )
        .await?;
    ok(&format!("ModelService: {}", reply.replace('\n', " / ")));

    // DeviceService
    let device = ctx.require::<DeviceService>().await?;
    let devices = device.list(&sec, None).await.unwrap_or_default();
    let sample: Vec<String> = devices
        .iter()
        .take(3)
        .map(|d| d.name.clone())
        .collect();
    ok(&format!(
        "DeviceService: 发现 {} 个设备节点{}",
        devices.len(),
        if sample.is_empty() {
            String::new()
        } else {
            format!("（如 {}）", sample.join(", "))
        }
    ));

    // ── 汇总 & 优雅关闭 ─────────────────────────────────────────
    println!("\n\x1b[1m── 服务清单 ──\x1b[0m");
    for svc in ctx.list_services() {
        println!(
            "  \x1b[32m●\x1b[0m {:<10} \x1b[90m{}\x1b[0m",
            svc.name, svc.description_of()
        );
    }

    ctx.dispose().await?;
    println!("\n\x1b[1m运行时已优雅关闭：子作用域级联销毁 → 监听器移除 → 服务停止 → Fiber 回收 → Effect 逆序执行。\x1b[0m");
    Ok(())
}

/// ServiceInfo 补充描述（type_name 的简短形式）。
trait ServiceInfoExt {
    fn description_of(&self) -> String;
}

impl ServiceInfoExt for cordis::service::ServiceInfo {
    fn description_of(&self) -> String {
        // type_name 形如 "aion_services::terminal::TerminalService"
        self.type_name
            .rsplit("::")
            .next()
            .unwrap_or(&self.type_name)
            .to_string()
    }
}

// 引用 AionError 以保证错误类型在演示错误路径可用
#[allow(unused)]
fn _touch(e: AionError) -> String {
    e.to_string()
}

//! 交互式 REPL：直接体验 AION 服务（exec / read / write / chat / fetch / events）。

use std::path::PathBuf;

use aion_services::fs::FileService;
use aion_services::model::{ChatMessage, ModelService};
use aion_services::network::NetworkService;
use aion_services::security::SecurityContext;
use aion_services::terminal::TerminalService;
use tokio::io::AsyncBufReadExt;

use crate::boot;

const HELP: &str = r#"命令:
  help                  显示本帮助
  services              列出系统服务及状态
  events                查看最近事件（监视通道）
  exec <命令>           沙箱内执行 shell 命令
  read <路径>           读文件（工作区白名单内）
  write <路径> <内容>   写文件
  chat <文本>           与模型对话
  fetch <http-url>      抓取网页标题
  caps                  查看当前 capability
  exit                  退出（优雅关闭运行时）
"#;

pub async fn run() -> anyhow::Result<()> {
    let ctx = boot::boot(boot::load_config()).await?;
    let mut monitor = ctx.monitor();

    let ws: PathBuf = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
    let sec = SecurityContext::new("repl-user")
        .allow_all()
        .root(&ws)
        .net("*")
        .max_processes(16);

    println!("AION REPL — 输入 help 查看命令。工作区: {}", ws.display());
    let stdin = tokio::io::stdin();
    let mut lines = tokio::io::BufReader::new(stdin).lines();

    loop {
        print!("\x1b[1;36maion>\x1b[0m ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let Some(line) = lines.next_line().await? else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (cmd, rest) = match line.split_once(' ') {
            Some((c, r)) => (c, r.trim()),
            None => (line, ""),
        };
        let result: anyhow::Result<String> = match cmd {
            "help" | "?" => Ok(HELP.to_string()),
            "exit" | "quit" => break,
            "caps" => Ok(format!("caps: {:?}", sec.caps)),
            "services" => Ok(ctx
                .list_services()
                .into_iter()
                .map(|s| format!("  {:<10} {:<10} deps={:?}", s.name, s.state.as_str(), s.deps))
                .collect::<Vec<_>>()
                .join("\n")),
            "events" => {
                let mut out = Vec::new();
                while let Ok(ev) = monitor.try_recv() {
                    out.push(format!("  [{}] {} (scope={})", ev.seq, ev.name, ev.scope));
                }
                if out.is_empty() {
                    Ok("  （暂无新事件）".into())
                } else {
                    Ok(out.join("\n"))
                }
            }
            "exec" if !rest.is_empty() => {
                let terminal = ctx.require::<TerminalService>().await?;
                let (program, args) = crate::agents::shell_args(rest);
                let out = terminal
                    .run_command(&sec, program, &args, None, std::time::Duration::from_secs(30))
                    .await?;
                Ok(format!(
                    "exit={} | sandboxed={}\n{}{}",
                    out.code,
                    out.sandboxed,
                    out.stdout,
                    if out.stderr.is_empty() { String::new() } else { out.stderr }
                ))
            }
            "read" if !rest.is_empty() => {
                let file = ctx.require::<FileService>().await?;
                let data = file.read(&sec, rest).await?;
                Ok(String::from_utf8_lossy(&data).into_owned())
            }
            "write" => {
                let (path, content) = rest.split_once(' ').ok_or_else(|| {
                    anyhow::anyhow!("usage: write <路径> <内容>")
                })?;
                let file = ctx.require::<FileService>().await?;
                file.write(&sec, path, content.as_bytes()).await?;
                Ok(format!("✓ 已写入 {path}（{} 字节）", content.len()))
            }
            "chat" if !rest.is_empty() => {
                let model = ctx.require::<ModelService>().await?;
                model
                    .chat(
                        &sec,
                        None,
                        &[ChatMessage::system("你是 AION 上的助手"), ChatMessage::user(rest)],
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!(e))
            }
            "fetch" if !rest.is_empty() => {
                let network = ctx.require::<NetworkService>().await?;
                let reply = network.http_get(&sec, rest).await?;
                Ok(format!(
                    "status={} bytes={} title={:?}",
                    reply.status,
                    reply.body.len(),
                    reply.title().unwrap_or_default()
                ))
            }
            _ => Ok(format!("未知命令 `{cmd}` —— 输入 help 查看命令")),
        };
        match result {
            Ok(text) => println!("{text}"),
            Err(e) => println!("\x1b[31m✗ {e}\x1b[0m"),
        }
    }

    ctx.dispose().await?;
    println!("运行时已关闭，再见。");
    Ok(())
}

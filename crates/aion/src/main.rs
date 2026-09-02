//! AION CLI 入口。

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "aion",
    version,
    about = "AION — Agent OS Runtime（Cordis-RS 核心 · 系统服务 · Linux 适配层）",
    after_help = "运行 `aion demo` 查看完整调用流程演示。"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 运行「调用流程示例」完整演示
    Demo,
    /// 启动运行时并执行一次 Agent 任务
    Run {
        /// agent 名称: coder / research / assistant / browser / custom
        #[arg(long, default_value = "assistant")]
        agent: String,
        /// 任务类型: run / chat / fetch / open / write / pipeline / echo
        #[arg(long, default_value = "chat")]
        kind: String,
        /// 任务输入
        #[arg(long, default_value = "介绍一下 AION")]
        input: String,
        /// 附加参数（JSON，如 --params '{"path":"a.txt","content":"hi"}'）
        #[arg(long, default_value = "{}")]
        params: String,
    },
    /// 交互式终端
    Repl,
    /// 列出运行时加载的系统服务
    Services,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Demo => aion::demo::run().await,
        Commands::Run {
            agent,
            kind,
            input,
            params,
        } => {
            let ctx = aion::boot::boot(aion::boot::load_config()).await?;
            let agent_impl = aion::agents::builtin()
                .into_iter()
                .find(|a| a.name() == agent)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "unknown agent `{agent}` (available: coder, research, assistant, browser, custom)"
                    )
                })?;
            let sec = aion::agents::developer_sec(agent_impl.name(), &[]);
            let params: serde_json::Value = serde_json::from_str(&params)
                .map_err(|e| anyhow::anyhow!("--params must be valid JSON: {e}"))?;
            let task = aion::agents::AgentTask { kind, input, params };
            println!("── {} ──", agent_impl.description());
            let output = agent_impl.handle(&ctx, &sec, &task).await?;
            println!("{output}");
            ctx.dispose().await?;
            Ok(())
        }
        Commands::Repl => aion::repl::run().await,
        Commands::Services => {
            let ctx = aion::boot::boot(aion::boot::load_config()).await?;
            println!("{:<10} {:<22} {:<10} deps", "name", "type", "state");
            for svc in ctx.list_services() {
                println!(
                    "{:<10} {:<22} {:<10} {:?}",
                    svc.name,
                    svc.type_name.rsplit("::").next().unwrap_or(&svc.type_name),
                    svc.state.as_str(),
                    svc.deps
                );
            }
            ctx.dispose().await?;
            Ok(())
        }
    }
}

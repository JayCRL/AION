//! AION 应用层集成测试：Agent 通过 Context 使用系统服务完成任务。

use aion::agents::{Agent, AgentTask};
use aion_services::SecurityContext;

async fn boot_ctx() -> cordis::Context {
    aion::boot::boot(aion::boot::load_config()).await.unwrap()
}

#[tokio::test]
async fn assistant_agent_chats_via_model_service() {
    let ctx = boot_ctx().await;
    let agent = aion::agents::builtin()
        .into_iter()
        .find(|a| a.name() == "assistant")
        .unwrap();

    let sec = SecurityContext::new("test-assistant").allow_all();
    let task = AgentTask::new("chat", "你好 AION");
    let reply = agent.handle(&ctx, &sec, &task).await.unwrap();
    assert!(reply.contains("AION"), "reply: {reply}");
    ctx.dispose().await.unwrap();
}

#[tokio::test]
async fn coder_agent_runs_command() {
    let ctx = boot_ctx().await;
    let agent = aion::agents::builtin()
        .into_iter()
        .find(|a| a.name() == "coder")
        .unwrap();

    let sec = SecurityContext::new("test-coder").allow_all();
    let task = AgentTask::new("run", "echo coder-agent-ok");
    let out = agent.handle(&ctx, &sec, &task).await.unwrap();
    assert!(out.contains("coder-agent-ok"), "out: {out}");
    ctx.dispose().await.unwrap();
}

#[tokio::test]
async fn coder_agent_write_pipeline() {
    let ctx = boot_ctx().await;
    let agent = aion::agents::builtin()
        .into_iter()
        .find(|a| a.name() == "coder")
        .unwrap();

    let root = std::env::temp_dir().join(format!("aion-agent-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut sec = SecurityContext::new("test-coder").allow_all();
    sec.fs_roots.push(root.clone());

    let file = root.join("note.txt");
    let mut task = AgentTask::new("pipeline", "build demo");
    task.params = serde_json::json!({
        "path": file.to_string_lossy(),
        "content": "hello from AION",
        "build": "echo built"
    });
    let out = agent.handle(&ctx, &sec, &task).await.unwrap();
    assert!(out.contains("流水线完成"), "out: {out}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello from AION");

    ctx.dispose().await.unwrap();
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn custom_agent_is_extensible() {
    let ctx = boot_ctx().await;
    let agent = aion::agents::builtin()
        .into_iter()
        .find(|a| a.name() == "custom")
        .unwrap();

    let sec = SecurityContext::new("test-custom");
    let task = AgentTask::new("echo", "任务内容");
    let out = agent.handle(&ctx, &sec, &task).await.unwrap();
    assert!(out.contains("任务内容"));
    ctx.dispose().await.unwrap();
}

#[tokio::test]
async fn builtin_agents_registry() {
    let agents = aion::agents::builtin();
    let names: Vec<&str> = agents.iter().map(|a| a.name()).collect();
    assert_eq!(
        names,
        vec!["coder", "research", "assistant", "browser", "custom"]
    );
}

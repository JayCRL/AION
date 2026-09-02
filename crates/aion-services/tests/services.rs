//! AION 服务层集成测试：权限检查、文件、终端、模型、存储、进程事件。

use std::time::Duration;

use aion_adapter::AdapterKit;
use aion_services::{AionError, SecurityContext};

fn kit() -> AdapterKit {
    AdapterKit::native(std::env::temp_dir().join(format!("aion-cg-{}", std::process::id())))
}

fn trusted(agent: &str, root: &std::path::Path) -> SecurityContext {
    SecurityContext::new(agent)
        .allow_all()
        .root(root)
        .net("*")
        .max_processes(16)
}

#[tokio::test]
async fn permission_denied_without_cap() {
    let ctx = cordis::App::new().run().await.unwrap();
    let services = aion_services::system_services(&kit(), std::env::temp_dir(), std::env::temp_dir());
    aion_services::provide_all(&ctx, services).unwrap();
    let file = ctx
        .require::<aion_services::fs::FileService>()
        .await
        .unwrap();

    let sec = SecurityContext::new("no-caps");
    let err = file.read(&sec, "tmp/anything").await.unwrap_err();
    assert!(matches!(err, AionError::PermissionDenied(_)));
    ctx.dispose().await.unwrap();
}

#[tokio::test]
async fn file_roundtrip_and_path_denied() {
    let ctx = cordis::App::new().run().await.unwrap();
    let root = std::env::temp_dir().join(format!("aion-fs-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let services = aion_services::system_services(&kit(), std::env::temp_dir(), std::env::temp_dir());
    aion_services::provide_all(&ctx, services).unwrap();
    let file = ctx
        .require::<aion_services::fs::FileService>()
        .await
        .unwrap();

    let sec = trusted("coder", &root);
    let path = root.join("demo.txt");
    file.write(&sec, &path, b"hello aion").await.unwrap();
    assert_eq!(file.read(&sec, &path).await.unwrap(), b"hello aion");

    // 白名单外路径被拒绝
    let outside = if cfg!(target_os = "windows") {
        std::path::PathBuf::from("C:\\Windows\\win.ini")
    } else {
        std::path::PathBuf::from("/etc/passwd")
    };
    let err = file.read(&sec, &outside).await.unwrap_err();
    assert!(matches!(err, AionError::PathDenied(_)));

    ctx.dispose().await.unwrap();
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn terminal_runs_echo_in_sandbox() {
    let ctx = cordis::App::new().run().await.unwrap();
    let root = std::env::temp_dir().join(format!("aion-term-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let services = aion_services::system_services(&kit(), std::env::temp_dir(), std::env::temp_dir());
    aion_services::provide_all(&ctx, services).unwrap();
    let terminal = ctx
        .require::<aion_services::terminal::TerminalService>()
        .await
        .unwrap();

    let sec = trusted("coder", &root);
    let outcome = terminal.echo(&sec, "hello AION").await.unwrap();
    assert_eq!(outcome.code, 0, "stderr: {}", outcome.stderr);
    assert!(
        outcome.stdout.contains("hello AION"),
        "stdout: {}",
        outcome.stdout
    );

    // 受限 agent 不能执行命令
    let restricted = SecurityContext::new("restricted").root(&root);
    let err = terminal.echo(&restricted, "rm -rf /").await.unwrap_err();
    assert!(matches!(err, AionError::PermissionDenied(_)));

    ctx.dispose().await.unwrap();
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn model_echo_chat() {
    let ctx = cordis::App::new().run().await.unwrap();
    let services = aion_services::system_services(&kit(), std::env::temp_dir(), std::env::temp_dir());
    aion_services::provide_all(&ctx, services).unwrap();
    let model = ctx
        .require::<aion_services::model::ModelService>()
        .await
        .unwrap();

    let sec = SecurityContext::new("assistant").allow_all();
    let reply = model
        .chat(
            &sec,
            None,
            &[
                aion_services::model::ChatMessage::system("你是 AION 上的助手"),
                aion_services::model::ChatMessage::user("你好，AION"),
            ],
        )
        .await
        .unwrap();
    assert!(reply.contains("AION"), "reply: {reply}");

    let backends = model.list_backends(&sec).unwrap();
    assert!(backends.contains(&"echo".to_string()));
    ctx.dispose().await.unwrap();
}

#[tokio::test]
async fn storage_quota_enforced() {
    let ctx = cordis::App::new().run().await.unwrap();
    let storage_root = std::env::temp_dir().join(format!("aion-store-q-{}", std::process::id()));
    let services = aion_services::system_services(&kit(), storage_root.clone(), std::env::temp_dir());
    aion_services::provide_all(&ctx, services).unwrap();
    let storage = ctx
        .require::<aion_services::storage::StorageService>()
        .await
        .unwrap();

    let sec = SecurityContext::new("app").allow_all();
    storage.allocate(&sec, "tenant-a", 1000).await.unwrap();
    storage
        .write_file(&sec, "tenant-a", "a.txt", vec![0u8; 600].as_slice())
        .await
        .unwrap();
    // 超配额被拒绝（配额元数据文件也计入用量）
    let err = storage
        .write_file(&sec, "tenant-a", "b.txt", vec![0u8; 600].as_slice())
        .await
        .unwrap_err();
    assert!(matches!(err, AionError::Limit(_)));
    let usage = storage.usage(&sec, "tenant-a").await.unwrap();
    assert_eq!(usage.max_bytes, 1000);
    assert!(usage.used_bytes >= 600);

    ctx.dispose().await.unwrap();
    std::fs::remove_dir_all(&storage_root).ok();
}

#[tokio::test]
async fn process_spawn_emits_exit_event() {
    let ctx = cordis::App::new().run().await.unwrap();
    let root = std::env::temp_dir().join(format!("aion-proc-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let services = aion_services::system_services(&kit(), std::env::temp_dir(), std::env::temp_dir());
    aion_services::provide_all(&ctx, services).unwrap();
    let process = ctx
        .require::<aion_services::process::ProcessService>()
        .await
        .unwrap();

    let mut monitor = ctx.monitor();
    let sec = trusted("coder", &root);

    #[cfg(target_os = "windows")]
    let spec = aion_adapter::process::ProcessSpec::new(["cmd", "/C", "echo", "aion-proc-test"]);
    #[cfg(not(target_os = "windows"))]
    let spec = aion_adapter::process::ProcessSpec::new(["echo", "aion-proc-test"]);

    let task = process.spawn(&sec, spec, true).await.unwrap();
    let code = task.wait().await;
    assert_eq!(code, 0);

    // 等待退出事件到达
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut found = false;
    while std::time::Instant::now() < deadline {
        match monitor.try_recv() {
            Ok(ev) if ev.name == "aion:process:exit" => {
                let payload = ev
                    .payload::<aion_services::process::ProcessExitEvent>()
                    .expect("typed payload");
                assert_eq!(payload.code, 0);
                found = true;
                break;
            }
            Ok(_) => continue,
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(_) => break,
        }
    }
    assert!(found, "process exit event should be observed");
    assert_eq!(process.live_count(), 0, "live table should be cleaned");

    ctx.dispose().await.unwrap();
    std::fs::remove_dir_all(&root).ok();
}

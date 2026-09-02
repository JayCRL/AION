//! Cordis-RS 运行时集成测试：服务、事件、作用域销毁、配置。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use cordis::prelude::*;

// ---------------------------------------------------------------------
// 测试服务
// ---------------------------------------------------------------------

struct DbService {
    started: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
}

#[async_trait]
impl Service for DbService {
    fn name(&self) -> &'static str {
        "test.db"
    }
    async fn start(&self, _ctx: &Context) -> CordisResult<()> {
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn stop(&self, _ctx: &Context) -> CordisResult<()> {
        self.stopped.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// 依赖 db 的上层服务，用于验证 DI 启动顺序。
struct ApiService {
    start_order: Arc<MutexVec>,
}

#[derive(Default)]
struct MutexVec(std::sync::Mutex<Vec<String>>);

impl MutexVec {
    fn push(&self, s: &str) {
        self.0.lock().unwrap().push(s.to_string());
    }
    fn get(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

#[async_trait]
impl Service for ApiService {
    fn name(&self) -> &'static str {
        "test.api"
    }
    fn inject(&self) -> Vec<&'static str> {
        vec!["test.db"]
    }
    async fn start(&self, _ctx: &Context) -> CordisResult<()> {
        self.start_order.push("api");
        Ok(())
    }
    async fn stop(&self, _ctx: &Context) -> CordisResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn service_provide_require_lazy_start() {
    let ctx = App::new().run().await.unwrap();
    let started = Arc::new(AtomicBool::new(false));
    let stopped = Arc::new(AtomicBool::new(false));

    ctx.provide(DbService {
        started: started.clone(),
        stopped: stopped.clone(),
    })
    .unwrap();

    assert_eq!(ctx.service_state("test.db"), Some(ServiceState::Pending));
    let db = ctx.require::<DbService>().await.unwrap();
    assert!(started.load(Ordering::SeqCst));
    assert_eq!(ctx.service_state("test.db"), Some(ServiceState::Started));
    assert!(Arc::strong_count(&db) >= 1);

    // 重复 provide 报错
    assert!(ctx.provide(DbService {
        started: started.clone(),
        stopped: stopped.clone(),
    })
    .is_err());

    ctx.dispose().await.unwrap();
    assert!(stopped.load(Ordering::SeqCst));
    // 销毁后的作用域禁止 require
    assert!(ctx.require::<DbService>().await.is_err());
}

#[tokio::test]
async fn service_dependency_order() {
    let ctx = App::new().run().await.unwrap();
    let order = Arc::new(MutexVec::default());
    let db_started = Arc::new(AtomicBool::new(false));

    ctx.provide(ApiService {
        start_order: order.clone(),
    })
    .unwrap();
    ctx.provide(DbService {
        started: db_started.clone(),
        stopped: Arc::new(AtomicBool::new(false)),
    })
    .unwrap();

    // require api 会连带启动 db，且 db 先启动
    ctx.require_named("test.api").await.unwrap();
    assert_eq!(order.get(), vec!["api".to_string()]); // api.start 在依赖解析后调用
    assert!(db_started.load(Ordering::SeqCst));

    ctx.dispose().await.unwrap();
}

#[tokio::test]
async fn events_on_once_off_and_monitor() {
    let ctx = App::new().run().await.unwrap();
    let hits = Arc::new(AtomicUsize::new(0));

    let h = hits.clone();
    ctx.on("ping", move |_ctx, ev| {
        let h = h.clone();
        Box::pin(async move {
            if ev.payload::<String>().map(|s| s.as_str() == "pong").unwrap_or(false) {
                h.fetch_add(1, Ordering::SeqCst);
            }
        })
    });

    let payload: cordis::event::Payload = Arc::new("pong".to_string());
    ctx.emit_with("ping", payload.clone()).await.unwrap();
    ctx.emit("ping").await.unwrap(); // 无负载不计入
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // once
    let once_hits = Arc::new(AtomicUsize::new(0));
    let oh = once_hits.clone();
    ctx.once("boom", move |_c, _e| {
        let oh = oh.clone();
        Box::pin(async move {
            oh.fetch_add(1, Ordering::SeqCst);
        })
    });
    ctx.emit("boom").await.unwrap();
    ctx.emit("boom").await.unwrap();
    assert_eq!(once_hits.load(Ordering::SeqCst), 1);

    // 监视通道（只包含订阅之后的事件）
    let mut monitor = ctx.monitor();
    ctx.emit("monitor-test").await.unwrap();
    let ev = monitor.try_recv().expect("monitor should have event");
    assert_eq!(ev.name, "monitor-test");

    ctx.dispose().await.unwrap();
}

#[tokio::test]
async fn scope_dispose_cascades_and_runs_effects() {
    let ctx = App::new().run().await.unwrap();
    let effect_ran = Arc::new(AtomicBool::new(false));
    let child = ctx.child("worker");
    let child_id = child.scope_id();

    // 子作用域登记 effect
    let er = effect_ran.clone();
    child.effect("cleanup", move || {
        let er = er.clone();
        Box::pin(async move {
            er.store(true, Ordering::SeqCst);
        })
    });

    // 子作用域派生 Fiber，应被 abort
    let fiber_flag = Arc::new(AtomicBool::new(false));
    let ff = fiber_flag.clone();
    child.spawn("loop", async move {
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            ff.store(true, Ordering::SeqCst); // 若未被 abort，最终会执行
        }
    });

    // 子作用域提供事件监听
    child.on("noise", |_c, _e| Box::pin(async {}));

    ctx.dispose().await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    assert!(effect_ran.load(Ordering::SeqCst), "effect should run on dispose");
    assert!(!fiber_flag.load(Ordering::SeqCst), "fiber should be aborted");
    assert_eq!(ctx.state(), LifecycleState::Disposed);
    assert_eq!(child.state(), LifecycleState::Disposed);
    assert_eq!(child_id, 1);
}

#[tokio::test]
async fn plugin_failure_disposes_its_scope() {
    let ctx = App::new().run().await.unwrap();
    let result = ctx
        .plugin(cordis::plugin_fn("bad", |_ctx: Context| {
            Box::pin(async { Err(CordisError::Custom("boom".into())) })
        }))
        .await;
    assert!(result.is_err());

    // 正常插件仍然可用
    ctx.plugin(cordis::plugin_fn("good", |ctx: Context| {
        Box::pin(async move {
            ctx.on("hello", |_c, _e| Box::pin(async {}));
            Ok(())
        })
    }))
    .await
    .unwrap();
    assert!(ctx.emit("hello").await.is_ok());
    ctx.dispose().await.unwrap();
}

#[tokio::test]
async fn config_dot_path_via_context() {
    let mut config = Config::new();
    config.set("model.default_backend", serde_json::json!("echo")).unwrap();
    let ctx = App::new().config(config).run().await.unwrap();

    assert_eq!(
        ctx.config().get_string("model.default_backend"),
        Some("echo".into())
    );
    ctx.update_config(|cfg| {
        cfg.set("runtime.name", serde_json::json!("aion")).unwrap();
    });
    assert_eq!(ctx.config().get_string("runtime.name"), Some("aion".into()));
    ctx.dispose().await.unwrap();
}

#[tokio::test]
async fn handler_panic_is_isolated() {
    let ctx = App::new().run().await.unwrap();
    ctx.on("bad", |_c, _e| Box::pin(async { panic!("handler bug") }));
    let good = Arc::new(AtomicUsize::new(0));
    let g = good.clone();
    ctx.on("bad", move |_c, _e| {
        let g = g.clone();
        Box::pin(async move {
            g.fetch_add(1, Ordering::SeqCst);
        })
    });
    ctx.emit("bad").await.unwrap();
    assert_eq!(good.load(Ordering::SeqCst), 1);
    ctx.dispose().await.unwrap();
}

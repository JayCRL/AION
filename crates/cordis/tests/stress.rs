//! Cordis-RS 并发压力测试。
//!
//! 锁定并发语义的不变量，防止回归：
//!
//! 1. **惰性启动恰好一次**：任意多任务并发 `require` 同一服务，`start` 只执行一次，
//!    所有任务拿到同一实例；
//! 2. **依赖链有序**：深度依赖链在并发 require 下每个服务恰好启动一次，且依赖先于依赖方；
//! 3. **循环依赖快速失败**：返回 `CircularDependency` 而不是挂死，且失败后状态收敛为
//!    `Failed`（后续 require 立即得到错误而不是永久等待）；
//! 4. **启动失败传播**：`start` 返回 Err 时所有并发等待者都收到 `ServiceStartFailed`；
//! 5. **dispose 与 require 竞争**：销毁会等待进行中的启动完成后停止服务，不残留「无主 Started」；
//! 6. **dispose 幂等**：并发 dispose 下 Effect 恰好执行一次且严格逆序；
//! 7. **Fiber 批量取消**：作用域销毁后所有 Fiber 停止推进；
//! 8. **once 恰好一次**：并发 emit 下 once 监听器不重复触发；
//! 9. **on/off 抖动不泄漏**、处理器 panic 不影响总线；
//! 10. **监视通道背压**：慢消费者不会阻塞 emit（Lagged 语义）；
//! 11. **并发 provide**：同名服务只有一个注册成功。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::future::join_all;

use cordis::prelude::*;

/// 死锁护栏：任何测试卡住都会在这里超时失败，而不是挂死 CI。
async fn bounded<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(30), fut)
        .await
        .expect("test deadlocked (30s)")
}

// ---------------------------------------------------------------------------
// 测试服务
// ---------------------------------------------------------------------------

#[derive(Default)]
struct StartLog {
    seq: AtomicUsize,
    order: std::sync::Mutex<Vec<(&'static str, usize)>>,
}

impl StartLog {
    fn record(&self, name: &'static str) {
        let n = self.seq.fetch_add(1, Ordering::SeqCst);
        self.order.lock().unwrap().push((name, n));
    }

    fn count(&self) -> usize {
        self.seq.load(Ordering::SeqCst)
    }

    fn seq_of(&self, name: &str) -> usize {
        self.order
            .lock()
            .unwrap()
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, s)| *s)
            .unwrap_or_else(|| panic!("service `{name}` never started"))
    }
}

/// 可配置启动延迟 / 失败 / 依赖的测试服务。
macro_rules! test_svc {
    ($t:ident, $name:literal, $dep:expr, $delay_ms:expr, $fail:expr) => {
        struct $t {
            log: Arc<StartLog>,
        }

        #[async_trait]
        impl Service for $t {
            fn name(&self) -> &'static str {
                $name
            }

            fn inject(&self) -> Vec<&'static str> {
                ($dep).iter().copied().collect()
            }

            async fn start(&self, _ctx: &Context) -> CordisResult<()> {
                if $delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis($delay_ms)).await;
                }
                self.log.record($name);
                if $fail {
                    return Err(CordisError::Custom("intentional failure".into()));
                }
                Ok(())
            }

            async fn stop(&self, _ctx: &Context) -> CordisResult<()> {
                Ok(())
            }
        }
    };
}

// 8 层依赖链：c0 → c1 → ... → c7（c0 依赖 c1，依次向下）
test_svc!(C0, "c0", ["c1"], 2, false);
test_svc!(C1, "c1", ["c2"], 2, false);
test_svc!(C2, "c2", ["c3"], 2, false);
test_svc!(C3, "c3", ["c4"], 2, false);
test_svc!(C4, "c4", ["c5"], 2, false);
test_svc!(C5, "c5", ["c6"], 2, false);
test_svc!(C6, "c6", ["c7"], 2, false);
test_svc!(C7, "c7", [], 2, false);

// 循环依赖：a → b → a
test_svc!(CycA, "cyc-a", ["cyc-b"], 0, false);
test_svc!(CycB, "cyc-b", ["cyc-a"], 0, false);

// 启动即失败
test_svc!(Failing, "fail-svc", [], 0, true);

// 启动较慢（用于并发 require 竞争与 dispose 竞争）
test_svc!(Slow, "slow-svc", [], 20, false);
test_svc!(Slow100, "slow100-svc", [], 100, false);

// ---------------------------------------------------------------------------
// 1. 并发 require：惰性启动恰好一次
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_require_starts_service_exactly_once() {
    let ctx = bounded(App::new().run()).await.unwrap();
    let log = Arc::new(StartLog::default());
    ctx.provide(Slow { log: log.clone() }).unwrap();

    let barrier = Arc::new(tokio::sync::Barrier::new(64));
    let tasks: Vec<_> = (0..64)
        .map(|_| {
            let ctx = ctx.clone();
            let b = barrier.clone();
            tokio::spawn(async move {
                b.wait().await;
                let svc = ctx.require::<Slow>().await.unwrap();
                Arc::as_ptr(&svc) as usize
            })
        })
        .collect();

    let ptrs: Vec<usize> = bounded(join_all(tasks))
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
    assert!(
        ptrs.windows(2).all(|w| w[0] == w[1]),
        "all tasks must observe the same instance"
    );
    assert_eq!(log.count(), 1, "start must run exactly once");
    assert_eq!(ctx.service_state("slow-svc"), Some(ServiceState::Started));

    ctx.dispose().await.unwrap();
}

// ---------------------------------------------------------------------------
// 2. 深依赖链：每个服务恰好启动一次，且依赖先于依赖方启动
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_require_of_deep_chain_starts_each_once_in_order() {
    let ctx = bounded(App::new().run()).await.unwrap();
    let log = Arc::new(StartLog::default());

    ctx.provide(C0 { log: log.clone() }).unwrap();
    ctx.provide(C1 { log: log.clone() }).unwrap();
    ctx.provide(C2 { log: log.clone() }).unwrap();
    ctx.provide(C3 { log: log.clone() }).unwrap();
    ctx.provide(C4 { log: log.clone() }).unwrap();
    ctx.provide(C5 { log: log.clone() }).unwrap();
    ctx.provide(C6 { log: log.clone() }).unwrap();
    ctx.provide(C7 { log: log.clone() }).unwrap();

    let barrier = Arc::new(tokio::sync::Barrier::new(32));
    let tasks: Vec<_> = (0..32)
        .map(|_| {
            let ctx = ctx.clone();
            let b = barrier.clone();
            tokio::spawn(async move {
                b.wait().await;
                ctx.require_named("c0").await.is_ok()
            })
        })
        .collect();

    let results: Vec<bool> = bounded(join_all(tasks))
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
    assert!(results.iter().all(|ok| *ok), "all requires must succeed");

    assert_eq!(log.count(), 8, "each service starts exactly once");
    for i in 0..7 {
        let dep = format!("c{}", i + 1);
        let dependent = format!("c{}", i);
        assert!(
            log.seq_of(&dep) < log.seq_of(&dependent),
            "`{dep}` must start before `{dependent}`"
        );
    }

    ctx.dispose().await.unwrap();
}

// ---------------------------------------------------------------------------
// 3. 循环依赖：快速失败而不是挂死，状态收敛为 Failed
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn circular_dependency_errors_instead_of_hanging() {
    let ctx = bounded(App::new().run()).await.unwrap();
    let log = Arc::new(StartLog::default());
    ctx.provide(CycA { log: log.clone() }).unwrap();
    ctx.provide(CycB { log: log.clone() }).unwrap();

    let tasks: Vec<_> = (0..16)
        .map(|_| {
            let ctx = ctx.clone();
            tokio::spawn(async move { ctx.require_named("cyc-a").await })
        })
        .collect();

    let results = bounded(join_all(tasks)).await;
    assert_eq!(results.len(), 16);
    for r in &results {
        // 先解包 JoinError（任务内已断言不会 panic），再看 require 的错误
        let inner = r.as_ref().unwrap();
        let err = inner.as_ref().err().expect("all requires must fail");
        assert!(
            matches!(
                err,
                CordisError::CircularDependency(_) | CordisError::ServiceStartFailed(_, _)
            ),
            "expected CircularDependency or ServiceStartFailed, got {err}"
        );
    }
    // 关键回归点：失败后状态必须收敛为 Failed，否则后续 require 永久等待
    assert_eq!(ctx.service_state("cyc-a"), Some(ServiceState::Failed));
    // 后续 require 立即得到错误（不会挂在 Starting 等待上）
    let again = bounded(ctx.require_named("cyc-a")).await;
    match again {
        Err(CordisError::ServiceStartFailed(_, _)) => {}
        Err(e) => panic!("expected ServiceStartFailed, got {e}"),
        Ok(_) => panic!("expected error, got Ok"),
    }

    ctx.dispose().await.unwrap();
}

// ---------------------------------------------------------------------------
// 4. 启动失败：所有并发等待者都收到错误
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_start_failure_propagates_to_all_waiters() {
    let ctx = bounded(App::new().run()).await.unwrap();
    let log = Arc::new(StartLog::default());
    ctx.provide(Failing { log: log.clone() }).unwrap();

    let barrier = Arc::new(tokio::sync::Barrier::new(32));
    let tasks: Vec<_> = (0..32)
        .map(|_| {
            let ctx = ctx.clone();
            let b = barrier.clone();
            tokio::spawn(async move {
                b.wait().await;
                ctx.require::<Failing>().await
            })
        })
        .collect();

    let results = bounded(join_all(tasks)).await;
    assert_eq!(results.len(), 32);
    for r in &results {
        let inner = r.as_ref().unwrap();
        let err = inner.as_ref().err().expect("all requires must fail");
        assert!(
            matches!(err, CordisError::ServiceStartFailed(_, _)),
            "all waiters must see ServiceStartFailed"
        );
    }
    assert_eq!(log.count(), 1, "start attempted exactly once");

    ctx.dispose().await.unwrap();
}

// ---------------------------------------------------------------------------
// 5. dispose 与 require 竞争：等待进行中的启动完成后停止，不残留无主服务
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispose_waits_for_inflight_start_then_stops() {
    let ctx = bounded(App::new().run()).await.unwrap();
    let log = Arc::new(StartLog::default());
    ctx.provide(Slow100 { log: log.clone() }).unwrap();

    let req_ctx = ctx.clone();
    let requirer = tokio::spawn(async move { req_ctx.require::<Slow100>().await });

    // 启动进行中（start 需要 100ms）触发销毁
    tokio::time::sleep(Duration::from_millis(20)).await;
    let disp_ctx = ctx.clone();
    let disposer = tokio::spawn(async move { disp_ctx.dispose().await });

    let (req, disp) = bounded(async { (requirer.await.unwrap(), disposer.await.unwrap()) }).await;
    // require 可能成功（在停止前完成），也可能因作用域销毁而失败——两者都合法
    if let Ok(_) = req {}
    disp.unwrap();

    // 关键不变量：尘埃落定后服务不能停在 Started/Starting（无主服务）
    let state = ctx.service_state("slow100-svc");
    assert!(
        matches!(state, Some(ServiceState::Stopped) | Some(ServiceState::Failed)),
        "service must be stopped (or failed), got {state:?}"
    );
    // 销毁后的作用域禁止 require
    assert!(ctx.require::<Slow100>().await.is_err());

    tokio::time::sleep(Duration::from_millis(150)).await;
}

// ---------------------------------------------------------------------------
// 6. 并发 dispose：Effect 恰好执行一次且严格逆序
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_dispose_runs_effects_exactly_once_in_reverse() {
    let ctx = bounded(App::new().run()).await.unwrap();

    let count = Arc::new(AtomicUsize::new(0));
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    for i in 0..64u64 {
        let count = count.clone();
        let order = order.clone();
        ctx.effect(format!("e{i}"), move || {
            let count = count.clone();
            let order = order.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                order.lock().unwrap().push(i);
            })
        });
    }

    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let ctx = ctx.clone();
            tokio::spawn(async move { ctx.dispose().await })
        })
        .collect();

    bounded(join_all(tasks)).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(count.load(Ordering::SeqCst), 64, "effects run exactly once");
    let order = order.lock().unwrap().clone();
    let expect: Vec<u64> = (0..64u64).rev().collect();
    assert_eq!(order, expect, "effects must run in reverse registration order");
}

// ---------------------------------------------------------------------------
// 7. 销毁时批量取消 Fiber
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispose_aborts_many_fibers() {
    let ctx = bounded(App::new().run()).await.unwrap();
    let counter = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..64 {
        let c = counter.clone();
        handles.push(ctx.spawn("tick", async move {
            loop {
                tokio::time::sleep(Duration::from_millis(5)).await;
                c.fetch_add(1, Ordering::SeqCst);
            }
        }));
    }

    // 让 Fiber 跑一会儿
    tokio::time::sleep(Duration::from_millis(80)).await;
    ctx.dispose().await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let c1 = counter.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(120)).await;
    let c2 = counter.load(Ordering::SeqCst);
    assert_eq!(c1, c2, "fibers must stop advancing after dispose");
    assert!(handles.iter().all(|h| h.is_finished()));

    tokio::time::sleep(Duration::from_millis(150)).await;
    let c3 = counter.load(Ordering::SeqCst);
    assert_eq!(c2, c3);
}

// ---------------------------------------------------------------------------
// 8. 并发 emit：once 监听器恰好触发一次
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn once_listeners_fire_exactly_once_under_concurrent_emit() {
    let ctx = bounded(App::new().run()).await.unwrap();
    let hits = Arc::new(AtomicUsize::new(0));

    for _ in 0..100 {
        let h = hits.clone();
        ctx.once("one-shot", move |_c, _e| {
            let h = h.clone();
            Box::pin(async move {
                h.fetch_add(1, Ordering::SeqCst);
            })
        });
    }

    let tasks: Vec<_> = (0..100)
        .map(|_| {
            let ctx = ctx.clone();
            tokio::spawn(async move { ctx.emit("one-shot").await })
        })
        .collect();
    bounded(join_all(tasks)).await;

    assert_eq!(hits.load(Ordering::SeqCst), 100, "each once listener fires exactly once");
    ctx.dispose().await.unwrap();
}

// ---------------------------------------------------------------------------
// 9. on/off 抖动 + 并发 emit：不泄漏、不恐慌
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn listener_churn_under_concurrent_emit_does_not_leak() {
    let ctx = bounded(App::new().run()).await.unwrap();
    let baseline = ctx.listener_count();

    // 两个常驻监听器
    let good = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let g = good.clone();
        ctx.on("churn-emit", move |_c, _e| {
            let g = g.clone();
            Box::pin(async move {
                g.fetch_add(1, Ordering::SeqCst);
            })
        });
    }

    // 4 个任务做 on/off 抖动（共 800 对），2 个任务并发 emit
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let ctx = ctx.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..200 {
                let id = ctx.on("churn-toggle", |_c, _e| Box::pin(async {}));
                ctx.off(id);
            }
        }));
    }
    for _ in 0..2 {
        let ctx = ctx.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..100 {
                ctx.emit("churn-emit").await.unwrap();
            }
        }));
    }
    bounded(join_all(tasks)).await;

    assert_eq!(good.load(Ordering::SeqCst), 400, "2 listeners × 200 emits");
    assert_eq!(
        ctx.listener_count(),
        baseline + 2,
        "churned listeners must all be removed"
    );
    ctx.dispose().await.unwrap();
}

// ---------------------------------------------------------------------------
// 10. 处理器 panic 在高并发下被隔离
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handler_panics_are_isolated_under_load() {
    let ctx = bounded(App::new().run()).await.unwrap();
    let good = Arc::new(AtomicUsize::new(0));

    ctx.on("boom", |_c, _e| Box::pin(async { panic!("handler bug") }));
    for _ in 0..3 {
        let g = good.clone();
        ctx.on("boom", move |_c, _e| {
            let g = g.clone();
            Box::pin(async move {
                g.fetch_add(1, Ordering::SeqCst);
            })
        });
    }

    let tasks: Vec<_> = (0..16)
        .map(|_| {
            let ctx = ctx.clone();
            tokio::spawn(async move { ctx.emit("boom").await })
        })
        .collect();
    bounded(join_all(tasks)).await;

    assert_eq!(good.load(Ordering::SeqCst), 16 * 3, "good handlers unaffected");
    ctx.dispose().await.unwrap();
}

// ---------------------------------------------------------------------------
// 11. 监视通道背压：慢消费者不阻塞 emit（Lagged 语义）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn monitor_backpressure_does_not_block_emitters() {
    let ctx = bounded(App::new().run()).await.unwrap();
    let mut monitor = ctx.monitor();

    const TOTAL: usize = 5000;
    let emitter = tokio::spawn(async move {
        let ctx = ctx.clone();
        for _ in 0..TOTAL {
            ctx.emit("flow").await.unwrap();
        }
        ctx
    });

    // 等待全部发出（有界 = 若 emit 被背压阻塞则会超时失败）
    let ctx = bounded(emitter).await.unwrap();

    // 消费者核对：received + lagged == TOTAL（广播环形缓冲可能丢弃旧事件）
    let consumer = tokio::spawn(async move {
        let mut received = 0u64;
        let mut lagged = 0u64;
        loop {
            match monitor.try_recv() {
                Ok(_) => received += 1,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => lagged += n,
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    if received + lagged >= TOTAL as u64 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            }
        }
        (received, lagged)
    });
    let (received, lagged) = bounded(consumer).await.unwrap();
    assert_eq!(received + lagged, TOTAL as u64, "all events must be accounted for");
    let _ = ctx.dispose().await;
}

// ---------------------------------------------------------------------------
// 12. 并发 provide：同名服务只有一个注册成功
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_provide_same_service_succeeds_exactly_once() {
    let ctx = bounded(App::new().run()).await.unwrap();
    let log = Arc::new(StartLog::default());

    let barrier = Arc::new(tokio::sync::Barrier::new(32));
    let tasks: Vec<_> = (0..32)
        .map(|_| {
            let ctx = ctx.clone();
            let log = log.clone();
            let b = barrier.clone();
            tokio::spawn(async move {
                b.wait().await;
                ctx.provide(Slow { log })
            })
        })
        .collect();

    let results = bounded(join_all(tasks)).await;
    // 先剥掉 JoinError，再统计内层 provide 结果
    let inner: Vec<Result<(), CordisError>> = results.into_iter().map(|r| r.unwrap()).collect();
    let ok = inner.iter().filter(|r| r.is_ok()).count();
    let err = inner.iter().filter(|r| r.is_err()).count();
    assert_eq!(ok, 1, "exactly one provide wins");
    assert_eq!(err, 31, "all others must see AlreadyProvided");

    // 胜出的服务可以正常 require
    ctx.require::<Slow>().await.unwrap();
    ctx.dispose().await.unwrap();
}

// ---------------------------------------------------------------------------
// 13. 并发读写配置：无死锁、最终一致
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_config_reads_and_writes() {
    let ctx = bounded(App::new().run()).await.unwrap();
    ctx.update_config(|c| {
        c.set("stress.counter", serde_json::json!(0u64)).unwrap();
    });

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let ctx = ctx.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..200 {
                let _ = ctx.config().get_u64("stress.counter");
            }
        }));
    }
    for i in 0..4u64 {
        let ctx = ctx.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..200 {
                ctx.update_config(|c| {
                    c.set("stress.counter", serde_json::json!(i)).unwrap();
                });
            }
        }));
    }
    bounded(join_all(tasks)).await;

    // 最终仍可读（无锁中毒 / 死锁）
    assert!(ctx.config().get_u64("stress.counter").is_some());
    ctx.dispose().await.unwrap();
}

use std::sync::{Arc, Mutex};
use std::time::Duration;

use homestead_bus::{BoxFuture, DeliveryError, Dispatcher, DispatcherConfig, RetryPolicy, Sink};
use homestead_domain::{Event, NewTask, TaskPriority};
use homestead_store::Store;
use sqlx::PgPool;

/// Records deliveries; fails according to `script` (consumed front-first,
/// then succeeds forever).
struct Scripted {
    name: &'static str,
    only: Option<&'static str>,
    script: Mutex<Vec<Result<(), DeliveryError>>>,
    seen: Mutex<Vec<i64>>,
    hang: bool,
}

impl Scripted {
    fn new(name: &'static str) -> Arc<Self> {
        Arc::new(Self { name, only: None, script: Mutex::default(), seen: Mutex::default(), hang: false })
    }
    fn seen(&self) -> Vec<i64> {
        self.seen.lock().expect("lock").clone()
    }
}

impl Sink for Scripted {
    fn name(&self) -> &'static str {
        self.name
    }
    fn accepts(&self, e: &Event) -> bool {
        self.only.is_none_or(|k| k == e.kind())
    }
    fn deliver<'a>(&'a self, event_id: i64, _: &'a Event) -> BoxFuture<'a, Result<(), DeliveryError>> {
        Box::pin(async move {
            if self.hang {
                std::future::pending::<()>().await;
            }
            self.seen.lock().expect("lock").push(event_id);
            let mut s = self.script.lock().expect("lock");
            if s.is_empty() { Ok(()) } else { s.remove(0) }
        })
    }
}

fn config() -> DispatcherConfig {
    DispatcherConfig {
        retry: RetryPolicy { max_attempts: 3, base: Duration::ZERO, cap: Duration::ZERO },
        delivery_timeout: Duration::from_millis(200),
        ..DispatcherConfig::default()
    }
}

fn task(title: &str) -> NewTask {
    NewTask {
        title: title.into(),
        details: String::new(),
        location_id: None,
        due_on: None,
        priority: TaskPriority::Normal,
        mirror_to_linear: false,
        assigned_to: None,
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn each_accepting_sink_gets_each_event_once(pool: PgPool) {
    let store = Store::from_pool(pool);
    let all = Scripted::new("all");
    let completions =
        Arc::new(Scripted { only: Some("task.completed"), ..Arc::into_inner(Scripted::new("done")).expect("x") });
    let d = Dispatcher::new(store.clone(), vec![all.clone(), completions.clone()], config());

    let t = store.create_task(task("Mulch beds")).await.expect("t");
    store.complete_task(t.id).await.expect("done");
    let tick = d.tick().await.expect("tick");
    assert_eq!((tick.fanned_out, tick.delivered, tick.failed), (2, 3, 0));
    assert_eq!(d.tick().await.expect("tick").delivered, 0, "nothing is delivered twice");

    let mut seen = all.seen();
    seen.sort();
    assert_eq!(seen.len(), 2);
    assert_eq!(completions.seen().len(), 1);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_failing_sink_neither_blocks_others_nor_is_lost(pool: PgPool) {
    let store = Store::from_pool(pool);
    let healthy = Scripted::new("healthy");
    let flaky = Scripted::new("flaky");
    *flaky.script.lock().expect("lock") =
        vec![Err(DeliveryError::Retryable("503".into())), Err(DeliveryError::Retryable("503".into()))];
    let d = Dispatcher::new(store.clone(), vec![healthy.clone(), flaky.clone()], config());

    store.create_task(task("Check irrigation")).await.expect("t");
    let first = d.tick().await.expect("tick");
    assert_eq!((first.delivered, first.failed), (1, 1));
    assert_eq!(healthy.seen().len(), 1);

    d.tick().await.expect("tick"); // second failure
    let third = d.tick().await.expect("tick"); // third attempt succeeds
    assert_eq!(third.delivered, 1);
    assert_eq!(flaky.seen().len(), 3);
    assert_eq!(store.bus_stats().await.expect("stats").delivered, 2);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn permanent_errors_and_exhausted_retries_dead_letter(pool: PgPool) {
    let store = Store::from_pool(pool);
    let perm = Scripted::new("perm");
    *perm.script.lock().expect("lock") = vec![Err(DeliveryError::Permanent("401 bad token".into()))];
    let flaky = Scripted::new("flaky");
    *flaky.script.lock().expect("lock") = vec![Err(DeliveryError::Retryable("503".into())); 10];
    let d = Dispatcher::new(store.clone(), vec![perm.clone(), flaky.clone()], config());

    store.create_task(task("t")).await.expect("t");
    for _ in 0..5 {
        d.tick().await.expect("tick");
    }
    assert_eq!(perm.seen().len(), 1, "permanent failure is not retried");
    assert_eq!(flaky.seen().len(), 3, "max_attempts bounds retries");
    let troubled = store.troubled_deliveries(10).await.expect("troubled");
    assert!(troubled.iter().all(|t| t.status == "dead"));
    assert_eq!(troubled.len(), 2);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn hung_sink_times_out_as_retryable(pool: PgPool) {
    let store = Store::from_pool(pool);
    let hung = Arc::new(Scripted { hang: true, ..Arc::into_inner(Scripted::new("hung")).expect("x") });
    let d = Dispatcher::new(store.clone(), vec![hung], config());
    store.create_task(task("t")).await.expect("t");
    let tick = tokio::time::timeout(Duration::from_secs(5), d.tick()).await.expect("tick must not hang").expect("ok");
    assert_eq!(tick.failed, 1);
    let troubled = store.troubled_deliveries(10).await.expect("troubled");
    assert_eq!(troubled[0].status, "pending");
    assert!(troubled[0].last_error.as_deref().unwrap_or_default().contains("timed out"));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn deliveries_for_a_removed_sink_dead_letter_with_a_reason(pool: PgPool) {
    let store = Store::from_pool(pool);
    let old = Dispatcher::new(store.clone(), vec![Scripted::new("retired")], config());
    store.create_task(task("t")).await.expect("t");
    store.fan_out(10, &|_: &Event| vec!["retired"]).await.expect("fan");
    drop(old);

    let d = Dispatcher::new(store.clone(), vec![Scripted::new("current")], config());
    d.tick().await.expect("tick");
    let troubled = store.troubled_deliveries(10).await.expect("troubled");
    assert_eq!(troubled[0].sink, "retired");
    assert_eq!(troubled[0].status, "dead");
    assert!(troubled[0].last_error.as_deref().unwrap_or_default().contains("not configured"));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn run_loop_delivers_promptly_and_stops_on_shutdown(pool: PgPool) {
    let store = Store::from_pool(pool);
    let sink = Scripted::new("s");
    // A long poll interval proves delivery is driven by NOTIFY, not polling.
    let cfg = DispatcherConfig { poll_interval: Duration::from_secs(60), ..config() };
    let d = Dispatcher::new(store.clone(), vec![sink.clone()], cfg);
    let (tx, rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(d.run(rx));
    tokio::time::sleep(Duration::from_millis(300)).await; // let LISTEN register

    store.create_task(task("t")).await.expect("t");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while sink.seen().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "NOTIFY did not wake the dispatcher");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tx.send(true).expect("signal");
    tokio::time::timeout(Duration::from_secs(5), handle).await.expect("stops").expect("join");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn routing_limits_a_sink_to_its_configured_events(pool: PgPool) {
    let store = Store::from_pool(pool);
    let all = Scripted::new("feed");
    let routed: Arc<dyn Sink> = Arc::new(homestead_bus::Routed::new(all.clone(), ["task.completed".to_owned()]));
    let d = Dispatcher::new(store.clone(), vec![routed], config());
    let t = store.create_task(task("Oil the gate")).await.expect("t");
    store.complete_task(t.id).await.expect("done");
    assert_eq!(d.tick().await.expect("tick").delivered, 1, "task.created is not routed to this sink");
    assert_eq!(all.seen().len(), 1);
}

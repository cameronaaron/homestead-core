//! The morning digest, against a real Postgres.

use homestead_domain::auth::{ActorKind, NewActor, Role};
use homestead_domain::digest::Digest;
use homestead_domain::{Event, NewInventoryItem, NewTask, TaskPriority};
use homestead_store::Store;
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::Date;
use time::macros::date;

const TODAY: Date = date!(2026 - 09 - 30);

fn due(title: &str, on: Date) -> NewTask {
    NewTask { due_on: Some(on), ..NewTask::titled(title) }
}

async fn posted(pool: &PgPool) -> Vec<Digest> {
    let rows: Vec<serde_json::Value> =
        sqlx::query_scalar("select payload from homestead.events where kind = 'digest.daily' order by id")
            .fetch_all(pool)
            .await
            .expect("events");
    rows.into_iter()
        .map(|p| match Event::from_parts("digest.daily", p).expect("decodes") {
            Event::DailyDigest { digest } => digest,
            other => panic!("unexpected {other:?}"),
        })
        .collect()
}

fn titles(tasks: &[homestead_domain::digest::DigestTask]) -> Vec<&str> {
    tasks.iter().map(|t| t.title.as_str()).collect()
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_digest_sorts_open_work_and_goes_out_once_a_day(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let ava = store
        .create_actor(NewActor {
            display_name: "Ava".into(),
            email: Some("ava@example.org".into()),
            kind: ActorKind::Human,
            roles: vec![Role::Crew],
        })
        .await
        .expect("ava");
    store
        .create_task(NewTask { assigned_to: Some(ava.id), ..due("Mend the fence", date!(2026 - 09 - 28)) })
        .await
        .expect("t");
    store.create_task(due("Order garlic", date!(2026 - 09 - 29))).await.expect("t");
    store
        .create_task(NewTask { priority: TaskPriority::High, ..due("Fix irrigation at Bed 3", TODAY) })
        .await
        .expect("t");
    store.create_task(NewTask { priority: TaskPriority::Urgent, ..NewTask::titled("Call the vet") }).await.expect("t");
    store.create_task(due("Plan spring beds", date!(2026 - 10 - 20))).await.expect("normal and later: left out");
    let done = store.create_task(due("Already done", date!(2026 - 09 - 01))).await.expect("t");
    store.complete_task(done.id).await.expect("done");
    store
        .create_inventory_item(NewInventoryItem {
            name: "Layer feed".into(),
            unit: "bag".into(),
            reorder_level: Decimal::from(2),
        })
        .await
        .expect("feed");
    store
        .create_inventory_item(NewInventoryItem {
            name: "Twine".into(),
            unit: "roll".into(),
            reorder_level: Decimal::ZERO,
        })
        .await
        .expect("no level set");

    let d = store.send_daily_digest_as_of(TODAY).await.expect("send").expect("something to say");
    assert_eq!(titles(&d.overdue), ["Mend the fence", "Order garlic"], "oldest first");
    assert_eq!(d.overdue[0].assignee, "Ava");
    assert_eq!(titles(&d.due_today), ["Fix irrigation at Bed 3"]);
    assert_eq!(titles(&d.high_priority), ["Call the vet"], "high-priority work not already listed");
    assert_eq!(d.low_stock, ["Layer feed (0 bag)"], "only items with a reorder level");
    assert_eq!(d.headline(), "2 overdue · 1 due today · 1 high priority · 1 low on stock");
    assert_eq!(posted(&pool).await, [d]);

    assert!(store.send_daily_digest_as_of(TODAY).await.expect("again").is_none(), "once a day");
    assert_eq!(posted(&pool).await.len(), 1);
    assert!(store.send_daily_digest_as_of(TODAY + time::Duration::days(1)).await.expect("tomorrow").is_some());
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_quiet_day_posts_nothing_and_stays_quiet(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    store.create_task(due("Plan spring beds", date!(2026 - 10 - 20))).await.expect("later");
    assert!(store.send_daily_digest_as_of(TODAY).await.expect("send").is_none());
    // Something going overdue later that day doesn't make a noon digest.
    store.create_task(due("Mend the fence", date!(2026 - 09 - 29))).await.expect("overdue");
    assert!(store.send_daily_digest_as_of(TODAY).await.expect("again").is_none());
    assert!(posted(&pool).await.is_empty());
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn two_runs_at_once_post_one_digest(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    store.create_task(due("Mend the fence", date!(2026 - 09 - 29))).await.expect("overdue");
    // Hold the day's claim uncommitted; a second run must wait on it and
    // then find the day taken, not post its own.
    let mut tx = pool.begin().await.expect("tx");
    sqlx::query("insert into homestead.daily_digests (on_date, posted) values ($1, true)")
        .bind(TODAY)
        .execute(&mut *tx)
        .await
        .expect("claim");
    let second = tokio::spawn({
        let store = store.clone();
        async move { store.send_daily_digest_as_of(TODAY).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!second.is_finished(), "the second run waits for the first");
    tx.commit().await.expect("commit");
    assert!(second.await.expect("join").expect("second run").is_none());
    assert!(posted(&pool).await.is_empty(), "only the claim's owner posts");
}

/// A zone where it is `hour` o'clock now (give or take the minutes).
fn zone_at(hour: i64) -> String {
    let utc = i64::from(time::OffsetDateTime::now_utc().hour());
    let offset = (hour - utc).rem_euclid(24);
    let offset = if offset > 12 { offset - 24 } else { offset };
    // POSIX names are inverted: Etc/GMT-9 is nine hours ahead of UTC.
    match offset {
        0 => "Etc/GMT".into(),
        o if o > 0 => format!("Etc/GMT-{o}"),
        o => format!("Etc/GMT+{}", -o),
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_digest_goes_out_only_in_the_morning(pool: PgPool) {
    Store::from_pool(pool.clone()).create_task(due("Mend the fence", date!(2020 - 01 - 01))).await.expect("overdue");
    // Mid-afternoon: a core that slept all morning skips the day.
    let afternoon = Store::from_pool(pool.clone()).with_timezone(&zone_at(15)).await.expect("zone");
    assert!(afternoon.send_daily_digest().await.expect("send").is_none());
    assert!(posted(&pool).await.is_empty(), "and doesn't claim it either");
    let morning = Store::from_pool(pool.clone()).with_timezone(&zone_at(9)).await.expect("zone");
    assert!(morning.send_daily_digest().await.expect("send").is_some());
    assert_eq!(posted(&pool).await.len(), 1);
}

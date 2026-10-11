//! Store behaviour against a real Postgres (`#[sqlx::test]` gives each test a
//! fresh migrated database).

use std::time::Duration;

use homestead_domain::*;
use homestead_store::{Store, StoreError, outbox::UNDECODABLE_SINK};
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::macros::date;

async fn events(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("select kind from homestead.events order by id").fetch_all(pool).await.expect("events")
}

async fn bed(store: &Store, name: &str) -> Location {
    store.create_location(NewLocation::named(name, LocationKind::Bed)).await.expect("location")
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
async fn every_enum_literal_is_accepted_by_the_database(pool: PgPool) {
    let store = Store::from_pool(pool);
    for (i, kind) in LocationKind::ALL.iter().enumerate() {
        store
            .create_location(NewLocation::named(format!("loc {i}"), *kind))
            .await
            .unwrap_or_else(|e| panic!("{kind}: {e}"));
    }
    let loc = bed(&store, "enum bed").await;
    let p = store
        .create_planting(NewPlanting {
            location_id: loc.id,
            crop: "Kale".into(),
            variety: String::new(),
            sown_on: date!(2026 - 02 - 01),
            expected_harvest_on: None,
            notes: String::new(),
            seed_lot_id: None,
            ..NewPlanting::at(loc.id, "", date!(2026 - 01 - 01))
        })
        .await
        .expect("planting");
    for s in PlantingStatus::ALL {
        store.set_planting_status(p.id, *s).await.unwrap_or_else(|e| panic!("{s}: {e}"));
    }
    for u in HarvestUnit::ALL {
        store
            .log_harvest(NewHarvest {
                planting_id: p.id,
                harvested_on: date!(2026 - 04 - 01),
                quantity: Decimal::ONE,
                unit: *u,
                notes: String::new(),
            })
            .await
            .unwrap_or_else(|e| panic!("{u}: {e}"));
    }
    for pr in TaskPriority::ALL {
        store.create_task(NewTask { priority: *pr, ..task("t") }).await.unwrap_or_else(|e| panic!("{pr}: {e}"));
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn nothing_is_created_in_the_public_schema(pool: PgPool) {
    let public: Vec<String> = sqlx::query_scalar(
        "select table_name::text from information_schema.tables
         where table_schema = 'public' and table_name <> '_sqlx_migrations'",
    )
    .fetch_all(&pool)
    .await
    .expect("query");
    assert!(public.is_empty(), "tables leaked into the Data-API-exposed schema: {public:?}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn planting_and_harvest_emit_events_and_advance_status(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let loc = bed(&store, "Bed 1").await;
    let p = store
        .create_planting(NewPlanting {
            location_id: loc.id,
            crop: "Tomato".into(),
            variety: "Sungold".into(),
            sown_on: date!(2026 - 03 - 01),
            expected_harvest_on: Some(date!(2026 - 06 - 15)),
            notes: String::new(),
            seed_lot_id: None,
            ..NewPlanting::at(loc.id, "", date!(2026 - 01 - 01))
        })
        .await
        .expect("planting");
    store
        .log_harvest(NewHarvest {
            planting_id: p.id,
            harvested_on: date!(2026 - 06 - 20),
            quantity: Decimal::new(35, 1),
            unit: HarvestUnit::Lb,
            notes: String::new(),
        })
        .await
        .expect("harvest");

    let active = store.list_active_plantings().await.expect("list");
    assert_eq!(active[0].0.status, PlantingStatus::Harvesting);
    assert_eq!(active[0].1, "Bed 1");
    assert_eq!(events(&pool).await, ["planting.created", "harvest.logged"]);

    let payload: serde_json::Value =
        sqlx::query_scalar("select payload from homestead.events where kind = 'harvest.logged'")
            .fetch_one(&pool)
            .await
            .expect("payload");
    let ev = Event::from_parts("harvest.logged", payload).expect("decodes");
    assert_eq!(ev.summary(), "Harvested 3.5 lb of Tomato from Bed 1");

    let totals = store.harvest_totals_since(date!(2026 - 01 - 01)).await.expect("totals");
    assert_eq!(totals, [("Tomato".to_owned(), "lb".to_owned(), Decimal::new(35, 1))]);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn failed_write_emits_no_event(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let missing = uuid::Uuid::new_v4();
    let err =
        store.create_task(NewTask { location_id: Some(missing), ..task("orphan") }).await.expect_err("FK violation");
    assert!(matches!(err, StoreError::NotFound(_)), "{err:?}");
    assert!(events(&pool).await.is_empty(), "an event escaped a rolled-back transaction");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn completing_a_task_twice_announces_once(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let t = store.create_task(task("Water seedlings")).await.expect("task");
    let done = store.complete_task(t.id).await.expect("complete");
    assert_eq!(done.status, TaskStatus::Done);
    assert!(done.completed_at.is_some());
    let again = store.complete_task(t.id).await.expect("second complete is a no-op");
    assert_eq!(again.completed_at, done.completed_at);
    assert_eq!(events(&pool).await, ["task.created", "task.completed"]);
    assert!(store.list_open_tasks().await.expect("open").is_empty());
    assert!(matches!(store.complete_task(uuid::Uuid::new_v4()).await, Err(StoreError::NotFound("task"))));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn open_tasks_sort_by_priority_then_due_date(pool: PgPool) {
    let store = Store::from_pool(pool);
    let mk = |title: &str, priority, due_on| NewTask { priority, due_on, ..task(title) };
    store.create_task(mk("low", TaskPriority::Low, Some(date!(2026 - 01 - 01)))).await.expect("t");
    store.create_task(mk("normal-undated", TaskPriority::Normal, None)).await.expect("t");
    store.create_task(mk("normal-soon", TaskPriority::Normal, Some(date!(2026 - 05 - 01)))).await.expect("t");
    store.create_task(mk("urgent", TaskPriority::Urgent, None)).await.expect("t");
    let order: Vec<String> = store.list_open_tasks().await.expect("list").into_iter().map(|t| t.title).collect();
    assert_eq!(order, ["urgent", "normal-soon", "normal-undated", "low"]);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn linear_issue_id_is_write_once(pool: PgPool) {
    let store = Store::from_pool(pool);
    let t = store.create_task(task("Fix fence")).await.expect("task");
    assert!(store.set_linear_issue_id(t.id, "LIN-1").await.expect("first"));
    assert!(!store.set_linear_issue_id(t.id, "LIN-2").await.expect("second"));
    assert_eq!(store.get_task(t.id).await.expect("get").linear_issue_id.as_deref(), Some("LIN-1"));
}

fn adj(delta: i64, reason: &str) -> StockAdjustment {
    StockAdjustment { delta: Decimal::from(delta), reason: reason.into() }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn stock_cannot_go_negative_and_failed_moves_leave_no_trace(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let feed = store
        .create_inventory_item(NewInventoryItem {
            name: "Layer feed".into(),
            unit: "bag".into(),
            reorder_level: Decimal::ZERO,
        })
        .await
        .expect("item");
    store.adjust_stock(feed.id, adj(3, "delivery")).await.expect("receive");
    let err = store.adjust_stock(feed.id, adj(-4, "feeding")).await.expect_err("overdraw");
    assert!(matches!(err, StoreError::InsufficientStock), "{err:?}");
    let items = store.list_inventory().await.expect("list");
    assert_eq!(items[0].on_hand, Decimal::from(3));
    let moves: i64 =
        sqlx::query_scalar("select count(*) from homestead.stock_movements").fetch_one(&pool).await.expect("n");
    assert_eq!(moves, 1, "the rejected movement must not be recorded");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn low_stock_fires_once_per_downward_crossing(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let feed = store
        .create_inventory_item(NewInventoryItem {
            name: "Layer feed".into(),
            unit: "bag".into(),
            reorder_level: Decimal::from(2),
        })
        .await
        .expect("item");
    store.adjust_stock(feed.id, adj(5, "delivery")).await.expect("5"); // 5, above
    store.adjust_stock(feed.id, adj(-2, "use")).await.expect("3"); // 3, above
    store.adjust_stock(feed.id, adj(-1, "use")).await.expect("2"); // 2, crosses → event
    store.adjust_stock(feed.id, adj(-1, "use")).await.expect("1"); // 1, still low → none
    store.adjust_stock(feed.id, adj(4, "delivery")).await.expect("5"); // back above
    store.adjust_stock(feed.id, adj(-4, "use")).await.expect("1"); // crosses again → event
    let low: Vec<serde_json::Value> =
        sqlx::query_scalar("select payload from homestead.events where kind = 'inventory.low_stock' order by id")
            .fetch_all(&pool)
            .await
            .expect("events");
    let previous: Vec<Decimal> = low
        .into_iter()
        .map(|p| match Event::from_parts("inventory.low_stock", p).expect("decodes") {
            Event::LowStock { previous_on_hand, .. } => previous_on_hand,
            other => panic!("wrong variant {other:?}"),
        })
        .collect();
    assert_eq!(previous, [Decimal::from(3), Decimal::from(5)]);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn concurrent_consumption_never_oversells(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let eggs = store
        .create_inventory_item(NewInventoryItem {
            name: "Egg cartons".into(),
            unit: "each".into(),
            reorder_level: Decimal::ZERO,
        })
        .await
        .expect("item");
    store.adjust_stock(eggs.id, adj(10, "delivery")).await.expect("stock");
    let attempts = (0..25).map(|_| {
        let store = store.clone();
        tokio::spawn(async move { store.adjust_stock(eggs.id, adj(-1, "sale")).await })
    });
    let mut ok = 0;
    for h in attempts {
        match h.await.expect("join") {
            Ok(_) => ok += 1,
            Err(StoreError::InsufficientStock) => {}
            Err(e) => panic!("unexpected error {e:?}"),
        }
    }
    assert_eq!(ok, 10, "exactly the stocked quantity can be consumed");
    let on_hand: Decimal =
        sqlx::query_scalar("select on_hand from homestead.inventory_items").fetch_one(&pool).await.expect("q");
    let ledger: Decimal =
        sqlx::query_scalar("select sum(delta) from homestead.stock_movements").fetch_one(&pool).await.expect("q");
    assert_eq!(on_hand, Decimal::ZERO);
    assert_eq!(on_hand, ledger, "running total must equal the movement ledger");
}

// ── Outbox mechanics ─────────────────────────────────────────────────────────

fn route_all(_: &Event) -> Vec<&'static str> {
    vec!["a", "b"]
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn fan_out_routes_each_event_once(pool: PgPool) {
    let store = Store::from_pool(pool);
    store.create_task(task("one")).await.expect("t");
    store.create_task(task("two")).await.expect("t");
    let only_a = |_: &Event| vec!["a"];
    assert_eq!(store.fan_out(100, &only_a).await.expect("fan"), 2);
    assert_eq!(store.fan_out(100, &route_all).await.expect("fan"), 0, "already fanned events are not re-routed");
    let stats = store.bus_stats().await.expect("stats");
    assert_eq!((stats.unfanned, stats.pending), (0, 2));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn concurrent_claims_are_disjoint_and_leases_expire(pool: PgPool) {
    let store = Store::from_pool(pool);
    for i in 0..10 {
        store.create_task(task(&format!("t{i}"))).await.expect("t");
    }
    store.fan_out(100, &route_all).await.expect("fan"); // 20 deliveries
    let lease = Duration::from_secs(60);
    let (x, y) = tokio::join!(store.claim_due(15, lease), store.claim_due(15, lease));
    let (x, y) = (x.expect("x"), y.expect("y"));
    let mut all: Vec<_> = x.iter().chain(&y).map(|c| (c.event_id, c.sink.clone())).collect();
    let n = all.len();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), n, "a delivery was leased to two workers");
    assert_eq!(n, 20);
    assert!(store.claim_due(100, lease).await.expect("again").is_empty(), "leased work must not be re-claimed");

    // A zero-length lease models a worker that died: its work comes back.
    let store2 = store.clone();
    sqlx::query("update homestead.deliveries set next_attempt_at = now() - interval '1 second'")
        .execute(store2.pool())
        .await
        .expect("expire");
    assert_eq!(store.claim_due(100, lease).await.expect("reclaim").len(), 20);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn failures_retry_then_dead_letter_and_can_be_revived(pool: PgPool) {
    let store = Store::from_pool(pool);
    store.create_task(task("t")).await.expect("t");
    store.fan_out(10, &|_: &Event| vec!["discord"]).await.expect("fan");
    let c = store.claim_due(10, Duration::from_secs(60)).await.expect("claim").remove(0);

    store.mark_failed(c.event_id, "discord", "503", Some(Duration::ZERO)).await.expect("retryable");
    let again = store.claim_due(10, Duration::from_secs(60)).await.expect("claim");
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].attempts, 1);

    store.mark_failed(c.event_id, "discord", "400 bad webhook", None).await.expect("dead");
    assert!(store.claim_due(10, Duration::ZERO).await.expect("claim").is_empty(), "dead letters are not retried");
    let troubled = store.troubled_deliveries(10).await.expect("troubled");
    assert_eq!(troubled[0].status, "dead");
    assert_eq!(troubled[0].attempts, 2);
    assert_eq!(troubled[0].last_error.as_deref(), Some("400 bad webhook"));

    assert!(store.retry_dead(c.event_id, "discord").await.expect("revive"));
    let revived = store.claim_due(10, Duration::from_secs(60)).await.expect("claim");
    assert_eq!(revived[0].attempts, 0, "revival grants a fresh attempt budget");
    store.mark_delivered(c.event_id, "discord").await.expect("ok");
    let stats = store.bus_stats().await.expect("stats");
    assert_eq!((stats.pending, stats.dead, stats.delivered), (0, 0, 1));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn undecodable_events_are_dead_lettered_not_dropped(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    sqlx::query("insert into homestead.events (kind, payload) values ('crop.teleported', '{}')")
        .execute(&pool)
        .await
        .expect("insert");
    assert_eq!(store.fan_out(10, &route_all).await.expect("fan"), 1);
    let troubled = store.troubled_deliveries(10).await.expect("troubled");
    assert_eq!(troubled.len(), 1);
    assert_eq!(troubled[0].sink, UNDECODABLE_SINK);
    assert!(
        !store.retry_dead(troubled[0].event_id, UNDECODABLE_SINK).await.expect("retry"),
        "cannot revive an undecodable event"
    );
}

// ── Releases ─────────────────────────────────────────────────────────────────

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn release_completion_announces_once_without_the_email_or_link(pool: PgPool) {
    use homestead_store::releases::{Outcome, SentRelease};
    let store = Store::from_pool(pool.clone());
    let id = uuid::Uuid::new_v4();
    let r = store
        .record_release_sent(&SentRelease {
            id,
            release_key: "photo-release",
            title: "Photo Release",
            signer_name: "  Jane Doe ",
            signer_email: " Jane@Example.org ",
            envelope_id: "envelope_1",
            signing_url: Some("https://sign.example/sign/tok_secret"),
        })
        .await
        .expect("record");
    assert_eq!(
        (r.signer_name.as_str(), r.signer_email.as_str(), r.status.as_str()),
        ("Jane Doe", "jane@example.org", "sent")
    );
    assert_eq!(r.signing_url.as_deref(), Some("https://sign.example/sign/tok_secret"));

    let done = store.complete_release(id, Outcome::Signed).await.expect("complete").expect("was pending");
    assert_eq!(done.status, "signed");
    assert!(done.completed_at.is_some());
    assert_eq!(done.signing_url, None, "a settled request no longer holds a working link");
    assert!(store.complete_release(id, Outcome::Signed).await.expect("repeat").is_none(), "webhook retry is a no-op");
    assert!(store.complete_release(uuid::Uuid::new_v4(), Outcome::Signed).await.expect("unknown").is_none());

    let payloads: Vec<(String, serde_json::Value)> =
        sqlx::query_as("select kind, payload from homestead.events order by id")
            .fetch_all(&pool)
            .await
            .expect("events");
    assert_eq!(payloads.len(), 1, "exactly one announcement");
    assert_eq!(payloads[0].0, "release.signed");
    assert!(!payloads[0].1.to_string().contains("example.org"), "emails never enter the outbox");
    assert!(!payloads[0].1.to_string().contains("tok_secret"), "nor signing links");
    let ev = Event::from_parts(&payloads[0].0, payloads[0].1.clone()).expect("decodes");
    assert_eq!(ev.summary(), "Jane Doe signed the Photo Release");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_signed_welcome_agreement_onboards_its_discord_member_once(pool: PgPool) {
    use homestead_domain::onboarding::OnboardingRole;
    use homestead_store::releases::{Joiner, Outcome, SentRelease};
    let store = Store::from_pool(pool.clone());
    let sent = |id, envelope_id| SentRelease {
        id,
        release_key: "onboarding",
        title: "Welcome Agreement",
        signer_name: "Jane Doe",
        signer_email: "jane@example.org",
        envelope_id,
        signing_url: Some("https://sign.example/sign/tok_secret"),
    };
    let jane = Joiner { discord_user_id: "111111111111", discord_username: "jane", role: OnboardingRole::Volunteer };
    let events = || async {
        sqlx::query_as::<_, (String, serde_json::Value)>(
            "select kind, payload from homestead.events where kind = 'member.onboarded' order by id",
        )
        .fetch_all(&pool)
        .await
        .expect("events")
    };

    // Asked twice (say, two taps on /onboard): two agreements out.
    let (first, second) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    store.record_onboarding_sent(&sent(first, "env_a"), jane).await.expect("first");
    store.record_onboarding_sent(&sent(second, "env_b"), jane).await.expect("second");
    let o = store.onboarding("111111111111").await.expect("q").expect("row");
    assert_eq!((o.role, o.completed_at), (OnboardingRole::Volunteer, None));
    assert_eq!(o.latest.as_ref().map(|r| r.id), Some(second), "shows the newest agreement");

    // Nobody who hasn't signed can claim a role.
    assert!(store.choose_onboarded_role(jane).await.expect("q").is_none());
    // Declining isn't joining.
    let declined = uuid::Uuid::new_v4();
    store.record_onboarding_sent(&sent(declined, "env_d"), jane).await.expect("third");
    store.complete_release(declined, Outcome::Declined).await.expect("decline");
    assert!(events().await.is_empty());

    // She signs the OLDER one: that still counts.
    store.complete_release(first, Outcome::Signed).await.expect("sign").expect("pending");
    let o = store.onboarding("111111111111").await.expect("q").expect("row");
    assert!(o.completed_at.is_some());
    let got = events().await;
    assert_eq!(got.len(), 1);
    let ev = Event::from_parts(&got[0].0, got[0].1.clone()).expect("decodes");
    assert_eq!(
        ev,
        Event::MemberOnboarded {
            discord_user_id: "111111111111".into(),
            name: "Jane Doe".into(),
            role: OnboardingRole::Volunteer
        }
    );
    assert!(!got[0].1.to_string().contains("example.org"), "emails never enter the outbox");

    // A webhook retry, or signing the other agreement too, grants nothing new.
    assert!(store.complete_release(first, Outcome::Signed).await.expect("retry").is_none());
    store.complete_release(second, Outcome::Signed).await.expect("sign").expect("pending");
    assert_eq!(events().await.len(), 1);

    // Signed members pick a role without signing again.
    let community = Joiner { role: OnboardingRole::Community, ..jane };
    let o = store.choose_onboarded_role(community).await.expect("q").expect("signed");
    assert_eq!(o.role, OnboardingRole::Community);
    assert_eq!(events().await.len(), 2);

    // Releases sent from the app, for no Discord member, onboard nobody.
    let plain = uuid::Uuid::new_v4();
    store.record_release_sent(&sent(plain, "env_p")).await.expect("plain");
    store.complete_release(plain, Outcome::Signed).await.expect("sign");
    assert_eq!(events().await.len(), 2);
    assert_eq!(store.onboardings().await.expect("list").len(), 1);

    // Discord ids are snowflakes.
    let bad = Joiner { discord_user_id: "<@1>", ..jane };
    let err = store.record_onboarding_sent(&sent(uuid::Uuid::new_v4(), "env_x"), bad).await.expect_err("bad id");
    assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn release_requests_reject_bad_emails(pool: PgPool) {
    use homestead_store::releases::SentRelease;
    let store = Store::from_pool(pool);
    let err = store
        .record_release_sent(&SentRelease {
            id: uuid::Uuid::new_v4(),
            release_key: "nda",
            title: "NDA",
            signer_name: "Omar",
            signer_email: "not-an-email",
            envelope_id: "env_2",
            signing_url: None,
        })
        .await
        .expect_err("invalid email");
    assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
}

// ── Grant outreach ──────────────────────────────────────────────────────────

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn grant_campaigns_are_frozen_approved_and_leased_once(pool: PgPool) {
    use std::collections::BTreeMap;

    use homestead_store::grants::NewGrantMessage;

    let store = Store::from_pool(pool.clone());
    let template =
        store.create_grant_template("Food systems", "Hello {{organization}}", "Proposal").await.expect("template");
    let recipient = store
        .create_grant_recipient(
            "Valley Fund",
            "Avery",
            " Grants@Example.org ",
            "Resilience",
            "foundation website",
            &BTreeMap::new(),
        )
        .await
        .expect("recipient");
    assert_eq!(recipient.email, "grants@example.org");
    let message_id = uuid::Uuid::new_v4();
    let campaign = store
        .create_grant_campaign(
            "Spring outreach",
            template.id,
            &[NewGrantMessage {
                id: message_id,
                recipient_id: recipient.id,
                recipient_email: recipient.email,
                recipient_name: recipient.contact_name,
                organization: recipient.organization,
                subject: "Frozen subject".into(),
                body_text: "Frozen body".into(),
                body_html: "<p>Frozen body</p>".into(),
            }],
        )
        .await
        .expect("campaign");
    assert!(store.claim_grant_messages(campaign, 25).await.expect("unapproved").is_empty());
    store.approve_grant_campaign(campaign).await.expect("approve");
    let claimed = store.claim_grant_messages(campaign, 25).await.expect("claim");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].subject, "Frozen subject");
    assert!(store.claim_grant_messages(campaign, 25).await.expect("double claim").is_empty());
    store.settle_grant_message(message_id, Ok("provider-1")).await.expect("settle");
    let done = store.get_grant_campaign(campaign).await.expect("campaign");
    assert_eq!((done.status.as_str(), done.sent, done.total), ("completed", 1, 1));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn suppressing_a_recipient_cancels_unsent_grant_messages(pool: PgPool) {
    use std::collections::BTreeMap;

    use homestead_store::grants::NewGrantMessage;

    let store = Store::from_pool(pool);
    let template = store.create_grant_template("T", "S", "B").await.expect("template");
    let recipient = store
        .create_grant_recipient("Fund", "", "fund@example.org", "", "public directory", &BTreeMap::new())
        .await
        .expect("recipient");
    let campaign = store
        .create_grant_campaign(
            "C",
            template.id,
            &[NewGrantMessage {
                id: uuid::Uuid::new_v4(),
                recipient_id: recipient.id,
                recipient_email: recipient.email,
                recipient_name: recipient.contact_name,
                organization: recipient.organization,
                subject: "S".into(),
                body_text: "B".into(),
                body_html: "<p>B</p>".into(),
            }],
        )
        .await
        .expect("campaign");
    store.approve_grant_campaign(campaign).await.expect("approve");
    store.suppress_grant_recipient(recipient.id).await.expect("suppress");
    assert!(store.claim_grant_messages(campaign, 25).await.expect("claim").is_empty());
    assert_eq!(store.grant_campaign_messages(campaign).await.expect("messages")[0].status, "suppressed");
    assert_eq!(store.get_grant_campaign(campaign).await.expect("campaign").status, "completed");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn grant_recipient_import_is_atomic(pool: PgPool) {
    use std::collections::BTreeMap;

    use homestead_store::grants::NewGrantRecipient;

    let store = Store::from_pool(pool.clone());
    store
        .create_grant_recipient("Existing", "", "same@example.org", "", "directory", &BTreeMap::new())
        .await
        .expect("existing");
    let row = |organization: &str, email: &str| NewGrantRecipient {
        organization: organization.into(),
        contact_name: String::new(),
        email: email.into(),
        program_name: String::new(),
        source: "directory".into(),
        custom_fields: BTreeMap::new(),
    };
    store
        .import_grant_recipients(&[row("Would insert", "new@example.org"), row("Duplicate", "same@example.org")])
        .await
        .expect_err("duplicate rolls back every row");
    let count: i64 =
        sqlx::query_scalar("select count(*) from homestead.grant_recipients").fetch_one(&pool).await.expect("count");
    assert_eq!(count, 1);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_bed_sits_in_a_garden_and_remembers_what_grew(pool: PgPool) {
    let store = Store::from_pool(pool);
    let garden = store.create_location(NewLocation::named("North garden", LocationKind::Garden)).await.expect("garden");
    let bed = store
        .create_location(NewLocation {
            parent_id: Some(garden.id),
            area_sqft: Some(Decimal::from(32)),
            ..NewLocation::named("A3", LocationKind::Bed)
        })
        .await
        .expect("bed");
    store
        .create_planting(NewPlanting {
            location_id: bed.id,
            crop: "Lettuce".into(),
            variety: String::new(),
            sown_on: date!(2026 - 03 - 01),
            expected_harvest_on: None,
            notes: String::new(),
            seed_lot_id: None,
            ..NewPlanting::at(bed.id, "", date!(2026 - 01 - 01))
        })
        .await
        .expect("planting");
    let history = store.location_history(bed.id).await.expect("history");
    assert_eq!(history.plantings[0].0.crop, "Lettuce");
    let looped = store
        .create_location(NewLocation { parent_id: Some(bed.id), ..NewLocation::named("nope", LocationKind::Row) })
        .await
        .expect("row");
    // Point the garden at the row that sits under the bed under the garden.
    let err = match sqlx::query("update homestead.locations set parent_id = $1 where id = $2")
        .bind(looped.id)
        .bind(garden.id)
        .execute(store.pool())
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("a parent cycle was stored"),
    };
    assert!(err.to_string().contains("cycle") || err.to_string().contains("loop"), "{err}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn saved_letter_templates_keep_their_copy_and_design(pool: PgPool) {
    let store = Store::from_pool(pool);
    let saved = store
        .create_grant_template_with_design("Sponsor", "Original subject", "Original body", "ink")
        .await
        .expect("save");
    assert_eq!(saved.design, "ink");
    let edited = store
        .update_grant_template(saved.id, "Sponsor revised", "New subject", "New body", "meadow")
        .await
        .expect("edit");
    assert_eq!(
        (edited.name.as_str(), edited.subject_template.as_str(), edited.body_template.as_str(), edited.design.as_str()),
        ("Sponsor revised", "New subject", "New body", "meadow")
    );
    assert_eq!(store.get_grant_template(saved.id).await.expect("reload").design, "meadow");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn someone_who_joins_twice_can_connect_the_same_discord_account(pool: PgPool) {
    use homestead_domain::onboarding::OnboardingRole;
    use homestead_store::releases::{Outcome, SentRelease};
    let store = Store::from_pool(pool);
    let mut joins = Vec::new();
    for (n, (hash, envelope)) in [("a".repeat(64), "env-1"), ("b".repeat(64), "env-2")].iter().enumerate() {
        let join =
            store.start_join(hash, "Sam Sprout", "sam@example.org", OnboardingRole::Volunteer).await.expect("join");
        let join = join.expect("a new token starts a join").id;
        let id = uuid::Uuid::new_v4();
        let row = SentRelease {
            id,
            release_key: "onboarding",
            title: "Welcome Agreement",
            signer_name: "Sam Sprout",
            signer_email: "sam@example.org",
            envelope_id: envelope,
            signing_url: Some("https://sign.example/sign/tok"),
        };
        store.record_join_sent(join, &row).await.unwrap_or_else(|e| panic!("sent {n}: {e}"));
        store.complete_release(id, Outcome::Signed).await.expect("signed");
        joins.push(join);
    }
    for join in joins {
        store.claim_join_discord(join, "777777777", "sam.grows").await.expect("the same person, signed twice");
    }
}

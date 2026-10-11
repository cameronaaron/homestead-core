//! Sinks end-to-end: real store + dispatcher, mocked outside HTTP APIs.

use std::sync::Arc;

use homestead_bus::{Dispatcher, DispatcherConfig, RetryPolicy, Sink};
use homestead_domain::{NewInventoryItem, NewTask, StockAdjustment, TaskPriority};
use homestead_integrations::linear::{LinearSink, issue_id_for, low_stock_issue_key, task_issue_key};
use homestead_integrations::{discord::WebhookSink, http_client};
use homestead_store::Store;
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::time::Duration;
use wiremock::matchers::{body_partial_json, body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg() -> DispatcherConfig {
    DispatcherConfig {
        retry: RetryPolicy { max_attempts: 3, base: Duration::ZERO, cap: Duration::ZERO },
        ..DispatcherConfig::default()
    }
}

fn task(title: &str, mirror: bool) -> NewTask {
    NewTask {
        title: title.into(),
        details: "Use the blue hose".into(),
        location_id: None,
        due_on: None,
        priority: TaskPriority::Urgent,
        mirror_to_linear: mirror,
        assigned_to: None,
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn discord_webhook_posts_summary_without_mentions(pool: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/hook"))
        .and(query_param("wait", "true"))
        .and(body_partial_json(serde_json::json!({
            "content": "📝 New urgent task: @everyone water the goats",
            "allowed_mentions": { "parse": [] }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "1"})))
        .expect(1)
        .mount(&server)
        .await;
    let store = Store::from_pool(pool);
    let sink: Arc<dyn Sink> = Arc::new(WebhookSink::new(http_client(), format!("{}/hook", server.uri())));
    let d = Dispatcher::new(store.clone(), vec![sink], cfg());
    store.create_task(task("@everyone water the goats", false)).await.expect("task");
    assert_eq!(d.tick().await.expect("tick").delivered, 1);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn discord_deleted_webhook_dead_letters(pool: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(404)).expect(1).mount(&server).await;
    let store = Store::from_pool(pool);
    let sink: Arc<dyn Sink> = Arc::new(WebhookSink::new(http_client(), format!("{}/hook", server.uri())));
    let d = Dispatcher::new(store.clone(), vec![sink], cfg());
    store.create_task(task("t", false)).await.expect("task");
    d.tick().await.expect("tick");
    d.tick().await.expect("tick");
    assert_eq!(store.bus_stats().await.expect("stats").dead, 1);
}

/// A grant deadline (a Linear issue in "Grant writing") and a local one.
async fn two_deadlines(store: &Store) {
    use homestead_store::linear::PulledIssue;
    let grant = PulledIssue {
        id: "00000000-0000-4000-8000-000000000046".into(),
        identifier: "SUN-46".into(),
        url: "https://linear.app/x/issue/SUN-46".into(),
        title: "Submit the grant application".into(),
        description: String::new(),
        priority: Some(TaskPriority::Urgent),
        due_on: Some(time::macros::date!(2026 - 10 - 08)),
        state_type: "unstarted".into(),
        state_name: "Todo".into(),
        assignee: None,
        project: "Grant writing".into(),
        archived: false,
        updated_at: time::macros::datetime!(2026-09-29 20:00 UTC),
        homestead_task: None,
    };
    store.apply_linear_issues(&[grant], time::macros::datetime!(2026-09-29 21:00 UTC)).await.expect("pull");
    let local = NewTask { due_on: Some(time::macros::date!(2026 - 10 - 02)), ..task("Order seed garlic", false) };
    store.create_task(local).await.expect("task");
    store.send_deadline_reminders_as_of(time::macros::date!(2026 - 10 - 01)).await.expect("remind");
}

fn grant_routes() -> std::collections::HashMap<String, String> {
    [("Grant writing".to_owned(), "grant-deadlines".to_owned())].into()
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn grant_deadlines_go_to_their_channel_and_the_rest_to_the_feed(pool: PgPool) {
    use homestead_integrations::discord::ChannelSink;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/guilds/g/channels"))
        .and(header("Authorization", "Bot t"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": "11", "name": "grant-deadlines", "type": 2 },
            { "id": "77", "name": "grant-deadlines", "type": 0 },
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/channels/77/messages"))
        .and(body_string_contains("Grant writing: deadlines"))
        .and(body_string_contains("[SUN-46](<https://linear.app/x/issue/SUN-46>)"))
        .and(body_partial_json(serde_json::json!({ "allowed_mentions": { "parse": [] } })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "1"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/hook"))
        .and(body_string_contains("Deadlines coming up"))
        .and(body_string_contains("Order seed garlic: due **tomorrow**"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "2"})))
        .expect(1)
        .mount(&server)
        .await;
    let store = Store::from_pool(pool);
    let feed =
        WebhookSink::new(http_client(), format!("{}/hook", server.uri())).except_projects(grant_routes().into_keys());
    let bot = ChannelSink::new(http_client(), Some(server.uri()), "t".into(), "g".into(), grant_routes());
    // Routed to reminders only, as homestead.toml does, so creating the
    // local task (task.created) posts nothing here.
    let feed = Arc::new(homestead_bus::Routed::new(Arc::new(feed), ["task.deadlines".to_owned()]));
    let d = Dispatcher::new(store.clone(), vec![feed, Arc::new(bot)], cfg());
    two_deadlines(&store).await;
    assert_eq!(d.tick().await.expect("tick").delivered, 2, "each post once, in one place");
    let stats = store.bus_stats().await.expect("stats");
    assert_eq!((stats.pending, stats.dead), (0, 0), "nothing else was attempted");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_missing_deadline_channel_dead_letters_with_the_fix(pool: PgPool) {
    use homestead_integrations::discord::ChannelSink;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{ "id": "5", "name": "general", "type": 0 }])),
        )
        .mount(&server)
        .await;
    let store = Store::from_pool(pool);
    let bot = ChannelSink::new(http_client(), Some(server.uri()), "t".into(), "g".into(), grant_routes());
    let d = Dispatcher::new(store.clone(), vec![Arc::new(bot)], cfg());
    two_deadlines(&store).await;
    d.tick().await.expect("tick");
    assert_eq!(store.bus_stats().await.expect("stats").dead, 1, "a missing channel won't appear by retrying");
    let dead = store.troubled_deliveries(10).await.expect("dead");
    assert!(dead[0].last_error.as_deref().is_some_and(|e| e.contains("no #grant-deadlines channel")));
}

// ── Linear ──────────────────────────────────────────────────────────────────
//
// The fake answers by operation: the sink looks an issue up by the id it
// derived (`issue(id:`), creates it (`issueCreate`), or completes it
// (`issueUpdate`).

const NOT_FOUND: &str =
    r#"{"errors":[{"message":"Entity not found: Issue","extensions":{"code":"INPUT_ERROR"}}],"data":null}"#;

fn issue_json(id: &str, state: &str) -> serde_json::Value {
    serde_json::json!({ "id": id, "identifier": "SUN-9", "url": "u", "state": { "type": state } })
}

fn found(id: &str, state: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": { "issue": issue_json(id, state) } }))
}

fn created(id: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "data": { "issueCreate": { "success": true, "issue": issue_json(id, "backlog") } }
    }))
}

fn not_found() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(NOT_FOUND)
}

fn lookups() -> wiremock::MockBuilder {
    Mock::given(method("POST")).and(body_string_contains("issue(id:"))
}

fn creates() -> wiremock::MockBuilder {
    Mock::given(method("POST")).and(body_string_contains("issueCreate"))
}

fn updates() -> wiremock::MockBuilder {
    Mock::given(method("POST")).and(body_string_contains("issueUpdate"))
}

fn linear(server: &MockServer, store: &Store) -> Arc<LinearSink> {
    Arc::new(
        LinearSink::new(
            http_client(),
            format!("{}/graphql", server.uri()),
            "lin_test_key".into(),
            "team-1".into(),
            store.clone(),
        )
        .with_done_state("state-done".into()),
    )
}

fn task_issue(task: uuid::Uuid) -> String {
    issue_id_for(&task_issue_key(task)).to_string()
}

async fn event_id(pool: &PgPool, kind: &str) -> i64 {
    sqlx::query_scalar("select id from homestead.events where kind = $1 order by id limit 1")
        .bind(kind)
        .fetch_one(pool)
        .await
        .expect("event id")
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn linear_mirrors_opted_in_tasks_once_under_the_tasks_own_id(pool: PgPool) {
    let server = MockServer::start().await;
    let store = Store::from_pool(pool.clone());
    let mirrored = store.create_task(task("Fix coop door", true)).await.expect("t");
    store.create_task(task("Not for Linear", false)).await.expect("t");
    let id = task_issue(mirrored.id);

    lookups().respond_with(not_found()).expect(1).mount(&server).await;
    creates()
        .and(path("/graphql"))
        .and(header("Authorization", "lin_test_key"))
        .and(body_partial_json(serde_json::json!({
            "variables": { "input": { "id": id, "teamId": "team-1", "title": "Fix coop door", "priority": 1 } }
        })))
        .respond_with(created(&id))
        .expect(1)
        .mount(&server)
        .await;
    let sink = linear(&server, &store);
    let d = Dispatcher::new(store.clone(), vec![sink.clone() as Arc<dyn Sink>], cfg());

    let tick = d.tick().await.expect("tick");
    assert_eq!((tick.delivered, tick.failed), (1, 0), "only the opted-in task is routed to Linear");
    assert_eq!(store.get_task(mirrored.id).await.expect("get").linear_issue_id, Some(id));

    // At-least-once redelivery of the same event: the recorded id stops it
    // before any call (the mocks' `.expect(1)` are checked on drop).
    let ev = homestead_domain::Event::TaskCreated { task: mirrored };
    sink.deliver(event_id(&pool, "task.created").await, &ev).await.expect("redelivery is a no-op");
}

/// The duplicate window this design closes: Linear accepted the issue, then
/// Homestead died before recording it. The retry must find it, not recreate.
#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn linear_retry_after_a_lost_write_back_finds_the_issue_instead_of_duplicating(pool: PgPool) {
    let server = MockServer::start().await;
    let store = Store::from_pool(pool.clone());
    let t = store.create_task(task("Fix coop door", true)).await.expect("t");
    let id = task_issue(t.id);
    lookups()
        .and(body_partial_json(serde_json::json!({ "variables": { "id": id } })))
        .respond_with(found(&id, "backlog"))
        .expect(1)
        .mount(&server)
        .await;
    creates().respond_with(created("must-not-happen")).expect(0).mount(&server).await;
    let d = Dispatcher::new(store.clone(), vec![linear(&server, &store) as Arc<dyn Sink>], cfg());

    assert_eq!(d.tick().await.expect("tick").delivered, 1);
    assert_eq!(store.get_task(t.id).await.expect("get").linear_issue_id, Some(id), "written back on the retry");
}

/// Two creates with the same id: Linear rejects the second. If the issue is
/// there afterwards, the delivery succeeded; it is not dead-lettered.
#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn linear_create_rejected_because_the_issue_exists_counts_as_delivered(pool: PgPool) {
    let server = MockServer::start().await;
    let store = Store::from_pool(pool.clone());
    let t = store.create_task(task("Fix coop door", true)).await.expect("t");
    let id = task_issue(t.id);
    lookups().respond_with(not_found()).up_to_n_times(1).expect(1).mount(&server).await;
    lookups().respond_with(found(&id, "backlog")).expect(1).mount(&server).await;
    creates()
        .respond_with(ResponseTemplate::new(400).set_body_string(
            // Verbatim from the live API (2026-09-28) for a second create with the same id.
            r#"{"errors":[{"message":"conflict on insert of Issue","extensions":{"type":"invalid input","code":"INPUT_ERROR","statusCode":400,"userError":true}}],"data":null}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let d = Dispatcher::new(store.clone(), vec![linear(&server, &store) as Arc<dyn Sink>], cfg());
    assert_eq!(d.tick().await.expect("tick").delivered, 1);
    assert_eq!(store.get_task(t.id).await.expect("get").linear_issue_id, Some(id));
}

/// A create rejected for a real reason (and no issue afterwards) is still a
/// permanent failure: the look-up must not paper over it.
#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn linear_rejected_create_with_no_issue_dead_letters(pool: PgPool) {
    let server = MockServer::start().await;
    let store = Store::from_pool(pool.clone());
    store.create_task(task("Fix coop door", true)).await.expect("t");
    lookups().respond_with(not_found()).expect(2).mount(&server).await;
    creates()
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"errors":[{"message":"Authentication required","extensions":{"code":"AUTHENTICATION_ERROR"}}]}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let d = Dispatcher::new(store.clone(), vec![linear(&server, &store) as Arc<dyn Sink>], cfg());
    d.tick().await.expect("tick");
    assert_eq!(store.bus_stats().await.expect("stats").dead, 1);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn linear_completing_a_task_moves_its_issue_to_done(pool: PgPool) {
    let server = MockServer::start().await;
    let store = Store::from_pool(pool.clone());
    let t = store.create_task(task("Fix coop door", true)).await.expect("t");
    let id = task_issue(t.id);
    lookups().respond_with(not_found()).up_to_n_times(1).expect(1).mount(&server).await;
    creates().respond_with(created(&id)).expect(1).mount(&server).await;
    let d = Dispatcher::new(store.clone(), vec![linear(&server, &store) as Arc<dyn Sink>], cfg());
    assert_eq!(d.tick().await.expect("tick").delivered, 1);

    lookups().respond_with(found(&id, "started")).expect(1).mount(&server).await;
    updates()
        .and(body_partial_json(serde_json::json!({ "variables": { "id": id, "stateId": "state-done" } })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "issueUpdate": { "success": true, "issue": issue_json(&id, "completed") } }
        })))
        .expect(1)
        .mount(&server)
        .await;
    store.complete_task(t.id).await.expect("done");
    assert_eq!(d.tick().await.expect("tick").delivered, 1);
}

/// Someone closed the issue in Linear first (here: Canceled). Finishing the
/// task must not reopen it as Done.
#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn linear_completion_leaves_an_issue_a_person_closed(pool: PgPool) {
    let server = MockServer::start().await;
    let store = Store::from_pool(pool.clone());
    let t = store.create_task(task("Fix coop door", true)).await.expect("t");
    let id = task_issue(t.id);
    store.set_linear_issue_id(t.id, &id).await.expect("recorded");
    lookups().respond_with(found(&id, "canceled")).mount(&server).await;
    updates().respond_with(ResponseTemplate::new(500)).expect(0).mount(&server).await;
    let d = Dispatcher::new(store.clone(), vec![linear(&server, &store) as Arc<dyn Sink>], cfg());
    store.complete_task(t.id).await.expect("done");
    let tick = d.tick().await.expect("tick");
    assert_eq!((tick.delivered, tick.failed), (2, 0), "created (already recorded) and completed, both no-ops");
}

/// Completion can overtake creation (both queue at once, or creation is
/// retrying): it waits for the issue rather than dying or creating one.
#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn linear_completion_before_the_issue_exists_retries(pool: PgPool) {
    let server = MockServer::start().await;
    let store = Store::from_pool(pool.clone());
    let t = store.create_task(task("Fix coop door", false)).await.expect("t");
    // Mirrored, but its task.created was never routed to Linear.
    sqlx::query("update homestead.tasks set mirror_to_linear = true where id = $1")
        .bind(t.id)
        .execute(&pool)
        .await
        .expect("mirror");
    lookups().respond_with(not_found()).mount(&server).await;
    creates().respond_with(created("must-not-happen")).expect(0).mount(&server).await;
    let sink = linear(&server, &store);
    let done = store.complete_task(t.id).await.expect("done");
    let err = sink.deliver(1, &homestead_domain::Event::TaskCompleted { task: done }).await.expect_err("not yet");
    assert!(matches!(err, homestead_bus::DeliveryError::Retryable(_)), "{err:?}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn linear_low_stock_retries_a_rate_limit_then_records_its_issue_once(pool: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "errors": [{ "message": "rate limited", "extensions": { "code": "RATELIMITED" } }]
        })))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    lookups().respond_with(not_found()).expect(1).mount(&server).await;
    let store = Store::from_pool(pool.clone());
    let feed = store
        .create_inventory_item(NewInventoryItem {
            name: "Layer feed".into(),
            unit: "bag".into(),
            reorder_level: Decimal::from(2),
        })
        .await
        .expect("item");
    store
        .adjust_stock(feed.id, StockAdjustment { delta: Decimal::from(5), reason: "delivery".into() })
        .await
        .expect("adj");
    store
        .adjust_stock(feed.id, StockAdjustment { delta: Decimal::from(-4), reason: "feeding".into() })
        .await
        .expect("adj");
    let ev_id = event_id(&pool, "inventory.low_stock").await;
    let id = issue_id_for(&low_stock_issue_key(feed.id, ev_id)).to_string();
    creates()
        .and(body_partial_json(
            serde_json::json!({ "variables": { "input": { "id": id, "title": "Reorder Layer feed" } } }),
        ))
        .respond_with(created(&id))
        .expect(1)
        .mount(&server)
        .await;
    let sink = linear(&server, &store);
    let d = Dispatcher::new(store.clone(), vec![sink.clone() as Arc<dyn Sink>], cfg());

    assert_eq!(d.tick().await.expect("tick").failed, 1, "rate limited on first attempt");
    assert_eq!(d.tick().await.expect("tick").delivered, 1, "retried and succeeded");
    let stats = store.bus_stats().await.expect("stats");
    assert_eq!((stats.dead, stats.delivered), (0, 1));
    assert_eq!(store.low_stock_issue(ev_id).await.expect("read"), Some(id), "written back");

    // Redelivered: the record stops it before any call.
    let item = store.get_inventory_item(feed.id).await.expect("item");
    let ev = homestead_domain::Event::LowStock { item, previous_on_hand: Decimal::from(5) };
    sink.deliver(ev_id, &ev).await.expect("redelivery is a no-op");
}

/// A signed welcome agreement for a Discord member becomes their role, through
/// the outbox: a real store event, a real dispatcher, Discord mocked.
#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn signing_the_welcome_agreement_grants_the_role_asked_for(pool: PgPool) {
    use homestead_domain::onboarding::OnboardingRole;
    use homestead_integrations::discord::RoleSink;
    use homestead_store::releases::{Joiner, Outcome, SentRelease};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/guilds/g/roles"))
        .and(header("Authorization", "Bot t"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": "1", "name": "Community" }, { "id": "2", "name": "Volunteer" }, { "id": "3", "name": "Crew" },
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/guilds/g/members/111111111111/roles/2"))
        .and(header("Authorization", "Bot t"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let store = Store::from_pool(pool);
    let id = uuid::Uuid::new_v4();
    let sent = SentRelease {
        id,
        release_key: "onboarding",
        title: "Welcome Agreement",
        signer_name: "Jane Doe",
        signer_email: "jane@example.org",
        envelope_id: "env_1",
        signing_url: None,
    };
    let jane = Joiner { discord_user_id: "111111111111", discord_username: "jane", role: OnboardingRole::Volunteer };
    store.record_onboarding_sent(&sent, jane).await.expect("sent");
    let roles = RoleSink::new(http_client(), Some(server.uri()), "t".into(), "g".into());
    let d = Dispatcher::new(store.clone(), vec![Arc::new(roles)], cfg());
    assert_eq!(d.tick().await.expect("tick").delivered, 0, "nothing until it's signed");
    store.complete_release(id, Outcome::Signed).await.expect("sign");
    assert_eq!(d.tick().await.expect("tick").delivered, 1);
}

#[tokio::test]
async fn a_member_who_left_is_not_retried() {
    use homestead_domain::Event;
    use homestead_domain::onboarding::OnboardingRole;
    use homestead_integrations::discord::RoleSink;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{ "id": "2", "name": "Volunteer" }])))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(serde_json::json!({ "message": "Unknown Member", "code": 10007 })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let roles = RoleSink::new(http_client(), Some(server.uri()), "t".into(), "g".into());
    let left = Event::MemberOnboarded {
        discord_user_id: "222222222222".into(),
        name: "Omar".into(),
        role: OnboardingRole::Volunteer,
    };
    let err = roles.deliver(1, &left).await.expect_err("gone");
    assert!(
        matches!(&err, homestead_bus::DeliveryError::Permanent(m) if m.contains("no longer in the server")),
        "{err:?}"
    );
}

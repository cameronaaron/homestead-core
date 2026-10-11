//! People, credentials and who-did-what attribution.

use homestead_domain::NewTask;
use homestead_domain::auth::{ActorKind, NewActor, Role};
use homestead_store::{Actor, Store, StoreError};
use sqlx::PgPool;
use time::Duration;

const HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2hoYXNoaGFzaA";

fn h(c: char) -> String {
    c.to_string().repeat(64)
}

async fn person(store: &Store, name: &str, roles: &[Role]) -> homestead_domain::auth::ActorRecord {
    store
        .create_actor(NewActor {
            display_name: name.into(),
            email: Some(format!("{}@example.org", name.to_lowercase())),
            kind: ActorKind::Human,
            roles: roles.to_vec(),
        })
        .await
        .expect("create person")
}

fn as_actor(store: &Store, a: &homestead_domain::auth::ActorRecord) -> Store {
    store.acting_as(Actor { id: Some(a.id), label: a.display_name.clone() })
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn writes_name_the_actor_and_never_leak_into_the_next_transaction(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let sam = person(&store, "Sam", &[Role::Crew]).await;
    let task = as_actor(&store, &sam).create_task(NewTask::titled("Mulch beds")).await.expect("task");
    // A plain store right after, very likely on the same pooled connection.
    let other = store.create_task(NewTask::titled("Oil hinges")).await.expect("task");

    let row = |id: uuid::Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, (String, Option<uuid::Uuid>)>(
                "select actor_label, actor_id from homestead.audit_entries
                 where resource_type = 'tasks' and resource_id = $1 and action = 'insert'",
            )
            .bind(id.to_string())
            .fetch_one(&pool)
            .await
            .expect("audit row")
        }
    };
    assert_eq!(row(task.id).await, ("Sam".to_owned(), Some(sam.id)));
    assert_eq!(row(other.id).await, ("system".to_owned(), None));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_setup_link_sets_a_password_once_and_signs_everyone_else_out(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let steward = person(&store, "Kiki", &[Role::Steward]).await;
    let sam = person(&store, "Sam", &[Role::Volunteer]).await;
    let kiki = as_actor(&store, &steward);

    kiki.issue_setup_link(sam.id, &h('a'), Duration::hours(72)).await.expect("link");
    assert_eq!(store.setup_link_actor(&h('a')).await.expect("q").map(|a| a.id), Some(sam.id));
    let sam_store = as_actor(&store, &sam);
    sam_store.start_session(sam.id, &h('5'), Duration::days(1)).await.expect("old session");

    let after = sam_store.redeem_setup_link(&h('a'), HASH).await.expect("redeem");
    assert!(after.has_password);
    assert!(store.session_actor(&h('5')).await.expect("q").is_none(), "old sessions end on a new password");
    assert!(matches!(sam_store.redeem_setup_link(&h('a'), HASH).await, Err(StoreError::NotFound(_))), "reused");

    // A newer link retires the older one, and expired links are dead.
    kiki.issue_setup_link(sam.id, &h('b'), Duration::hours(72)).await.expect("link b");
    kiki.issue_setup_link(sam.id, &h('c'), Duration::hours(72)).await.expect("link c");
    assert!(store.setup_link_actor(&h('b')).await.expect("q").is_none());
    sqlx::query("update homestead.setup_links set created_at = now() - interval '4 days', expires_at = now() - interval '1 second' where token_hash = $1")
        .bind(h('c'))
        .execute(&pool)
        .await
        .expect("age link");
    assert!(store.setup_link_actor(&h('c')).await.expect("q").is_none());

    // The hash itself never lands in the audit history.
    let leaked: i64 =
        sqlx::query_scalar("select count(*) from homestead.audit_entries where (before::text || after::text || metadata::text) like '%argon2%'")
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(leaked, 0);
    let audited: Vec<String> = sqlx::query_scalar(
        "select action from homestead.audit_entries where resource_type = 'actors' and resource_id = $1 order by id",
    )
    .bind(sam.id.to_string())
    .fetch_all(&pool)
    .await
    .expect("actions");
    for a in ["setup_link_issued", "login", "password_set"] {
        assert!(audited.iter().any(|x| x == a), "{a} not audited: {audited:?}");
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn ten_wrong_passwords_lock_the_account(pool: PgPool) {
    let store = Store::from_pool(pool);
    let sam = person(&store, "Sam", &[Role::Crew]).await;
    as_actor(&store, &sam).issue_setup_link(sam.id, &h('a'), Duration::hours(1)).await.expect("link");
    store.redeem_setup_link(&h('a'), HASH).await.expect("redeem");
    for _ in 0..9 {
        store.record_login_failure(sam.id).await.expect("fail");
    }
    let c = store.login_candidate("SAM@example.org ").await.expect("q").expect("found by normalised email");
    assert!(c.locked_until.is_none());
    store.record_login_failure(sam.id).await.expect("tenth");
    let c = store.login_candidate("sam@example.org").await.expect("q").expect("found");
    assert!(c.locked_until.is_some_and(|t| t > time::OffsetDateTime::now_utc()));
    // A successful sign-in (after the lock) clears the counter.
    as_actor(&store, &sam).start_session(sam.id, &h('5'), Duration::days(1)).await.expect("session");
    assert!(store.login_candidate("sam@example.org").await.expect("q").expect("found").locked_until.is_none());
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn disabling_someone_ends_their_sessions_and_tokens(pool: PgPool) {
    let store = Store::from_pool(pool);
    let steward = person(&store, "Kiki", &[Role::Steward]).await;
    let kiki = as_actor(&store, &steward);
    let sam = person(&store, "Sam", &[Role::Crew]).await;
    as_actor(&store, &sam).start_session(sam.id, &h('5'), Duration::days(1)).await.expect("session");
    assert_eq!(store.session_actor(&h('5')).await.expect("q").map(|a| a.id), Some(sam.id));

    let bot = kiki
        .create_actor(NewActor {
            display_name: "cron".into(),
            email: None,
            kind: ActorKind::Service,
            roles: vec![Role::Service],
        })
        .await
        .expect("service");
    kiki.create_api_token(bot.id, "nightly", &h('7')).await.expect("token");
    assert_eq!(store.api_token_actor(&h('7')).await.expect("q").map(|a| a.id), Some(bot.id));
    assert!(
        matches!(kiki.create_api_token(sam.id, "x", &h('9')).await, Err(StoreError::Conflict(_))),
        "people don't get tokens"
    );

    kiki.set_actor_disabled(sam.id, true).await.expect("disable sam");
    kiki.set_actor_disabled(bot.id, true).await.expect("disable bot");
    assert!(store.session_actor(&h('5')).await.expect("q").is_none());
    assert!(store.api_token_actor(&h('7')).await.expect("q").is_none());
    assert!(store.get_actor(sam.id).await.expect("q").capabilities().iter().next().is_none());

    // Nobody can lock the organization out by accident.
    assert!(matches!(kiki.set_actor_disabled(steward.id, true).await, Err(StoreError::Conflict(_))));
    assert!(matches!(kiki.set_roles(steward.id, &[Role::Crew]).await, Err(StoreError::Conflict(_))));
    let granted = kiki.set_roles(sam.id, &[Role::FarmLead, Role::Board]).await.expect("roles");
    assert_eq!(granted.roles, vec![Role::Board, Role::FarmLead]);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_revoked_token_stops_working_and_the_legacy_token_actor_is_stable(pool: PgPool) {
    let store = Store::from_pool(pool);
    let a = store.admin_token_actor().await.expect("admin token actor");
    let b = store.admin_token_actor().await.expect("again");
    assert_eq!((a.id, a.roles.clone()), (b.id, vec![Role::Service]));
    assert!(store.list_actors().await.expect("list").iter().all(|x| x.id != a.id), "hidden from the people list");

    let steward = person(&store, "Kiki", &[Role::Steward]).await;
    let kiki = as_actor(&store, &steward);
    let bot = kiki
        .create_actor(NewActor {
            display_name: "sensor".into(),
            email: None,
            kind: ActorKind::Service,
            roles: vec![Role::Service],
        })
        .await
        .expect("service");
    let id = kiki.create_api_token(bot.id, "weather station", &h('7')).await.expect("token");
    kiki.revoke_api_token(id).await.expect("revoke");
    assert!(store.api_token_actor(&h('7')).await.expect("q").is_none());

    // Linking a Discord identity attributes chat actions to the person.
    kiki.set_external_subject(steward.id, Some("discord:42")).await.expect("link");
    assert_eq!(store.actor_by_external_subject("discord:42").await.expect("q").map(|a| a.id), Some(steward.id));
    assert!(kiki.set_external_subject(steward.id, Some("admin-token")).await.is_err());
}

/// An audit entry carries its whole row, so every audited table needs a
/// rule for who may see it (`AUDIT_VISIBILITY`); otherwise only stewards
/// would, silently. Read from the live trigger catalogue, so a new table
/// with an audit trigger fails here until it has a rule.
#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn every_audited_table_has_a_visibility_rule(pool: PgPool) {
    let audited: Vec<String> = sqlx::query_scalar(
        "select distinct event_object_table::text from information_schema.triggers
         where trigger_schema = 'homestead' and action_statement like '%audit_row_change%'",
    )
    .fetch_all(&pool)
    .await
    .expect("triggers");
    assert!(audited.len() > 20, "found only {audited:?}");
    let ruled: Vec<&str> = homestead_domain::auth::AUDIT_VISIBILITY.iter().map(|(t, _)| *t).collect();
    let missing: Vec<_> = audited.iter().filter(|t| *t != "attachments" && !ruled.contains(&t.as_str())).collect();
    assert!(missing.is_empty(), "audited tables with no visibility rule: {missing:?}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_audit_log_shows_only_rows_the_reader_could_read(pool: PgPool) {
    use homestead_domain::auth::CapabilitySet;
    let store = Store::from_pool(pool.clone());
    store.create_task(NewTask::titled("Mulch beds")).await.expect("task");
    sqlx::query("insert into homestead.contacts (name, email, source) values ('Ana', 'ana@example.org', 'test')")
        .execute(&pool)
        .await
        .expect("contact");
    let seen = |caps: CapabilitySet| {
        let store = store.clone();
        async move {
            let mut t: Vec<String> = store
                .recent_audit_entries(caps, 100)
                .await
                .expect("audit")
                .into_iter()
                .map(|e| e.resource_type)
                .collect();
            t.dedup();
            t
        }
    };
    let service = seen(Role::Service.capabilities()).await;
    assert!(service.contains(&"tasks".to_owned()) && !service.contains(&"contacts".to_owned()), "{service:?}");
    assert!(seen(Role::Board.capabilities()).await.contains(&"contacts".to_owned()));
    assert!(seen(CapabilitySet::EMPTY).await.is_empty());
}

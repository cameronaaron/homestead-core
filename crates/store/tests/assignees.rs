//! Who is doing a task, against a real Postgres.

use homestead_domain::auth::{ActorKind, ActorRecord, NewActor, Role};
use homestead_domain::{Event, NewTask};
use homestead_store::{Store, StoreError};
use sqlx::PgPool;
use time::macros::date;

async fn actor(store: &Store, name: &str, kind: ActorKind) -> ActorRecord {
    let email = (kind == ActorKind::Human).then(|| format!("{}@example.org", name.to_lowercase()));
    let roles = if kind == ActorKind::Human { vec![Role::Crew] } else { vec![Role::Service] };
    store.create_actor(NewActor { display_name: name.into(), email, kind, roles }).await.expect("actor")
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_task_can_be_given_to_a_person_and_back_to_anyone(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let ava = actor(&store, "Ava", ActorKind::Human).await;
    let cam = actor(&store, "Cam", ActorKind::Human).await;

    let t = store.create_task(NewTask { assigned_to: Some(ava.id), ..NewTask::titled("Mend the fence") }).await;
    let t = t.expect("assigned at creation");
    assert_eq!(store.get_task(t.id).await.expect("read").assigned_to, Some(ava.id));
    let created: serde_json::Value =
        sqlx::query_scalar("select payload from homestead.events where kind = 'task.created'")
            .fetch_one(&pool)
            .await
            .expect("event");
    match Event::from_parts("task.created", created).expect("decodes") {
        Event::TaskCreated { task } => assert_eq!(task.assigned_to, Some(ava.id), "integrations hear who"),
        other => panic!("unexpected {other:?}"),
    }

    assert_eq!(store.assign_task(t.id, Some(cam.id)).await.expect("reassign").assigned_to, Some(cam.id));
    assert_eq!(store.assign_task(t.id, None).await.expect("let go").assigned_to, None);

    let people: Vec<String> = store.assignable_people().await.expect("people").into_iter().map(|p| p.1).collect();
    assert_eq!(people, ["Ava", "Cam"]);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn only_active_people_can_be_given_a_task(pool: PgPool) {
    let store = Store::from_pool(pool);
    let bot = actor(&store, "Discord bot", ActorKind::Service).await;
    let sam = actor(&store, "Sam", ActorKind::Human).await;
    let gone = actor(&store, "Gone", ActorKind::Human).await;
    store.set_actor_disabled(gone.id, true).await.expect("disable");
    let t = store.create_task(NewTask::titled("Turn compost")).await.expect("task");

    for (who, id) in [("a service account", bot.id), ("someone disabled", gone.id), ("nobody", uuid::Uuid::nil())] {
        let err = store.assign_task(t.id, Some(id)).await.expect_err(who);
        assert!(matches!(&err, StoreError::Conflict(m) if m.contains("active account")), "{who}: {err:?}");
        let err = store.create_task(NewTask { assigned_to: Some(id), ..NewTask::titled("x") }).await.expect_err(who);
        assert!(matches!(err, StoreError::Conflict(_)), "{who} at creation: {err:?}");
    }
    let people: Vec<_> = store.assignable_people().await.expect("people").into_iter().map(|p| p.0).collect();
    assert_eq!(people, [sam.id], "the picker offers only who can be chosen");

    // Disabling someone releases current responsibility but preserves who
    // completed historical work.
    store.assign_task(t.id, Some(sam.id)).await.expect("assign");
    let done = store
        .create_task(NewTask { assigned_to: Some(sam.id), ..NewTask::titled("Wash eggs") })
        .await
        .expect("assigned task");
    store.complete_task(done.id).await.expect("complete");
    store.set_actor_disabled(sam.id, true).await.expect("disable sam");
    assert_eq!(store.get_task(t.id).await.expect("read").assigned_to, None, "open work returns to anyone");
    assert_eq!(store.get_task(done.id).await.expect("read").assigned_to, Some(sam.id), "completed work keeps history");
    assert_eq!(store.person_names().await.expect("names").get(&sam.id).map(String::as_str), Some("Sam"));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_closed_task_keeps_who_did_it(pool: PgPool) {
    let store = Store::from_pool(pool);
    let ava = actor(&store, "Ava", ActorKind::Human).await;
    let t = store.create_task(NewTask { assigned_to: Some(ava.id), ..NewTask::titled("Order garlic") }).await;
    let t = t.expect("task");
    store.complete_task(t.id).await.expect("done");
    let err = store.assign_task(t.id, None).await.expect_err("closed");
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
    assert_eq!(store.get_task(t.id).await.expect("read").assigned_to, Some(ava.id));
    let err = store.assign_task(uuid::Uuid::nil(), None).await.expect_err("missing");
    assert!(matches!(err, StoreError::NotFound("task")), "{err:?}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn deadline_reminders_name_the_homestead_assignee(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let ava = actor(&store, "Ava", ActorKind::Human).await;
    let due = |title: &str| NewTask { due_on: Some(date!(2026 - 10 - 02)), ..NewTask::titled(title) };
    store.create_task(NewTask { assigned_to: Some(ava.id), ..due("Submit the grant") }).await.expect("mine");
    store.create_task(due("Mow the orchard")).await.expect("anyone");
    store.send_deadline_reminders_as_of(date!(2026 - 10 - 01)).await.expect("remind");
    let payload: serde_json::Value =
        sqlx::query_scalar("select payload from homestead.events where kind = 'task.deadlines'")
            .fetch_one(&pool)
            .await
            .expect("reminder");
    let Event::TaskDeadlines { items, .. } = Event::from_parts("task.deadlines", payload).expect("decodes") else {
        panic!("not a reminder")
    };
    let who: Vec<(&str, &str)> = items.iter().map(|i| (i.title.as_str(), i.assignee.as_str())).collect();
    assert_eq!(who, [("Mow the orchard", ""), ("Submit the grant", "Ava")]);
}

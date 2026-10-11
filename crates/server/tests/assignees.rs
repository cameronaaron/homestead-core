//! Task assignees in the UI: taking a task, giving it to someone, and
//! seeing yours first on Today.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_domain::NewTask;
use homestead_domain::auth::Role;
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;

mod common;

const AVA: &str = "a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";

async fn send(r: &Router, cookie: &str, method: &str, uri: &str, form: Option<&str>) -> (StatusCode, String) {
    let b = Request::builder().method(method).uri(uri).header(header::COOKIE, format!("sbh_session={cookie}"));
    let req = match form {
        Some(f) => b.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded").body(Body::from(f.to_owned())),
        None => b.body(Body::empty()),
    }
    .expect("request");
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// What Today shows under `heading`, up to the next heading.
fn under<'a>(page: &'a str, heading: &str) -> &'a str {
    let start = page.find(&format!("<h2>{heading}</h2>")).unwrap_or_else(|| panic!("no {heading} on {page}"));
    let rest = &page[start + heading.len() + 9..];
    &rest[..rest.find("<h2>").unwrap_or(rest.len())]
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn people_take_tasks_and_see_theirs_first(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    common::steward(&pool).await;
    let ava = common::person(&pool, "Ava", &[Role::Crew], AVA).await;
    let bot = common::service(&pool, "Discord bot", "b0b0").await;
    let r = app(AppState::new(store.clone()));
    let mut input = NewTask::titled("Mend the fence");
    input.details = "Replace the loose boards beside the north gate.".into();
    let fence = store.create_task(input).await.expect("task");

    let (_, page) = send(&r, AVA, "GET", "/tasks", None).await;
    assert!(page.contains("Take it") && page.contains("anyone"), "{page}");
    assert!(
        page.contains("<details class=\"task-description\">")
            && page.contains("Show details")
            && page.contains("Hide details")
            && page.contains("Replace the loose boards beside the north gate."),
        "task details can be expanded without JavaScript: {page}"
    );
    assert!(!page.contains(">Discord bot<"), "a service account isn't offered: {page}");

    let take = format!("assigned_to={}", ava.id);
    let (status, _) = send(&r, AVA, "POST", &format!("/tasks/{}/assign", fence.id), Some(&take)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, page) = send(&r, AVA, "GET", "/tasks", None).await;
    assert!(page.contains("you") && page.contains("Let go"), "{page}");
    let (_, page) = send(&r, common::STEWARD, "GET", "/tasks", None).await;
    assert!(page.contains("Ava"), "{page}");

    // The steward gives a new task to Ava from the form.
    let form = format!("title=Order+garlic&priority=normal&assigned_to={}", ava.id);
    let (status, _) = send(&r, common::STEWARD, "POST", "/tasks", Some(&form)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    store.create_task(NewTask::titled("Turn compost")).await.expect("anyone's");

    let (_, mine) = send(&r, AVA, "GET", "/tasks?who=mine", None).await;
    assert!(mine.contains("Mend the fence") && mine.contains("Order garlic"), "{mine}");
    assert!(!mine.contains("Turn compost"), "mine excludes unassigned work: {mine}");
    let (_, unassigned) = send(&r, AVA, "GET", "/tasks?who=unassigned", None).await;
    assert!(unassigned.contains("Turn compost"), "{unassigned}");
    assert!(!unassigned.contains("Mend the fence") && !unassigned.contains("Order garlic"), "{unassigned}");

    let (_, today) = send(&r, AVA, "GET", "/", None).await;
    let yours = under(&today, "Yours");
    assert!(yours.contains("Mend the fence") && yours.contains("Order garlic"), "{yours}");
    assert!(!yours.contains("Turn compost"), "{yours}");
    let rest = under(&today, "Coming up");
    assert!(rest.contains("Turn compost") && !rest.contains("Mend the fence"), "yours aren't repeated: {rest}");
    let (_, theirs) = send(&r, common::STEWARD, "GET", "/", None).await;
    assert!(!theirs.contains("<h2>Yours</h2>"), "nothing is the steward's");
    assert!(theirs.contains("Mend the fence") && theirs.contains(" · Ava"), "{theirs}");

    // A service account can't be handed work.
    let bad = format!("assigned_to={}", bot.id);
    let (status, page) = send(&r, AVA, "POST", &format!("/tasks/{}/assign", fence.id), Some(&bad)).await;
    assert_ne!(status, StatusCode::SEE_OTHER, "{page}");
    assert_eq!(store.get_task(fence.id).await.expect("read").assigned_to, Some(ava.id));

    let (status, _) = send(&r, AVA, "POST", &format!("/tasks/{}/assign", fence.id), Some("assigned_to=")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(store.get_task(fence.id).await.expect("read").assigned_to, None, "let go");
}

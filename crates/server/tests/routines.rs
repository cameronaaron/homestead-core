//! Routines through the Tasks page and the API.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;

mod common;

const BOT: &str = "0000000000000000000000000000000000000000000000000000000000000007";

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_routine_added_on_the_tasks_page_shows_up_as_todays_task(pool: PgPool) {
    common::steward(&pool).await;
    common::service(&pool, "cron", BOT).await;
    let store = Store::from_pool(pool.clone());
    let today = store.today().await.expect("today");
    let r = app(AppState::new(store));
    let send = |req: Request<Body>| {
        let r = r.clone();
        async move {
            let resp = r.oneshot(req).await.expect("infallible");
            let status = resp.status();
            (status, String::from_utf8_lossy(&resp.into_body().collect().await.expect("b").to_bytes()).into_owned())
        }
    };
    let form = format!("title=Check+water&repeats=days&days=1&day=1&starts_on={today}&priority=normal&lead_days=0");
    let post = Request::post("/routines")
        .header(header::COOKIE, format!("sbh_session={}", common::STEWARD))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form))
        .expect("req");
    assert_eq!(send(post).await.0, StatusCode::SEE_OTHER);
    let page =
        Request::get("/tasks").header(header::COOKIE, format!("sbh_session={}", common::STEWARD)).body(Body::empty());
    let (_, html) = send(page.expect("req")).await;
    assert!(html.contains("🔁 Check water") && html.contains("every day"), "{html}");

    // An outside cron can trigger generation; it's idempotent.
    for _ in 0..2 {
        let run = Request::post("/api/v1/recurring/run")
            .header(header::AUTHORIZATION, format!("Bearer {BOT}"))
            .header("idempotency-key", uuid::Uuid::new_v4().to_string())
            .body(Body::empty())
            .expect("req");
        assert_eq!(send(run).await.0, StatusCode::OK);
    }
    let n: i64 = sqlx::query_scalar("select count(*) from homestead.tasks where template_id is not null")
        .fetch_one(&pool)
        .await
        .expect("n");
    assert_eq!(n, 1);
    let by: String = sqlx::query_scalar(
        "select actor_label from homestead.audit_entries where resource_type = 'tasks' and action = 'insert'",
    )
    .fetch_one(&pool)
    .await
    .expect("audit");
    assert_eq!(by, "scheduler");
}

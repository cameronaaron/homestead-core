//! Linear issues reach Today: a page pulls them through a fake Linear.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_integrations::linear::Puller;
use homestead_server::linear_pull::LinearPull;
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use wiremock::matchers::{header as h, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;

fn node(n: u32, title: &str, state: &str, due: Option<&str>) -> Value {
    json!({
        "id": format!("00000000-0000-4000-8000-{n:012}"), "identifier": format!("SUN-{n}"),
        "url": format!("https://linear.app/sunbrewedhomestead/issue/SUN-{n}"),
        "title": title, "description": "", "priority": 2, "dueDate": due,
        "updatedAt": "2026-09-28T20:58:22.117Z", "archivedAt": null,
        "state": { "type": state, "name": "Backlog" }, "assignee": { "name": "photomancer", "displayName": "" }
    })
}

fn page(nodes: Vec<Value>) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "data": { "issues": { "nodes": nodes, "pageInfo": { "hasNextPage": false, "endCursor": null } } }
    }))
}

fn router(pool: PgPool, linear: &MockServer) -> Router {
    let puller = Puller::new(reqwest::Client::new(), linear.uri(), "lin_key".into(), "SUN".into());
    let state = AppState {
        linear_pull: Some(Arc::new(LinearPull::new(puller, Duration::from_secs(300)))),
        ..AppState::new(Store::from_pool(pool))
    };
    app(state)
}

async fn get(r: &Router, uri: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .uri(uri)
        .header(header::COOKIE, format!("sbh_session={}", common::STEWARD))
        .body(Body::empty())
        .expect("request");
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn today_shows_issues_filed_in_linear(pool: PgPool) {
    common::steward(&pool).await;
    let linear = MockServer::start().await;
    Mock::given(method("POST"))
        .and(h("authorization", "lin_key"))
        .respond_with(page(vec![
            node(25, "Start fall seeds", "backlog", None),
            node(5, "File IRS Form 990-N", "unstarted", Some("2020-01-01")),
            node(3, "Old and done", "completed", None),
        ]))
        .expect(1) // the second page view finds the pull fresh
        .mount(&linear)
        .await;
    let r = router(pool, &linear);

    let (status, today) = get(&r, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(today.contains("File IRS Form 990-N") && today.contains("Overdue"), "dated issues are due work");
    assert!(today.contains("Start fall seeds"));
    assert!(
        today.contains(r#"href="https://linear.app/sunbrewedhomestead/issue/SUN-25""#),
        "each task links its issue"
    );
    assert!(today.contains("photomancer"), "and says who has it");
    assert!(!today.contains("Old and done"), "finished issues aren't imported");
    assert!(today.contains("Tasks from Linear, pulled just now"));

    let (_, tasks) = get(&r, "/tasks").await;
    assert!(tasks.contains("SUN-25") && tasks.contains("SUN-5"));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_failing_pull_is_shown_not_hidden(pool: PgPool) {
    common::steward(&pool).await;
    let linear = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "errors": [{ "message": "Authentication required", "extensions": { "code": "AUTHENTICATION_ERROR" } }]
        })))
        .mount(&linear)
        .await;
    let (status, today) = get(&router(pool, &linear), "/").await;
    assert_eq!(status, StatusCode::OK, "Today still renders");
    assert!(today.contains("Linear pull failing") && today.contains("Authentication required"), "{today}");
}

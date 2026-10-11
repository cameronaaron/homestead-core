//! Equipment through the forms: record, service, remind, attach.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_domain::auth::Role;
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;

mod common;

const VOLUNTEER: &str = "0000000000000000000000000000000000000000000000000000000000000003";
const CREW: &str = "0000000000000000000000000000000000000000000000000000000000000006";

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_service_is_recorded_and_the_next_one_lands_in_tasks(pool: PgPool) {
    common::person(&pool, "Cam", &[Role::Crew], CREW).await;
    common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    let r = app(AppState::new(Store::from_pool(pool)));
    let send = |who: &str, method: &str, uri: &str, form: &str| {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::COOKIE, format!("sbh_session={who}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form.to_owned()))
            .expect("req");
        let r = r.clone();
        async move {
            let resp = r.oneshot(req).await.expect("infallible");
            let (status, loc) =
                (resp.status(), resp.headers().get(header::LOCATION).map(|v| v.to_str().expect("a").to_owned()));
            (
                status,
                loc,
                String::from_utf8_lossy(&resp.into_body().collect().await.expect("b").to_bytes()).into_owned(),
            )
        }
    };
    let (status, page, _) = send(
        CREW,
        "POST",
        "/equipment",
        "name=Tiller&category=Power+tools&manufacturer=Honda&model=FG110&serial_number=GCAAH-1001&purchase_cost=499&condition=good",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let page = page.expect("detail");
    let (status, _, body) = send(
        CREW,
        "POST",
        &format!("{page}/maintenance"),
        "performed_on=2026-09-05&work=Oil+change&cost=20&performed_by=Sam&next_service_on=2027-03-01&remind=on",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    let (_, _, detail) = send(CREW, "GET", &page, "").await;
    assert!(
        detail.contains("Oil change") && detail.contains("Next service 2027-03-01") && detail.contains("GCAAH-1001")
    );
    assert!(!detail.contains("$499"), "crew don't see what it cost: {detail}");
    let (_, _, tasks) = send(CREW, "GET", "/tasks", "").await;
    assert!(tasks.contains("Service Tiller"), "reminder task: {tasks}");

    // Volunteers can't see equipment at all.
    assert_eq!(send(VOLUNTEER, "GET", "/equipment", "").await.0, StatusCode::FORBIDDEN);
}

//! Soil and crop health through the forms.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_domain::*;
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;

mod common;

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn soil_and_health_are_recorded_and_shown_on_the_place(pool: PgPool) {
    common::steward(&pool).await;
    let store = Store::from_pool(pool.clone());
    let bed = store.create_location(NewLocation::named("A3", LocationKind::Bed)).await.expect("bed");
    let r = app(AppState::new(Store::from_pool(pool)));
    let send = |method: &str, uri: &str, form: &str| {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::COOKIE, format!("sbh_session={}", common::STEWARD))
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
    let (s, test_page, body) = send(
        "POST",
        "/soil/tests",
        &format!("location_id={}&tested_on=2026-03-01&lab=UC+Davis&ph=5.8&measurements=Calcium+%3D+1200+ppm", bed.id),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "{body}");
    let (_, _, page) = send("GET", &test_page.expect("test page"), "").await;
    assert!(page.contains("Calcium") && page.contains("1200 ppm") && page.contains("Lab report"), "{page}");
    let (s, _, body) = send(
        "POST",
        "/soil/amendments",
        &format!("location_id={}&applied_on=2026-03-08&kind=lime&amount=5&unit=lb&reason=pH+5.8", bed.id),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "{body}");
    let (s, obs, body) = send(
        "POST",
        "/health",
        &format!("observed_on=2026-06-01&location_id={}&category=pest&severity=high&observation=Aphids", bed.id),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "{body}");
    let obs = obs.expect("observation page");
    let (s, _, _) = send("POST", &format!("{obs}/resolve"), "resolved_on=2026-06-10&outcome=Gone").await;
    assert_eq!(s, StatusCode::SEE_OTHER);

    let (_, _, place) = send("GET", &format!("/locations/{}", bed.id), "").await;
    assert!(place.contains("pH 5.8 · Calcium 1200 ppm") && place.contains("5 lb"), "{place}");
    assert!(place.contains("Aphids") && place.contains("closed <time datetime=\"2026-06-10\">"), "{place}");
}

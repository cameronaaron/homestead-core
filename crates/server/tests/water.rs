//! Weather and watering through the forms.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;

mod common;

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_watering_and_a_rain_gauge_reading_round_trip_in_local_units(pool: PgPool) {
    common::steward(&pool).await;
    let store = Store::from_pool(pool.clone()).with_timezone("America/Los_Angeles").await.expect("tz");
    let r = app(AppState::new(store));
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
            let status = resp.status();
            (status, String::from_utf8_lossy(&resp.into_body().collect().await.expect("b").to_bytes()).into_owned())
        }
    };
    send("POST", "/locations", "name=Bed+4&kind=bed").await;
    let (_, page) = send("GET", "/water", "").await;
    let select = &page[page.find("name=\"location_id\"").expect("place select")..];
    let marker = "—</option><option value=\"";
    let bed = &select[select.find(marker).expect("bed") + marker.len()..][..36];

    let (status, body) = send(
        "POST",
        "/water/irrigation",
        &format!("location_id={bed}&started_at=2026-09-28T07%3A00&minutes=40&gallons=20&method=drip"),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    let (status, _) =
        send("POST", "/water/weather", "observed_at=2026-09-28T16%3A30&temperature_f=91&rain_in=0.5").await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let (_, page) = send("GET", "/water", "").await;
    assert!(page.contains("2026-09-28 07:00") && page.contains("Bed 4") && page.contains(">40<"), "{page}");
    assert!(page.contains(">20.0<"), "20 gallons shown back as 20: {page}");
    assert!(page.contains("91.0°F") && page.contains("0.5 in") && page.contains("2026-09-28 16:30"), "{page}");
    // Stored as an instant: 7 am Pacific daylight time is 14:00 UTC.
    let at: time::OffsetDateTime =
        sqlx::query_scalar("select started_at from homestead.irrigation_records").fetch_one(&pool).await.expect("q");
    assert_eq!(at, time::macros::datetime!(2026-09-28 14:00 UTC));

    let (status, _) = send("POST", "/water/weather", "observed_at=2026-09-28T16%3A30").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "an empty observation");
}

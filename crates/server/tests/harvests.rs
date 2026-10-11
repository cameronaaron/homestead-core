//! A harvest's page records where it went; donations list and export.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_domain::*;
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::macros::date;
use tower::ServiceExt;

mod common;

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_harvest_is_split_on_its_page_and_donations_export(pool: PgPool) {
    common::steward(&pool).await;
    let store = Store::from_pool(pool.clone());
    let bed = store.create_location(NewLocation::named("A3", LocationKind::Bed)).await.expect("bed");
    let p = store.create_planting(NewPlanting::at(bed.id, "Tomato", date!(2026 - 04 - 01))).await.expect("p");
    let h = store
        .log_harvest(NewHarvest {
            planting_id: p.id,
            harvested_on: date!(2026 - 07 - 04),
            quantity: Decimal::from(42),
            unit: HarvestUnit::Lb,
            notes: String::new(),
        })
        .await
        .expect("h");
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
            let status = resp.status();
            (status, String::from_utf8_lossy(&resp.into_body().collect().await.expect("b").to_bytes()).into_owned())
        }
    };
    let at = format!("/harvests/{}/dispositions", h.id);
    // The form always sends a donation date; it only applies to donations.
    let (s, body) =
        send("POST", &at, "kind=sale&quantity=22&donated_on=2026-07-04&recipient=&operational_value=").await;
    assert_eq!(s, StatusCode::SEE_OTHER, "{body}");
    let (s, body) = send(
        "POST",
        &at,
        "kind=donation&quantity=15&recipient=%3DValley+Pantry&donated_on=2026-07-05&operational_value=%2430",
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "{body}");
    let (s, body) = send("POST", &at, "kind=compost&quantity=6&donated_on=2026-07-04").await;
    assert_eq!(s, StatusCode::CONFLICT, "only 5 lb left: {body}");

    let (_, page) = send("GET", &format!("/harvests/{}", h.id), "").await;
    assert!(page.contains("sold 22 lb (52.4%)") && page.contains("to =Valley Pantry"), "{page}");
    assert!(page.contains("5 lb not yet recorded"), "{page}");
    assert!(page.contains(&format!("href=\"/plantings/{}\"", p.id)), "traces back to its planting");

    let (_, list) = send("GET", "/donations?from=2026-07-01&to=2026-07-31", "").await;
    assert!(list.contains("15 lb") && list.contains("Estimated value $30.00"), "{list}");
    let (s, csv) = send("GET", "/donations/export.csv?from=2026-07-01&to=2026-07-31", "").await;
    assert_eq!(s, StatusCode::OK);
    assert!(csv.contains("2026-07-05,'=Valley Pantry,Tomato,15,lb,30.00"), "formula neutralised: {csv}");
}

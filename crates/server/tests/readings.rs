//! Readings through the forms: taken on a place's page, shown there and on
//! /readings, refused to people who can't record the thing measured.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_domain::auth::Role;
use homestead_domain::*;
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;

mod common;

const VOLUNTEER: &str = "0000000000000000000000000000000000000000000000000000000000000003";
const BOARD: &str = "0000000000000000000000000000000000000000000000000000000000000004";

async fn send(r: &Router, who: &str, method: &str, uri: &str, form: &str) -> (StatusCode, Option<String>, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, format!("sbh_session={who}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form.to_owned()))
        .expect("req");
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let (status, to) = (resp.status(), resp.headers().get(header::LOCATION).map(|v| v.to_str().expect("a").to_owned()));
    (status, to, String::from_utf8_lossy(&resp.into_body().collect().await.expect("b").to_bytes()).into_owned())
}

fn enc(s: &str) -> String {
    s.replace('%', "%25").replace(' ', "+").replace('=', "%3D").replace('\n', "%0A").replace('°', "%C2%B0")
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_reading_taken_on_a_place_shows_there_and_on_the_readings_page(pool: PgPool) {
    common::steward(&pool).await;
    common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    common::person(&pool, "Bo", &[Role::Board], BOARD).await;
    let store = Store::from_pool(pool.clone());
    let pile = store.create_location(NewLocation::named("Pile 2", LocationKind::Compost)).await.expect("pile");
    let r = app(AppState::new(Store::from_pool(pool)));
    let st = common::STEWARD;
    let here = format!("/locations/{}", pile.id);

    for (when, temp) in [("2026-09-01T08:00", "120"), ("2026-09-03T08:00", "150"), ("", "141")] {
        let form = format!(
            "back={here}&subject=location:{}&kind=Compost+temperature&observed_at={when}&values={}",
            pile.id,
            enc(&format!("temperature = {temp} °F\nmoisture = 55 %"))
        );
        let (s, to, body) = send(&r, st, "POST", "/readings", &form).await;
        assert_eq!((s, to.as_deref()), (StatusCode::SEE_OTHER, Some(here.as_str())), "{body}");
    }
    let (_, _, page) = send(&r, st, "GET", &here, "").await;
    assert!(page.contains("<h2>Readings</h2>") && page.contains("141 °F"), "latest on the place: {page}");
    assert!(page.contains("120–150 · 3 readings"), "the range of all three");
    assert!(page.contains("class=\"spark\""), "and its shape");

    let (s, _, list) = send(&r, st, "GET", "/readings?kind=compost+temperature", "").await;
    assert_eq!(s, StatusCode::OK);
    assert!(list.contains("Pile 2") && list.contains("compost temperature"), "{list}");

    // Nonsense is refused with the reason, and never redirects off-site.
    let bad = format!("back=https://evil.example&subject=location:{}&kind=x&values=hot", pile.id);
    let (s, _, body) = send(&r, st, "POST", "/readings", &bad).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("expected a number") && body.contains("href=\"/readings\""), "{body}");

    // A volunteer reads them but takes none; the board can't record the
    // flock's feed either, and the form isn't offered to them.
    let (s, _, vlist) = send(&r, VOLUNTEER, "GET", "/readings", "").await;
    assert_eq!(s, StatusCode::OK);
    assert!(vlist.contains("Pile 2") && !vlist.contains("Take a reading"));
    let (_, _, vplace) = send(&r, VOLUNTEER, "GET", &here, "").await;
    assert!(!vplace.contains("Take a reading"));
    let form = format!("subject=location:{}&kind=x&values=1", pile.id);
    assert_eq!(send(&r, VOLUNTEER, "POST", "/readings", &form).await.0, StatusCode::FORBIDDEN);
    let hens = uuid::Uuid::new_v4();
    let feed = format!("subject=animal:{hens}&kind=feed+eaten&values=3+lb");
    assert_eq!(send(&r, BOARD, "POST", "/readings", &feed).await.0, StatusCode::FORBIDDEN, "needs animals.write");
}

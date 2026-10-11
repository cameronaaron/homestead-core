//! Moving an animal through the forms, and its history on its page and the places'.

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

async fn send(r: &Router, who: &str, method: &str, uri: &str, form: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, format!("sbh_session={who}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form.to_owned()))
        .expect("req");
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let status = resp.status();
    (status, String::from_utf8_lossy(&resp.into_body().collect().await.expect("b").to_bytes()).into_owned())
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_goat_moved_between_pastures_shows_where_it_has_been(pool: PgPool) {
    common::steward(&pool).await;
    common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    let store = Store::from_pool(pool.clone());
    let a = store.create_location(NewLocation::named("Pasture A", LocationKind::AnimalArea)).await.expect("a");
    let b = store.create_location(NewLocation::named("Pasture B", LocationKind::AnimalArea)).await.expect("b");
    let r = app(AppState::new(Store::from_pool(pool)));
    let st = common::STEWARD;
    let form =
        "name=Clover&species=goat&head_count=1&sex=female&status=resident&origin=purchased&arrived_on=2026-07-01";
    assert_eq!(send(&r, st, "POST", "/animals", form).await.0, StatusCode::SEE_OTHER);
    let (_, list) = send(&r, st, "GET", "/animals", "").await;
    let at = list.find("href=\"/animals/").expect("link") + 6;
    let goat = &list[at..at + list[at..].find('"').expect("end")];

    let mv = |to: uuid::Uuid, at: &str, note: &str| format!("to={to}&at={at}&note={}", note.replace(' ', "+"));
    let (s, body) = send(&r, st, "POST", &format!("{goat}/move"), &mv(a.id, "2026-08-01T09:00", "")).await;
    assert_eq!(s, StatusCode::SEE_OTHER, "{body}");
    let (s, _) = send(&r, st, "POST", &format!("{goat}/move"), &mv(b.id, "2026-08-15T09:00", "A grazed down")).await;
    assert_eq!(s, StatusCode::SEE_OTHER);

    let (_, page) = send(&r, st, "GET", goat, "").await;
    assert!(page.contains("Where it's been") && page.contains("A grazed down"), "{page}");
    assert!(page.contains("2026-08-01") && page.contains("14 days"), "two weeks on Pasture A");
    assert!(page.find("Pasture B") < page.rfind("Pasture A"), "newest first");

    // Out of order is refused with the reason.
    let (s, body) = send(&r, st, "POST", &format!("{goat}/move"), &mv(a.id, "2026-08-10T09:00", "")).await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("record moves in order"));

    let (_, pasture) = send(&r, st, "GET", &format!("/locations/{}", a.id), "").await;
    assert!(pasture.contains("Moved in and out") && pasture.contains("Pasture A → Pasture B"), "{pasture}");

    // A volunteer sees the history but isn't offered the move.
    let (_, vpage) = send(&r, VOLUNTEER, "GET", goat, "").await;
    assert!(vpage.contains("Where it's been") && !vpage.contains("/move\""));
    assert_eq!(send(&r, VOLUNTEER, "POST", &format!("{goat}/move"), &mv(a.id, "", "")).await.0, StatusCode::FORBIDDEN);
}

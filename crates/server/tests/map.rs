//! The farm map through the forms: draw a place, see it on the map, open it.

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
async fn a_place_drawn_on_its_page_shows_on_the_map_with_what_grows_there(pool: PgPool) {
    common::steward(&pool).await;
    common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    let store = Store::from_pool(pool.clone());
    let garden = store.create_location(NewLocation::named("Kitchen garden", LocationKind::Garden)).await.expect("g");
    let bed = store
        .create_location(NewLocation { parent_id: Some(garden.id), ..NewLocation::named("Bed A", LocationKind::Bed) })
        .await
        .expect("bed");
    let today = store.today().await.expect("today");
    store.create_planting(NewPlanting::at(bed.id, "Tomato", today)).await.expect("tomato");
    let r = app(AppState::new(Store::from_pool(pool)));
    let st = common::STEWARD;

    let (_, empty) = send(&r, st, "GET", "/map", "").await;
    assert!(empty.contains("Nothing is on the map yet"), "{empty}");

    let draw = |id: uuid::Uuid, how: &str, pts: &str| {
        (format!("/locations/{id}/shape"), format!("how={how}&points={}", pts.replace(' ', "+").replace(',', "%2C")))
    };
    for (uri, form) in [draw(garden.id, "rectangle", "0,0 60,40"), draw(bed.id, "rectangle", "10,20 14,32")] {
        let (s, body) = send(&r, st, "POST", &uri, &form).await;
        assert_eq!(s, StatusCode::SEE_OTHER, "{body}");
    }
    let (uri, bow_tie) = draw(bed.id, "outline", "0,0 10,10 10,0 0,10");
    let (s, body) = send(&r, st, "POST", &uri, &bow_tie).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "a crossed outline is refused");
    assert!(body.contains("edges cross"), "and says why: {body}");

    let (s, map) = send(&r, st, "GET", "/map", "").await;
    assert_eq!(s, StatusCode::OK);
    let bed_link = format!("href=\"/locations/{}\"", bed.id);
    assert!(map.contains("<svg") && map.contains(&bed_link), "the bed is a link on the plan: {map}");
    assert!(map.contains("Bed A — growing Tomato"), "its hover text says what grows there");
    assert!(map.contains("48 sq ft"), "4 × 12 ft, measured from the outline");
    // Outer places are drawn first, so the bed sits on top of the garden and gets the tap.
    assert!(map.find(&format!("/locations/{}", garden.id)) < map.find(&bed_link));

    let (_, page) = send(&r, st, "GET", &format!("/locations/{}", bed.id), "").await;
    assert!(page.contains("On the map") && page.contains("shape grow here"), "{page}");
    assert!(page.contains("The outline measures 48 sq ft") && page.contains("10,20 14,20 14,32 10,32"));

    // A volunteer reads the map but can't redraw it.
    let (s, vmap) = send(&r, VOLUNTEER, "GET", "/map", "").await;
    assert_eq!(s, StatusCode::OK);
    assert!(vmap.contains(&bed_link));
    let (_, vpage) = send(&r, VOLUNTEER, "GET", &format!("/locations/{}", bed.id), "").await;
    assert!(!vpage.contains("/shape\""), "no drawing form for a volunteer");
    let (uri, form) = draw(bed.id, "point", "1,1");
    assert_eq!(send(&r, VOLUNTEER, "POST", &uri, &form).await.0, StatusCode::FORBIDDEN);

    // Taking it off the map.
    let (s, _) = send(&r, st, "POST", &format!("/locations/{}/shape", bed.id), "how=none").await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    let (_, map) = send(&r, st, "GET", "/map", "").await;
    assert!(!map.contains(&bed_link) && map.contains("1 place in use isn't on the map yet"), "{map}");
}

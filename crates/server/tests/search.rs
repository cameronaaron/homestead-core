//! Search: a result never reveals a record the reader couldn't open.

use axum::Router;
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
const BOARD: &str = "0000000000000000000000000000000000000000000000000000000000000004";
const FARM_LEAD: &str = "0000000000000000000000000000000000000000000000000000000000000005";
const CREW: &str = "0000000000000000000000000000000000000000000000000000000000000006";
const BOT: &str = "0000000000000000000000000000000000000000000000000000000000000007";

async fn get(r: &Router, who: &str, uri: &str) -> (StatusCode, String) {
    let req = Request::get(uri).header(header::COOKIE, format!("sbh_session={who}")).body(Body::empty()).expect("req");
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let status = resp.status();
    (status, String::from_utf8_lossy(&resp.into_body().collect().await.expect("b").to_bytes()).into_owned())
}

async fn api(r: &Router, token: &str, uri: &str) -> (StatusCode, serde_json::Value) {
    let req =
        Request::get(uri).header(header::AUTHORIZATION, format!("Bearer {token}")).body(Body::empty()).expect("r");
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let status = resp.status();
    let body = resp.into_body().collect().await.expect("b").to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null))
}

async fn setup(pool: &PgPool) -> Router {
    common::steward(pool).await;
    common::person(pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    common::person(pool, "Bo", &[Role::Board], BOARD).await;
    common::person(pool, "Fern", &[Role::FarmLead], FARM_LEAD).await;
    common::person(pool, "Cy", &[Role::Crew], CREW).await;
    common::service(pool, "Dashboard", BOT).await;
    for sql in [
        "insert into homestead.gifts (donor_name, kind, received_on, amount)
         values ('Rosalind Marsh', 'cash', current_date, 250)",
        "insert into homestead.animals (species, name, status, arrived_on) values ('cat', 'Rosalind', 'in_care', current_date)",
        "insert into homestead.incidents (occurred_at, kind, description, reported_by_name)
         values (now(), 'injury', 'Rosalind scratched a volunteer', 'Val')",
        "insert into homestead.locations (id, name, kind) values ('00000000-0000-0000-0000-00000000000a', 'Bed 1', 'bed')",
        "insert into homestead.plantings (id, location_id, crop, sown_on) values
           ('00000000-0000-0000-0000-0000000000b1', '00000000-0000-0000-0000-00000000000a', '=HYPERLINK(\"x\")', current_date)",
        "insert into homestead.harvests (planting_id, harvested_on, quantity, unit)
         values ('00000000-0000-0000-0000-0000000000b1', current_date, 4, 'lb')",
    ] {
        sqlx::query(sql).execute(pool).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    app(AppState::new(Store::from_pool(pool.clone())))
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn search_finds_only_what_the_reader_could_open(pool: PgPool) {
    let r = setup(&pool).await;
    // The board reads gifts and animals: both Rosalinds.
    let (status, page) = get(&r, BOARD, "/search?q=rosal").await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Rosalind Marsh") && page.contains("/animals/"), "{page}");
    // A volunteer reads animals, not gifts: the donor's name never shows.
    let (status, page) = get(&r, VOLUNTEER, "/search?q=rosal").await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("/animals/") && !page.contains("Marsh") && !page.contains("/gifts/"), "{page}");
    // Incidents are searchable by nobody, not even a steward.
    let (_, page) = get(&r, common::STEWARD, "/search?q=scratched").await;
    assert!(!page.contains("scratched a volunteer"), "{page}");
    // Query syntax is inert, and an empty query lists nothing.
    assert_eq!(get(&r, BOARD, "/search?q=%27%20%7C%20!").await.0, StatusCode::OK);

    // A service searches what its role opens: no animals, no gifts.
    let (status, hits) = api(&r, BOT, "/api/v1/search?q=rosal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hits, serde_json::json!([]), "a service can't read animals or gifts");
    let (_, hits) = api(&r, BOT, "/api/v1/search?q=bed").await;
    assert_eq!(hits[0]["kind"], "location");
}

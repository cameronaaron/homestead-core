//! The fiscal-year report: each reader sees only the sections they could
//! read the records of, and the CSV can't smuggle in a formula.

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
async fn the_report_shows_each_reader_only_their_sections(pool: PgPool) {
    let r = setup(&pool).await;
    // No reports.read: refused outright, even though crew read the farm.
    assert_eq!(get(&r, CREW, "/report").await.0, StatusCode::FORBIDDEN);
    assert_eq!(get(&r, VOLUNTEER, "/report.csv").await.0, StatusCode::FORBIDDEN);

    let (status, board) = get(&r, BOARD, "/report").await;
    assert_eq!(status, StatusCode::OK);
    assert!(board.contains("Gifts recorded here") && board.contains("$250.00"), "{board}");
    // A farm lead holds reports.read but not gifts.read: a one-gift total
    // would be that donor's gift, so the section isn't there at all.
    let (status, lead) = get(&r, FARM_LEAD, "/report").await;
    assert_eq!(status, StatusCode::OK);
    assert!(lead.contains("Harvest") && lead.contains("Incidents"), "{lead}");
    assert!(!lead.contains("Gifts recorded here") && !lead.contains("250"), "{lead}");
    assert!(lead.contains("can't read those records: gifts"), "says what's missing and why");

    let (status, json) = api(&r, BOT, "/api/v1/report").await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["gifts"].is_null() && json["incidents"].is_null() && json["animals"].is_null(), "{json}");
    assert_eq!(json["harvest"]["harvested"][0]["quantity"], "4.000");

    let (status, err) = api(&r, BOT, "/api/v1/report?year=1").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{err}");
    let (status, _) = api(&r, BOT, "/api/v1/report?year=2099").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "a year that hasn't started");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_report_csv_neutralises_formulas_but_keeps_numbers(pool: PgPool) {
    let r = setup(&pool).await;
    let (status, csv) = get(&r, BOARD, "/report.csv").await;
    assert_eq!(status, StatusCode::OK);
    assert!(csv.starts_with("section,metric,key,unit,value\n"), "{csv}");
    assert!(csv.contains("harvest,harvested,\"'=HYPERLINK(\"\"x\"\")\",lb,4\n"), "{csv}");
    assert!(csv.contains("gifts,cash,,USD,250\n"), "{csv}");
    assert!(!csv.lines().any(|l| l.split(',').any(|cell| cell.starts_with('='))), "{csv}");
}

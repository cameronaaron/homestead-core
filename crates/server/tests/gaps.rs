//! Welcome setup, remembered field entry, the carry file, the installable
//! shell, request metrics, and the Buffer channel checklist.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_domain::auth::Role;
use homestead_domain::equipment::{Condition, NewEquipment, NewMaintenance};
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::Duration;
use tower::ServiceExt;

mod common;

const LEAD: &str = "1ead5eeda1ead5eeda1ead5eeda1ead5eeda1ead5eeda1ead5eeda1ead5eeda1ead";

fn router(pool: PgPool) -> Router {
    app(AppState::new(Store::from_pool(pool)))
}

async fn send(r: &Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let (parts, body) = resp.into_parts();
    let bytes = body.collect().await.expect("body").to_bytes();
    (parts.status, parts.headers, String::from_utf8_lossy(&bytes).into_owned())
}

fn ui(cookie: &str, method: &str, uri: &str, form: Option<&str>) -> Request<Body> {
    let b = Request::builder().method(method).uri(uri).header(header::COOKIE, format!("sbh_session={cookie}"));
    match form {
        Some(f) => b.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded").body(Body::from(f.to_owned())),
        None => b.body(Body::empty()),
    }
    .expect("request")
}

fn form_json(json: &str) -> String {
    let mut out = String::from("document=");
    for b in json.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn welcome_sets_up_the_empty_farm_and_today_remembers_the_last_entry(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    common::person(&pool, "Lea", &[Role::FarmLead], LEAD).await;
    let r = router(pool.clone());

    let (_, _, welcome) = send(&r, ui(LEAD, "GET", "/welcome", None)).await;
    assert!(
        welcome.contains("First place") && welcome.contains("First flock") && welcome.contains("First checklist"),
        "{welcome}"
    );
    assert!(
        welcome.contains("Place name") && welcome.contains("Flock name") && welcome.contains("Checklist name"),
        "{welcome}"
    );

    let (status, headers, _) = send(&r, ui(LEAD, "POST", "/welcome/place", Some("name=North+bed&kind=bed"))).await;
    assert_eq!((status, headers[header::LOCATION].to_str().unwrap_or("")), (StatusCode::SEE_OTHER, "/welcome"));
    let (status, _, _) = send(&r, ui(LEAD, "POST", "/welcome/flock", Some("name=Laying+flock&head_count=8"))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (status, _, _) =
        send(&r, ui(LEAD, "POST", "/welcome/checklist", Some("name=Morning&items=Latch+the+gate%0AWater"))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let (_, _, again) = send(&r, ui(LEAD, "GET", "/welcome", None)).await;
    assert!(again.contains("Places are already recorded"), "{again}");
    assert!(
        again.contains("A flock is already recorded") && again.contains("A checklist is already recorded"),
        "{again}"
    );

    let places = store.list_locations().await.expect("places");
    assert_eq!(places[0].name, "North bed");
    let flocks = store.list_animals().await.expect("flocks");
    assert_eq!((flocks[0].name.as_str(), flocks[0].head_count), ("Laying flock", 8));

    let (_, _, today) = send(&r, ui(LEAD, "GET", "/", None)).await;
    assert!(
        today.contains("North bed") && today.contains("Latch the gate") && today.contains("Laying flock"),
        "{today}"
    );
    assert!(today.contains("manifest.webmanifest") && today.contains("field-queue"), "{today}");

    let (status, _, _) =
        send(&r, ui(LEAD, "POST", "/today/water", Some(&format!("location_id={}", places[0].id)))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, _, watered) = send(&r, ui(LEAD, "GET", "/", None)).await;
    assert!(watered.contains("Watered again") && watered.contains("Watered North bed again"), "{watered}");
    assert!(watered.contains("selected"), "the last place stays chosen: {watered}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn today_shows_service_and_obligations_that_are_due(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    common::steward(&pool).await;
    let today = store.today().await.expect("today");
    let tool = store
        .create_equipment(NewEquipment {
            name: "Tiller".into(),
            category: "tools".into(),
            manufacturer: String::new(),
            model: String::new(),
            serial_number: String::new(),
            purchased_on: None,
            purchase_cost: None,
            purchase_id: None,
            location_id: None,
            condition: Condition::Good,
            notes: String::new(),
        })
        .await
        .expect("tool");
    store
        .add_maintenance(
            tool.id,
            NewMaintenance {
                performed_on: today - Duration::days(1),
                work: "Oil".into(),
                cost: None,
                performed_by: String::new(),
                vendor_id: None,
                next_service_on: Some(today),
                notes: String::new(),
                remind: false,
            },
        )
        .await
        .expect("service");
    store.add_obligation("City water report", "City", Some(today), "").await.expect("obligation");

    let (_, _, page) = send(&router(pool), ui(common::STEWARD, "GET", "/", None)).await;
    assert!(page.contains("Service due") && page.contains("Tiller"), "{page}");
    assert!(page.contains("Obligations") && page.contains("City water report"), "{page}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn carry_file_creates_what_is_new_and_skips_what_is_here(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    common::steward(&pool).await;
    common::person(
        &pool,
        "Val",
        &[Role::Volunteer],
        "a01e5eeda01e5eeda01e5eeda01e5eeda01e5eeda01e5eeda01e5eeda01e5eed",
    )
    .await;
    store
        .create_location(homestead_domain::NewLocation::named("North bed", homestead_domain::LocationKind::Bed))
        .await
        .expect("bed");
    let r = router(pool);

    let (status, _, _) =
        send(&r, ui("a01e5eeda01e5eeda01e5eeda01e5eeda01e5eeda01e5eeda01e5eeda01e5eed", "GET", "/carry", None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, headers, file) = send(&r, ui(common::STEWARD, "GET", "/carry/export.json", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers[header::CONTENT_TYPE].to_str().unwrap_or("").contains("json"), "{headers:?}");
    assert!(file.contains("North bed"), "{file}");

    let incoming = r#"{"version":1,"locations":[{"name":"North bed","kind":"bed"},{"name":"South bed","kind":"bed"}],"animals":[{"name":"Ducks","species":"duck","head_count":3}],"inventory":[{"name":"Twine","unit":"roll","reorder_level":"1","on_hand":"4"}],"equipment":[{"name":"Wheelbarrow","category":"tools"}]}"#;
    let (status, _, page) = send(&r, ui(common::STEWARD, "POST", "/carry", Some(&form_json(incoming)))).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("<strong>4</strong>") && page.contains("<strong>1</strong>"), "{page}");

    let names: Vec<_> = store.list_locations().await.expect("places").into_iter().map(|l| l.name).collect();
    assert!(
        names.iter().any(|n| n == "South bed") && names.iter().filter(|n| *n == "North bed").count() == 1,
        "{names:?}"
    );
    let stock = store.list_inventory().await.expect("stock");
    assert_eq!(stock[0].on_hand, Decimal::from(4));

    let (status, _, page) = send(&r, ui(common::STEWARD, "POST", "/carry", Some(&form_json(incoming)))).await;
    assert!(page.contains("<strong>0</strong>"), "a second import creates nothing: {status} {page}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn shell_metrics_and_social_setup_are_reachable(pool: PgPool) {
    common::steward(&pool).await;
    let r = router(pool.clone());
    let open = |uri: &str| Request::builder().uri(uri).body(Body::empty()).expect("request");

    let (status, _, manifest) = send(&r, open("/manifest.webmanifest")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(manifest.contains("\"start_url\": \"/\"") && manifest.contains("Sun Brewed Homestead"), "{manifest}");

    let (status, _, worker) = send(&r, open("/sw.js")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(worker.contains("Authenticated pages stay on the network"), "{worker}");
    assert!(!worker.contains("/today"), "the worker does not cache Today: {worker}");

    let (status, _, offline) = send(&r, open("/offline")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(offline.contains("no connection"), "{offline}");
    // The app bar shows the brand mark, served from the cached shell icon.
    assert!(offline.contains(r#"src="/icon.svg""#), "{offline}");
    let (status, _, icon) = send(&r, open("/icon.svg")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(icon.contains("Sun Brewed Homestead mark"), "{icon}");
    assert!(worker.contains("caches.delete"), "a new shell icon replaces the cached one: {worker}");

    let (_, _, social) = send(&r, ui(common::STEWARD, "GET", "/ops/social", None)).await;
    assert!(social.contains("instagram") && social.contains("Business or Creator"), "{social}");
    assert!(social.contains("buffer-verify"), "{social}");

    let svc = common::service(&pool, "metrics", "metrics-token-metrics-token-metrics").await;
    let _ = svc;
    let req = Request::builder()
        .uri("/api/v1/metrics")
        .header(header::AUTHORIZATION, "Bearer metrics-token-metrics-token-metrics")
        .body(Body::empty())
        .expect("request");
    let (status, _, body) = send(&r, req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("homestead_http_requests_total{class=\"5xx\"}"), "{body}");
    assert!(body.contains("homestead_http_request_duration_seconds_bucket{le=\"+Inf\"}"), "{body}");
}

/// The age of the last restore drill is `extract(epoch …)`, which Postgres 14+
/// returns as `numeric`. Decoding that as `f64` failed the whole page with a
/// 500 the moment a first drill was recorded; while the table was empty the
/// value was NULL and the page looked fine, so nothing had ever caught it.
#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn metrics_report_the_age_of_a_recorded_restore_drill(pool: PgPool) {
    common::service(&pool, "metrics", "metrics-token-metrics-token-metrics").await;
    let r = router(pool.clone());
    let scrape = || {
        Request::builder()
            .uri("/api/v1/metrics")
            .header(header::AUTHORIZATION, "Bearer metrics-token-metrics-token-metrics")
            .body(Body::empty())
            .expect("request")
    };
    let age = |body: &str| -> f64 {
        body.lines()
            .find_map(|l| l.strip_prefix("homestead_last_restore_test_age_seconds "))
            .unwrap_or_else(|| panic!("no restore age line: {body}"))
            .parse()
            .unwrap_or_else(|e| panic!("restore age is not a number ({e}): {body}"))
    };

    let (status, _, body) = send(&r, scrape()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(age(&body).is_nan(), "no drill yet reads NaN: {body}");

    sqlx::query(
        "insert into homestead.restore_tests (backup_name, backup_sha256, postgres_version, integrity_ok, tested_at)
         values ('homestead-test.dump', repeat('a', 64), '17.11', true, now() - interval '3 days')",
    )
    .execute(&pool)
    .await
    .expect("record a drill");

    let (status, _, body) = send(&r, scrape()).await;
    assert_eq!(status, StatusCode::OK, "a recorded drill must not break the page: {body}");
    let three_days = 3.0 * 86_400.0;
    assert!((age(&body) - three_days).abs() < 3_600.0, "about three days old: {body}");
}

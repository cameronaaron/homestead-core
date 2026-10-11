//! Vendor → purchase → receipt → stock, through the forms and the API.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_domain::auth::Role;
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

mod common;

const CREW: &str = "0000000000000000000000000000000000000000000000000000000000000006";
const BOARD: &str = "0000000000000000000000000000000000000000000000000000000000000004";
const BOT: &str = "0000000000000000000000000000000000000000000000000000000000000007";

async fn send(r: &Router, req: Request<Body>) -> (StatusCode, Option<String>, String) {
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let (parts, body) = resp.into_parts();
    let loc = parts.headers.get(header::LOCATION).map(|v| v.to_str().expect("ascii").to_owned());
    (parts.status, loc, String::from_utf8_lossy(&body.collect().await.expect("b").to_bytes()).into_owned())
}

fn post(who: &str, uri: &str, form: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::COOKIE, format!("sbh_session={who}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form.to_owned()))
        .expect("req")
}

fn get(who: &str, uri: &str) -> Request<Body> {
    Request::get(uri).header(header::COOKIE, format!("sbh_session={who}")).body(Body::empty()).expect("req")
}

fn id_after<'a>(html: &'a str, prefix: &str) -> &'a str {
    let start = html.find(prefix).unwrap_or_else(|| panic!("no {prefix} in page")) + prefix.len();
    &html[start..start + 36]
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_delivery_is_bought_received_once_and_traceable(pool: PgPool) {
    common::steward(&pool).await;
    common::person(&pool, "Cam", &[Role::Crew], CREW).await;
    common::person(&pool, "Bo", &[Role::Board], BOARD).await;
    let r = app(AppState::new(Store::from_pool(pool.clone())));
    let s = common::STEWARD;

    send(&r, post(s, "/locations", "name=Feed+shed&kind=storage")).await;
    send(&r, post(s, "/inventory", "name=Layer+feed&unit=bag&reorder_level=2")).await;
    send(&r, post(s, "/vendors", "name=Valley+Feed&email=orders%40valleyfeed.example")).await;
    let (_, _, page) = send(&r, get(s, "/purchases")).await;
    let vendor = id_after(&page, "—</option><option value=\"").to_owned();
    let (status, to, _) = send(
        &r,
        post(
            s,
            "/purchases",
            &format!(
                "vendor_id={vendor}&purchased_on=2026-09-10&amount=%2486.50&category=Feed&external_reference=INV-2291"
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{vendor}");
    let purchase = to.expect("to purchase");

    let (_, _, page) = send(&r, get(s, &purchase)).await;
    let item = id_after(&page, "—</option><option value=\"").to_owned();
    send(&r, post(s, &format!("{purchase}/lines"), &format!("item_id={item}&quantity=4&unit_cost=19.50"))).await;
    send(&r, post(s, &format!("{purchase}/lines"), "description=Delivery&quantity=1&unit_cost=8")).await;
    let (_, _, page) = send(&r, get(s, &purchase)).await;
    assert!(page.contains("Lines add up to $86"), "{page}");
    let line = id_after(&page, "/purchase-lines/").to_owned();
    let shed = id_after(&page[page.find("— where? —").expect("location select")..], "option value=\"").to_owned();

    // Board members see purchases but can't receive; crew can't see them.
    assert_eq!(send(&r, get(BOARD, &purchase)).await.0, StatusCode::OK);
    let receive = format!("/purchase-lines/{line}/receive");
    assert_eq!(send(&r, post(BOARD, &receive, &format!("location_id={shed}"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(send(&r, get(CREW, &purchase)).await.0, StatusCode::FORBIDDEN);

    for _ in 0..2 {
        assert_eq!(send(&r, post(s, &receive, &format!("location_id={shed}"))).await.0, StatusCode::SEE_OTHER);
    }
    let (_, _, page) = send(&r, get(s, &format!("/inventory/{item}"))).await;
    assert!(page.contains(">4 bag<") || page.contains("4 bag"), "received once: {page}");
    assert!(page.contains("Where it came from") && page.contains("Valley Feed") && page.contains("Kiki"), "{page}");

    // Crew record use from the shed and see the ledger, without the money.
    let (status, _, _) =
        send(&r, post(CREW, &format!("/inventory/{item}/movements"), "kind=consumption&quantity=3&note=fed+flock"))
            .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, _, page) = send(&r, get(CREW, &format!("/inventory/{item}"))).await;
    assert!(page.contains("Cam") && page.contains("fed flock") && !page.contains("Where it came from"), "{page}");
    let (status, _, page) =
        send(&r, post(CREW, &format!("/inventory/{item}/movements"), "kind=consumption&quantity=5")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{page}");
    assert!(page.contains("not enough stock"));

    // Low stock fired once, when the flock's meal took it to 1 bag.
    let n: i64 = sqlx::query_scalar("select count(*) from homestead.events where kind = 'inventory.low_stock'")
        .fetch_one(&pool)
        .await
        .expect("n");
    assert_eq!(n, 1);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn services_record_purchases_and_stock_through_the_api(pool: PgPool) {
    common::service(&pool, "bookkeeping sync", BOT).await;
    let r = app(AppState::new(Store::from_pool(pool.clone())));
    let api = |method: &str, uri: &str, body: Value| {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {BOT}"))
            .header("idempotency-key", uuid::Uuid::new_v4().to_string())
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("req")
    };
    let (_, _, item) =
        send(&r, api("POST", "/api/v1/inventory", json!({"name": "Straw", "unit": "bale", "reorder_level": "0"})))
            .await;
    let item: Value = serde_json::from_str(&item).unwrap_or_else(|_| panic!("item: {item}"));
    let (_, _, p) = send(
        &r,
        api("POST", "/api/v1/purchases", json!({"purchased_on": "2026-09-12", "amount": "60", "category": "bedding"})),
    )
    .await;
    let p: Value = serde_json::from_str(&p).expect("purchase");
    let (status, _, l) = send(
        &r,
        api(
            "POST",
            &format!("/api/v1/purchases/{}/lines", p["id"].as_str().expect("id")),
            json!({"item_id": item["id"], "quantity": "10"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{l}");
    let l: Value = serde_json::from_str(&l).expect("line");
    let (status, _, body) =
        send(&r, api("POST", &format!("/api/v1/purchase-lines/{}/receive", l["id"].as_str().expect("id")), json!({})))
            .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, _, ledger) = send(
        &r,
        Request::get(format!("/api/v1/inventory/{}/movements", item["id"].as_str().expect("id")))
            .header(header::AUTHORIZATION, format!("Bearer {BOT}"))
            .body(Body::empty())
            .expect("req"),
    )
    .await;
    let ledger: Value = serde_json::from_str(&ledger).expect("ledger");
    assert_eq!(ledger["item"]["on_hand"], "10.000");
    assert_eq!(ledger["movements"][0]["kind"], "receipt");
    assert_eq!(ledger["movements"][0]["recorded_by"], "bookkeeping sync");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn today_warns_before_fast_moving_stock_runs_out(pool: PgPool) {
    common::person(&pool, "Cam", &[Role::Crew], CREW).await;
    let store = Store::from_pool(pool.clone());
    let new = |name: &str| homestead_domain::NewInventoryItem {
        name: name.into(),
        unit: "bag".into(),
        reorder_level: rust_decimal::Decimal::ONE,
    };
    let grit = store.create_inventory_item(new("Grit")).await.expect("item").id;
    let hose = store.create_inventory_item(new("Hose washers")).await.expect("item").id;
    // Grit: 7 in three days ago, a bag a day since, 4 left: four days.
    // Hose washers: plenty, and nobody's used any.
    for (item, kind, delta, days_ago) in
        [(grit, "adjustment", 7, 3), (grit, "consumption", -3, 1), (hose, "adjustment", 9, 3)]
    {
        sqlx::query(
            "insert into homestead.stock_movements (item_id, delta, reason, kind, at)
             values ($1, $2, 'test', $3, now() - make_interval(days => $4))",
        )
        .bind(item)
        .bind(rust_decimal::Decimal::from(delta))
        .bind(kind)
        .bind(days_ago)
        .execute(&pool)
        .await
        .expect("movement");
    }
    let r = app(AppState::new(store));
    let (_, _, today) = send(&r, get(CREW, "/")).await;
    assert!(today.contains("Runs out within a week") && today.contains("Grit: 4 bag, about 4 days"), "{today}");
    assert!(!today.contains("Hose washers:"), "unused stock never runs out");
    let (_, _, inventory) = send(&r, get(CREW, "/inventory")).await;
    assert!(inventory.contains("about 4 days") && inventory.contains("At this pace"), "{inventory}");
    let (_, _, item) = send(&r, get(CREW, &format!("/inventory/{grit}"))).await;
    assert!(item.contains("3 bag used in the last 3 days: that lasts about 4 days"), "{item}");
}

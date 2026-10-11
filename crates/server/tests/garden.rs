//! The garden's seed provenance through the forms.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;

mod common;

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_planting_traces_back_to_its_seed_lot(pool: PgPool) {
    common::steward(&pool).await;
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
    send("POST", "/locations", "name=Bed+4&kind=bed").await;
    let (status, lot, _) = send(
        "POST",
        "/seeds",
        "crop=Tomato&variety=Sungold&supplier=Baker+Creek&supplier_lot=A-2291&acquired_on=2026-01-15&quantity=30&unit=seeds&organic=on",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let lot = lot.expect("lot page");
    let (_, _, garden) = send("GET", "/garden", "").await;
    let select = &garden[garden.find("name=\"location_id\"").expect("location select")..];
    let bed = &select[select.find("<option value=\"").expect("bed option") + 15..][..36];
    let lot_id = &lot["/seeds/".len()..];
    let (status, _, body) = send(
        "POST",
        "/plantings",
        &format!("location_id={bed}&crop=Tomato&variety=Sungold&sown_on=2026-03-01&seed_lot_id={lot_id}&seed_quantity_used=12"),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    let (_, _, garden) = send("GET", "/garden", "").await;
    assert!(garden.contains(&format!("href=\"/seeds/{lot_id}\"")), "planting links to its lot");
    let (_, _, page) = send("GET", &lot, "").await;
    assert!(page.contains("Baker Creek") && page.contains("A-2291") && page.contains("2026-01-15"), "{page}");
    assert!(
        page.contains("18 of 30 seeds left") && page.contains("Bed 4") && page.contains("certified organic"),
        "{page}"
    );
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_bed_page_shows_its_rotation_and_can_be_retired(pool: PgPool) {
    common::steward(&pool).await;
    let store = Store::from_pool(pool.clone());
    let garden = store
        .create_location(homestead_domain::NewLocation::named("Kitchen garden", homestead_domain::LocationKind::Garden))
        .await
        .expect("garden");
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
    let (status, _) = send("POST", "/locations", &format!("name=A3&kind=bed&parent_id={}", garden.id)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let bed = store.list_locations().await.expect("list").into_iter().find(|l| l.name == "A3").expect("bed");
    assert!(bed.active, "a new place is in use");
    let (status, body) =
        send("POST", "/plantings", &format!("location_id={}&crop=Tomato&family=nightshade&sown_on=2026-04-01", bed.id))
            .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");

    let (_, page) = send("GET", &format!("/locations/{}?crop=Eggplant&family=nightshade", bed.id), "").await;
    assert!(page.contains("Kitchen garden") && page.contains("Tomato"), "{page}");
    assert!(page.contains("Eggplant or its family (nightshade) is in the ground here now."), "{page}");

    // The edit form leaves "In use" unchecked to retire the bed.
    let (status, _) =
        send("POST", &format!("/locations/{}", bed.id), &format!("name=A3&kind=bed&parent_id={}", garden.id)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(!store.get_location(bed.id).await.expect("bed").active);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_succession_plan_is_made_from_the_crops_page(pool: PgPool) {
    common::steward(&pool).await;
    let store = Store::from_pool(pool.clone());
    let bed = store
        .create_location(homestead_domain::NewLocation::named("A3", homestead_domain::LocationKind::Bed))
        .await
        .expect("bed");
    let today = store.today().await.expect("today");
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
    let (status, plan, body) = send(
        "POST",
        "/crops",
        &format!(
            "crop=Lettuce&variety=&family=aster&location_id={}&first_sow_on={today}&every_days=14&count=4\
             &method=direct&days_to_harvest=45&expected_yield=8&expected_yield_unit=lb",
            bed.id
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    let (_, _, page) = send("GET", &plan.expect("plan page"), "").await;
    assert_eq!(page.matches("<td>planned</td>").count(), 4, "{page}");
    assert!(page.contains("expected 8 lb"), "{page}");
    let (_, _, crops) = send("GET", "/crops", "").await;
    assert!(crops.contains("sow this week") && crops.contains("Lettuce"), "{crops}");
    let (_, _, bed_page) = send("GET", &format!("/locations/{}", bed.id), "").await;
    assert!(bed_page.contains("Planned here"), "{bed_page}");
}

//! The Volunteers page: everyone who can read Community sees who and how
//! many hours; only whoever manages volunteers sees contact details, edits
//! or merges.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_domain::auth::Role;
use homestead_domain::community::{NewVolunteerShift, VolunteerEdit, WorkingGroup};
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use rust_decimal::Decimal;
use sqlx::PgPool;
use tower::ServiceExt;

mod common;

const CREW: &str = "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";

async fn send(r: &Router, cookie: &str, method: &str, uri: &str, form: Option<&str>) -> (StatusCode, String) {
    let b = Request::builder().method(method).uri(uri).header(header::COOKIE, format!("sbh_session={cookie}"));
    let req = match form {
        Some(f) => b.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded").body(Body::from(f.to_owned())),
        None => b.body(Body::empty()),
    }
    .expect("request");
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn crew_see_hours_and_leads_see_contact_details(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    common::steward(&pool).await;
    common::person(&pool, "Cal", &[Role::Crew], CREW).await;
    let r = app(AppState::new(store.clone()));
    let today = store.today().await.expect("today");
    let shift = |name: &str| NewVolunteerShift {
        person_name: name.into(),
        worked_on: today,
        hours: Decimal::from(2),
        working_group: WorkingGroup::FarmLabor,
        event_id: None,
        notes: String::new(),
    };
    let sam = store.log_volunteer_shift(shift("Sam")).await.expect("sam").volunteer_id;
    let sam_r = store.log_volunteer_shift(shift("Sam R.")).await.expect("sam r").volunteer_id;
    let phone = VolunteerEdit {
        name: "Sam".into(),
        email: Some("sam@example.org".into()),
        phone: "555-0100".into(),
        active: true,
    };
    store.update_volunteer(sam, phone).await.expect("details");

    let (status, page) = send(&r, CREW, "GET", "/community/volunteers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Sam R."), "{page}");
    assert!(!page.contains("555-0100") && !page.contains("sam@example.org"), "crew don't see contact details");
    assert!(!page.contains("/merge"), "nor a merge they couldn't do");
    let (status, _) =
        send(&r, CREW, "POST", &format!("/community/volunteers/{sam}/merge"), Some(&format!("into={sam_r}"))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (_, page) = send(&r, common::STEWARD, "GET", "/community/volunteers", None).await;
    assert!(page.contains("555-0100") && page.contains("mailto:sam@example.org"), "{page}");
    let (status, _) = send(
        &r,
        common::STEWARD,
        "POST",
        &format!("/community/volunteers/{sam}/merge"),
        Some(&format!("into={sam_r}")),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, page) = send(&r, common::STEWARD, "GET", "/community/volunteers", None).await;
    assert!(page.contains("also “sam”") && page.contains(">4<"), "one Sam R. with both shifts: {page}");

    // The hours form suggests the names people already go by.
    let (_, community) = send(&r, common::STEWARD, "GET", "/community", None).await;
    assert!(community.contains(r#"<option value="Sam R.">"#), "{community}");
}

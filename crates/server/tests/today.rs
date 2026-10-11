//! Today: the work list, the short field forms, first login, and who gets pinged.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_domain::animals::*;
use homestead_domain::auth::Role;
use homestead_domain::incidents::NewIncident;
use homestead_domain::*;
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::Duration;
use time::macros::datetime;
use tower::ServiceExt;

mod common;

const VOLUNTEER: &str = "a01e5eeda01e5eeda01e5eeda01e5eeda01e5eeda01e5eeda01e5eeda01e5eed";

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

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn today_lists_the_work_and_the_field_forms_write_it(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let steward = common::steward(&pool).await;
    let volunteer = common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    let today = store.today().await.expect("today");
    let bed = store.create_location(NewLocation::named("North bed", LocationKind::Bed)).await.expect("bed");
    let mut kale = NewPlanting::at(bed.id, "Kale", today - Duration::days(2));
    kale.status = Some(PlantingStatus::Planned);
    store.create_planting(kale).await.expect("kale");
    let tomato =
        store.create_planting(NewPlanting::at(bed.id, "Tomato", today - Duration::days(40))).await.expect("tomato");
    store
        .create_task(NewTask { due_on: Some(today - Duration::days(1)), ..NewTask::titled("Weed the beds") })
        .await
        .expect("task");
    store
        .create_inventory_item(NewInventoryItem {
            name: "Lime".into(),
            unit: "bag".into(),
            reorder_level: Decimal::from(2),
        })
        .await
        .expect("lime");
    let flock = store
        .create_animal(NewAnimal {
            species: Species::Chicken,
            name: "Laying flock".into(),
            head_count: 8,
            sex: Sex::Female,
            status: AnimalStatus::InCare,
            origin: AnimalOrigin::Purchased,
            arrived_on: today,
            born_on: None,
            microchip: String::new(),
            notes: String::new(),
        })
        .await
        .expect("flock");
    let incident = store
        .acting_as(homestead_store::Actor { id: Some(steward.id), label: steward.display_name.clone() })
        .report_incident(NewIncident {
            occurred_at: datetime!(2026-09-01 16:00 UTC),
            location_id: Some(bed.id),
            location_detail: String::new(),
            kind: homestead_domain::incidents::IncidentKind::Hazard,
            description: "Loose board by the coop door".into(),
            immediate_action: String::new(),
            people_involved: String::new(),
            witnesses: String::new(),
            treatment: None,
        })
        .await
        .expect("incident");
    store.create_checklist("Morning", &["Latch the gate".into()]).await.expect("list");

    let r = router(pool.clone());
    let (_, _, page) = send(&r, ui(common::STEWARD, "GET", "/", None)).await;
    assert!(page.contains("Weed the beds"), "{page}");
    assert!(page.contains("Overdue"), "{page}");
    assert!(page.contains("Sow Kale in North bed, overdue"), "{page}");
    assert!(page.contains("Lime"), "{page}");
    assert!(page.contains(&incident.reference()), "{page}");
    assert!(page.contains("Skip to content"), "{page}");
    assert!(page.contains("aria-current=\"page\""), "{page}");
    assert!(page.contains("New here?"), "{page}");

    let (_, _, theirs) = send(&r, ui(VOLUNTEER, "GET", "/", None)).await;
    assert!(theirs.contains("Weed the beds"), "{theirs}");
    assert!(!theirs.contains(&incident.reference()), "a volunteer does not see someone else's incident: {theirs}");
    assert!(!theirs.contains("Log harvest"), "a volunteer cannot write the garden: {theirs}");

    async fn post(r: &Router, form: &str, body: &str) {
        let (status, headers, _) = send(r, ui(common::STEWARD, "POST", form, Some(body))).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{form}");
        assert_eq!(headers[header::LOCATION], "/");
    }
    post(&r, "/today/water", &format!("location_id={}", bed.id)).await;
    post(&r, "/today/harvest", &format!("planting_id={}&quantity=3&unit=lb", tomato.id)).await;
    post(&r, "/today/eggs", &format!("animal_id={}&collected=11", flock.id)).await;
    post(&r, "/today/loss", "reason=Aphids+on+the+kale").await;
    let templates = store.list_checklist_templates().await.expect("templates");
    post(&r, "/today/checklist", &format!("template_id={}", templates[0].0)).await;
    let items = store.checklist_items(today).await.expect("items");
    assert_eq!(items.len(), 1);
    post(&r, "/today/check", &format!("item_id={}", items[0].0)).await;
    let items = store.checklist_items(today).await.expect("items");
    assert!(items[0].3, "the gate latch is done");

    let (_, _, done) = send(&r, ui(common::STEWARD, "GET", "/", None)).await;
    assert!(!done.contains("Latch the gate"), "a finished checklist item leaves the list: {done}");

    let (_, _, welcome) = send(&r, ui(VOLUNTEER, "GET", "/welcome", None)).await;
    assert!(welcome.contains("Hi Val") && welcome.contains("You can add tasks"), "{welcome}");
    let (status, headers, _) = send(&r, ui(VOLUNTEER, "POST", "/welcome", Some(""))).await;
    assert_eq!((status, headers[header::LOCATION].to_str().unwrap_or("")), (StatusCode::SEE_OTHER, "/"));
    let (_, _, after) = send(&r, ui(VOLUNTEER, "GET", "/", None)).await;
    assert!(!after.contains("New here?"), "{after}");
    let _ = volunteer;
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn discord_pings_only_people_who_asked_and_can_see_the_record(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let steward = common::steward(&pool).await;
    let volunteer = common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    store.set_external_subject(steward.id, Some("discord:111")).await.expect("link");
    store.set_external_subject(volunteer.id, Some("discord:222")).await.expect("link");
    for who in [steward.id, volunteer.id] {
        store.set_notification(who, "discord", "incident.reported", true).await.expect("pref");
        store.set_notification(who, "discord", "task.created", true).await.expect("pref");
    }
    let incidents = store.discord_mentions("incident.reported").await.expect("incidents");
    assert_eq!(incidents, vec!["111".to_owned()], "a volunteer can file a report but not read others");
    let tasks = store.discord_mentions("task.created").await.expect("tasks");
    assert_eq!(tasks, vec!["111".to_owned(), "222".to_owned()]);

    store.set_notification(steward.id, "today", "task.created", false).await.expect("mute");
    store.create_task(NewTask::titled("Collect eggs")).await.expect("task");
    let lines = store.notices_for(steward.id, steward.capabilities(), 10).await.expect("notices");
    assert!(lines.iter().all(|l| !l.contains("Collect eggs")), "{lines:?}");
    let vals = store.notices_for(volunteer.id, volunteer.capabilities(), 10).await.expect("val");
    assert!(vals.iter().any(|l| l.contains("Collect eggs")), "{vals:?}");
}

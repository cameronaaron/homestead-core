//! Incident reports: anyone files, few read, nothing leaks.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_domain::auth::Role;
use homestead_integrations::blobs::Blobs;
use homestead_server::attachments::Storage;
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;

mod common;

const CREW: &str = "0000000000000000000000000000000000000000000000000000000000000006";
const VOLUNTEER: &str = "0000000000000000000000000000000000000000000000000000000000000003";
const FARM_LEAD: &str = "0000000000000000000000000000000000000000000000000000000000000005";
const BOT: &str = "0000000000000000000000000000000000000000000000000000000000000007";
const JPEG: &[u8] = b"\xFF\xD8\xFF\xE0\x00\x10JFIF\x00 a photo of the coop door latch";

async fn send(r: &Router, who: &str, method: &str, uri: &str, form: &str) -> (StatusCode, Option<String>, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, format!("sbh_session={who}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form.to_owned()))
        .expect("req");
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let (status, loc) =
        (resp.status(), resp.headers().get(header::LOCATION).map(|v| v.to_str().expect("a").to_owned()));
    (status, loc, String::from_utf8_lossy(&resp.into_body().collect().await.expect("b").to_bytes()).into_owned())
}

async fn upload(r: &Router, who: &str, incident: &str) -> StatusCode {
    let b = "xXb";
    let mut body =
        format!("--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"latch.jpg\"\r\n\r\n").into_bytes();
    body.extend(JPEG);
    body.extend(format!("\r\n--{b}--\r\n").as_bytes());
    let req = Request::post(format!("/attachments/incident/{incident}"))
        .header(header::COOKIE, format!("sbh_session={who}"))
        .header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={b}"))
        .body(Body::from(body))
        .expect("req");
    r.clone().oneshot(req).await.expect("infallible").status()
}

const REPORT: &str = "kind=animal_bite&occurred_at=2026-09-28T09%3A15&location_detail=north+coop+door\
                      &description=Rooster+spurred+Val%27s+hand+collecting+eggs&immediate_action=Washed+and+bandaged\
                      &people_involved=Val+%28volunteer%29&treatment=first_aid";

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_report_is_private_to_its_reporter_and_oversight(pool: PgPool) {
    common::person(&pool, "Cam", &[Role::Crew], CREW).await;
    common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    common::person(&pool, "Fern", &[Role::FarmLead], FARM_LEAD).await;
    common::service(&pool, "cron", BOT).await;
    let r = app(AppState {
        attachments: Some(Storage {
            blobs: Blobs::memory(),
            max_bytes: 1024 * 1024,
            link_secs: 300,
            retain_deleted_days: 30,
        }),
        ..AppState::new(Store::from_pool(pool.clone()))
    });

    let (status, to, _) = send(&r, CREW, "POST", "/incidents", REPORT).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let page = to.expect("report page");
    let id = page.trim_start_matches("/incidents/").to_owned();
    let (status, _, mine) = send(&r, CREW, "GET", &page, "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        mine.contains("Rooster spurred") && mine.contains("first_aid") && mine.contains("reported by Cam"),
        "{mine}"
    );
    assert!(!mine.contains("Close incident"), "reporters don't close their own");

    // The reporter can add evidence to their open report, but not remove it.
    assert_eq!(upload(&r, CREW, &id).await, StatusCode::SEE_OTHER);
    let (_, _, mine) = send(&r, CREW, "GET", &page, "").await;
    let att = &mine[mine.find("/attachments/").expect("file") + "/attachments/".len()..][..36];
    assert_eq!(send(&r, CREW, "GET", &format!("/attachments/{att}"), "").await.0, StatusCode::OK);
    assert_eq!(send(&r, CREW, "POST", &format!("/attachments/{att}/delete"), "").await.0, StatusCode::NOT_FOUND);

    // Another volunteer can report too, but can't see Cam's report or its photo.
    let (_, _, list) = send(&r, VOLUNTEER, "GET", "/incidents", "").await;
    assert!(list.contains("Your reports") && !list.contains(&id), "{list}");
    assert_eq!(send(&r, VOLUNTEER, "GET", &page, "").await.0, StatusCode::NOT_FOUND);
    assert_eq!(send(&r, VOLUNTEER, "GET", &format!("/attachments/{att}"), "").await.0, StatusCode::NOT_FOUND);
    assert_eq!(upload(&r, VOLUNTEER, &id).await, StatusCode::FORBIDDEN);
    // Services never see incidents.
    let api =
        Request::get("/api/v1/incidents").header(header::AUTHORIZATION, format!("Bearer {BOT}")).body(Body::empty());
    assert_eq!(r.clone().oneshot(api.expect("req")).await.expect("i").status(), StatusCode::FORBIDDEN);

    // The farm lead follows up; closing needs a corrective action first.
    let (status, _, body) = send(&r, FARM_LEAD, "POST", &format!("{page}/close"), "notes=done").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    send(&r, FARM_LEAD, "POST", &format!("{page}/follow-up"), "corrective_action=Gloves+by+the+coop+door").await;
    assert_eq!(
        send(&r, FARM_LEAD, "POST", &format!("{page}/close"), "notes=Gloves+hung").await.0,
        StatusCode::SEE_OTHER
    );
    let (_, _, closed) = send(&r, CREW, "GET", &page, "").await;
    assert!(closed.contains("Closed") && closed.contains("by Fern") && closed.contains("Gloves by the coop door"));
    assert_eq!(upload(&r, CREW, &id).await, StatusCode::FORBIDDEN, "no more evidence once closed");

    // What leaves the app says only that something happened.
    let payload: serde_json::Value =
        sqlx::query_scalar("select payload from homestead.events where kind = 'incident.reported'")
            .fetch_one(&pool)
            .await
            .expect("event");
    let text = payload.to_string();
    for private in ["Rooster", "Val", "coop", "Cam", "bandaged"] {
        assert!(!text.contains(private), "{private} leaked into the event: {text}");
    }
    assert_eq!(payload["kind"], "animal_bite");
}

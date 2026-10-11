//! The program pages (animals, gifts, community, farm stand) through the
//! full router against a real database.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;

mod common;
use common::STEWARD as TOKEN;

async fn router(pool: PgPool) -> Router {
    common::steward(&pool).await;
    app(AppState::new(Store::from_pool(pool)))
}

async fn send(r: &Router, req: Request<Body>) -> (StatusCode, Option<String>, String) {
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let (parts, body) = resp.into_parts();
    let bytes = body.collect().await.expect("body").to_bytes();
    let location = parts.headers.get(header::LOCATION).map(|v| v.to_str().expect("ascii").to_owned());
    (parts.status, location, String::from_utf8_lossy(&bytes).into_owned())
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).header(header::COOKIE, format!("sbh_session={TOKEN}")).body(Body::empty()).expect("req")
}

fn post(uri: &str, form: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::COOKIE, format!("sbh_session={TOKEN}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form.to_owned()))
        .expect("req")
}

const CAT: &str = "name=Mochi&species=cat&head_count=1&sex=female&status=in_care&origin=trapped&arrived_on=2026-09-01";

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_cat_can_be_added_cared_for_and_adopted_from_the_forms(pool: PgPool) {
    let r = router(pool.clone()).await;
    let (status, to, _) = send(&r, post("/animals", CAT)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let detail = to.expect("redirect to the new animal");
    assert!(detail.starts_with("/animals/"), "{detail}");

    let care = "occurred_on=2026-09-05&kind=spay_neuter&provider=County+TNR&cost=%2445";
    assert_eq!(send(&r, post(&format!("{detail}/care"), care)).await.0, StatusCode::SEE_OTHER);
    let adopt = "kind=adoption&person_name=Jordan&started_on=2026-09-27&fee=75";
    assert_eq!(send(&r, post(&format!("{detail}/placements"), adopt)).await.0, StatusCode::SEE_OTHER);

    let (status, _, html) = send(&r, get(&detail)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("adopted") && html.contains("spay_neuter") && html.contains("$45"), "{html}");
    let kinds: Vec<String> =
        sqlx::query_scalar("select kind from homestead.events").fetch_all(&pool).await.expect("ev");
    assert_eq!(kinds, ["animal.adopted"]);

    let (_, _, list) = send(&r, get("/animals")).await;
    assert!(list.contains("Placed, released or passed") && list.contains("Mochi"));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn animal_forms_reject_bad_input_without_writing(pool: PgPool) {
    let r = router(pool.clone()).await;
    let litter = CAT.replace("head_count=1", "head_count=4");
    assert_eq!(send(&r, post("/animals", &litter)).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let no_date = CAT.replace("arrived_on=2026-09-01", "arrived_on=");
    assert_eq!(send(&r, post("/animals", &no_date)).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let n: i64 = sqlx::query_scalar("select count(*) from homestead.animals").fetch_one(&pool).await.expect("n");
    assert_eq!(n, 0);
    let missing = format!("/animals/{}/placements", uuid::Uuid::new_v4());
    assert_eq!(
        send(&r, post(&missing, "kind=adoption&person_name=X&started_on=2026-09-27")).await.0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn names_are_escaped_on_every_page(pool: PgPool) {
    let r = router(pool).await;
    let evil = CAT.replace("name=Mochi", "name=%3Cscript%3Ealert(1)%3C%2Fscript%3E");
    let (_, to, _) = send(&r, post("/animals", &evil)).await;
    for page in ["/animals".to_owned(), to.expect("redirect")] {
        let (_, _, html) = send(&r, get(&page)).await;
        assert!(!html.contains("<script>alert"), "{page} rendered raw HTML");
        assert!(html.contains("&lt;script&gt;"), "{page}");
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn an_in_kind_gift_gets_a_letter_that_never_values_it(pool: PgPool) {
    let r = router(pool).await;
    let gift = "donor_name=Green+Thumb+Nursery&kind=in_kind&received_on=2026-09-18\
                &description=40+vegetable+starts&estimated_value=%24400&fund=Community+workshops";
    assert_eq!(send(&r, post("/gifts", gift)).await.0, StatusCode::SEE_OTHER);
    let (_, _, html) = send(&r, get("/gifts")).await;
    assert!(html.contains("Owed a thank-you letter"), "an unacknowledged $400 gift is owed a letter");
    let start = html.find("/gifts/").expect("letter link") + "/gifts/".len();
    let id = &html[start..start + 36];

    let (status, _, letter) = send(&r, get(&format!("/gifts/{id}/letter"))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(letter.contains("40 vegetable starts") && letter.contains("92-0510478"));
    // The page links back to the gift by its random UUID, which can itself
    // contain "400"; only the letter's own text is under test.
    let letter_text = letter.split("class=\"card letter\"").nth(1).expect("letter body");
    let letter_text = letter_text.split("class=\"noprint\"").next().unwrap_or(letter_text);
    assert!(!letter_text.replace(id, "").contains("400"), "the letter must not value donated property");

    let (status, to, _) = send(&r, post(&format!("/gifts/{id}/acknowledged"), "on=2026-09-20")).await;
    assert_eq!((status, to.as_deref()), (StatusCode::SEE_OTHER, Some(format!("/gifts/{id}/letter").as_str())));
    let (_, _, html) = send(&r, get("/gifts")).await;
    assert!(!html.contains("Owed a thank-you letter") && html.contains("sent 2026-09-20"));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_cash_gift_without_an_amount_is_refused(pool: PgPool) {
    let r = router(pool).await;
    let (status, _, _) = send(&r, post("/gifts", "donor_name=X&kind=cash&received_on=2026-09-18")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn events_and_volunteer_hours_add_up_on_the_community_page(pool: PgPool) {
    let r = router(pool).await;
    let today = time::OffsetDateTime::now_utc().date();
    let event = format!("title=Work+day%3A+coop+build&kind=work_day&held_on={today}&time_note=1pm");
    assert_eq!(send(&r, post("/community/events", &event)).await.0, StatusCode::SEE_OTHER);
    for (who, hours, group) in
        [("Ricardo", "3", "farm_labor"), ("Randy", "2.5", "farm_labor"), ("Hola", "1", "social_media")]
    {
        let form = format!("person_name={who}&worked_on={today}&hours={hours}&working_group={group}");
        assert_eq!(send(&r, post("/community/hours", &form)).await.0, StatusCode::SEE_OTHER, "{who}");
    }
    let (_, _, html) = send(&r, get("/community")).await;
    assert!(html.contains("Work day: coop build"));
    assert!(html.contains(">6.5<"), "total hours: {html}");
    assert!(html.contains("Farm &amp; Labor · 2 people"), "{html}");
    let bad = format!("person_name=X&worked_on={today}&hours=30&working_group=other");
    assert_eq!(send(&r, post("/community/hours", &bad)).await.0, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn stand_sales_and_compost_roll_up(pool: PgPool) {
    let r = router(pool).await;
    let today = time::OffsetDateTime::now_utc().date();
    for (item, qty, unit, paid) in
        [("eggs", "2", "dozen", "%2414"), ("Eggs", "1", "dozen", "7"), ("Kale", "1", "bunch", "4")]
    {
        let form = format!("item={item}&quantity={qty}&unit={unit}&amount={paid}&payment=cash&sold_on={today}");
        assert_eq!(send(&r, post("/stand/sales", &form)).await.0, StatusCode::SEE_OTHER, "{item}");
    }
    let compost = format!("contributor=Cafe&weight_lb=12.5&received_on={today}");
    assert_eq!(send(&r, post("/stand/compost", &compost)).await.0, StatusCode::SEE_OTHER);
    let (_, _, html) = send(&r, get("/stand")).await;
    assert!(html.contains("$25.00") || html.contains("$25<"), "month total: {html}");
    assert!(html.contains("3 dozen"), "eggs grouped across spellings: {html}");
    assert!(html.contains("12.5 lb weighed"), "{html}");
    let bad = format!("item=Eggs&quantity=0&unit=dozen&amount=7&payment=cash&sold_on={today}");
    assert_eq!(send(&r, post("/stand/sales", &bad)).await.0, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_mailing_list_imports_honours_opt_outs_and_exports_safely(pool: PgPool) {
    let r = router(pool).await;
    let add = "name=Kiki&email=Kiki%40Example.org&interests=workshops&interests=cat_adoption&source=Farm+stand";
    assert_eq!(send(&r, post("/contacts", add)).await.0, StatusCode::SEE_OTHER);
    assert_eq!(
        send(&r, post("/contacts/unsubscribe", "email=gone%40example.org")).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let add_gone = "email=gone%40example.org&source=website";
    assert_eq!(send(&r, post("/contacts", add_gone)).await.0, StatusCode::SEE_OTHER);
    assert_eq!(send(&r, post("/contacts/unsubscribe", "email=gone%40example.org")).await.0, StatusCode::SEE_OTHER);

    let csv = "name,email,interests\n=HYPERLINK(\"x\"),evil%40example.org,newsletter\n,gone%40example.org,newsletter";
    let form = format!(
        "source=website&csv={}",
        csv.replace('\n', "%0A").replace('"', "%22").replace('=', "%3D").replace(',', "%2C")
    );
    let (status, _, html) = send(&r, post("/contacts/import", &form)).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("1 added") && html.contains("1 left unsubscribed"), "{html}");

    let (status, _, body) = send(&r, get("/contacts/export.csv")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("kiki@example.org") && body.contains("cat_adoption;workshops"), "{body}");
    assert!(!body.contains("gone@example.org"), "an opt-out was exported");
    assert!(body.contains("'=HYPERLINK"), "formula not neutralised: {body}");
    let (_, _, cats) = send(&r, get("/contacts/export.csv?interest=cat_adoption")).await;
    assert!(cats.contains("kiki@example.org") && !cats.contains("evil@example.org"));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn an_event_page_takes_rsvps_and_can_be_cancelled(pool: PgPool) {
    let r = router(pool.clone()).await;
    let today = time::OffsetDateTime::now_utc().date();
    let event = format!("title=Fall+Volunteer+Day&kind=work_day&held_on={today}");
    assert_eq!(send(&r, post("/community/events", &event)).await.0, StatusCode::SEE_OTHER);
    let hours = format!("person_name=Ava&worked_on={today}&hours=1&working_group=farm_labor");
    assert_eq!(send(&r, post("/community/hours", &hours)).await.0, StatusCode::SEE_OTHER);
    let event_id: uuid::Uuid =
        sqlx::query_scalar("select id from homestead.community_events").fetch_one(&pool).await.expect("event");
    let volunteer_id: uuid::Uuid =
        sqlx::query_scalar("select id from homestead.volunteers").fetch_one(&pool).await.expect("volunteer");
    let page = format!("/community/events/{event_id}");

    let (status, _, html) = send(&r, get(&page)).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("Fall Volunteer Day"), "{html}");

    let rsvp = format!("volunteer_id={volunteer_id}&availability=attending&note=after+lunch");
    assert_eq!(send(&r, post(&format!("{page}/rsvps"), &rsvp)).await.0, StatusCode::SEE_OTHER);
    let (_, _, html) = send(&r, get(&page)).await;
    assert!(html.contains("Ava") && html.contains("after lunch"), "the answer shows on the page: {html}");

    // Bad input is refused, not a crash, and changes nothing.
    let bad = |form: &str| send(&r, post(&format!("{page}/rsvps"), form));
    assert_eq!(bad("volunteer_id=nobody&availability=attending").await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        bad(&format!("volunteer_id={volunteer_id}&availability=perhaps")).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(send(&r, post(&format!("{page}/status"), "status=postponed")).await.0, StatusCode::UNPROCESSABLE_ENTITY);

    assert_eq!(send(&r, post(&format!("{page}/status"), "status=cancelled")).await.0, StatusCode::SEE_OTHER);
    let status: String =
        sqlx::query_scalar("select status from homestead.community_events").fetch_one(&pool).await.expect("status");
    assert_eq!(status, "cancelled");
    let (status, _, _) = send(&r, get(&format!("/community/events/{}", uuid::Uuid::new_v4()))).await;
    assert_ne!(status, StatusCode::OK, "an unknown event has no page");
}

//! Animals through the forms: scanning a chip, and what a chip is registered to.

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

const CREW: &str = "0000000000000000000000000000000000000000000000000000000000000006";
const VOLUNTEER: &str = "0000000000000000000000000000000000000000000000000000000000000003";

async fn send(r: &Router, method: &str, uri: &str, who: &str, form: &str) -> (StatusCode, Option<String>, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, format!("sbh_session={who}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form.to_owned()))
        .expect("req");
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let loc = resp.headers().get(header::LOCATION).map(|v| v.to_str().expect("ascii").to_owned());
    let status = resp.status();
    (status, loc, String::from_utf8_lossy(&resp.into_body().collect().await.expect("body").to_bytes()).into_owned())
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_scanned_chip_opens_its_animal_or_says_how_to_find_the_owner(pool: PgPool) {
    common::person(&pool, "Cam", &[Role::Crew], CREW).await;
    common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    let r = app(AppState::new(Store::from_pool(pool)));
    let cat = "name=Mochi&species=cat&head_count=1&sex=female&status=in_care&origin=trapped&arrived_on=2026-09-01\
               &microchip=985+112+003+456+789";
    let (status, page, body) = send(&r, "POST", "/animals", CREW, cat).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    let page = page.expect("animal page");

    // A reader in keyboard mode types the number and presses Enter.
    let (status, to, _) = send(&r, "GET", "/animals/chip?number=985112003456789", VOLUNTEER, "").await;
    assert_eq!((status, to.as_deref()), (StatusCode::SEE_OTHER, Some(page.as_str())));
    let list = send(&r, "GET", "/animals", VOLUNTEER, "").await.2;
    assert!(list.contains("action=\"/animals/chip\""), "the scan box is on the list");

    // A stray with someone else's chip: how to find the owner, and a quick intake.
    let (status, _, stray) = send(&r, "GET", "/animals/chip?number=900-164-001-234-567", CREW, "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(stray.contains("900164001234567") && stray.contains("petmicrochiplookup.org"), "{stray}");
    assert!(stray.contains("Record it as a new arrival"));
    let (_, _, stray) = send(&r, "GET", "/animals/chip?number=900164001234567", VOLUNTEER, "").await;
    assert!(!stray.contains("Record it as a new arrival"), "volunteers can't add animals");
    let (status, _, _) = send(&r, "GET", "/animals/chip?number=12ab", CREW, "").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Adopted but still registered to us: the page says so.
    let id = "chip_registry=Found.org&chip_registered_to=us&band=&microchip=985112003456789";
    assert_eq!(send(&r, "POST", &format!("{page}/identity"), CREW, id).await.0, StatusCode::SEE_OTHER);
    let adopt = "kind=adoption&person_name=Randy&started_on=2026-09-20&fee=75";
    assert_eq!(send(&r, "POST", &format!("{page}/placements"), CREW, adopt).await.0, StatusCode::SEE_OTHER);
    let (_, _, detail) = send(&r, "GET", &page, CREW, "").await;
    assert!(detail.contains("should be registered to the adopter"), "{detail}");
    let (_, _, tasks) = send(&r, "GET", "/tasks", CREW, "").await;
    assert!(tasks.contains("Transfer Mochi's microchip to Randy"), "{tasks}");
    let moved = "chip_registry=Found.org&chip_registered_to=adopter&band=&microchip=985112003456789";
    send(&r, "POST", &format!("{page}/identity"), CREW, moved).await;
    assert!(!send(&r, "GET", &page, CREW, "").await.2.contains("should be registered to the adopter"));

    // Identity is animals.write.
    assert_eq!(send(&r, "POST", &format!("{page}/identity"), VOLUNTEER, moved).await.0, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn an_overcrowded_coop_shows_up_as_a_number(pool: PgPool) {
    use homestead_domain::{LocationKind, NewLocation};
    common::person(&pool, "Cam", &[Role::Crew], CREW).await;
    common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    let store = Store::from_pool(pool);
    let coop = store.create_location(NewLocation::named("Main coop", LocationKind::Coop)).await.expect("coop").id;
    let r = app(AppState::new(store));
    let flock = "name=Laying+flock&species=chicken&head_count=26&sex=female&status=resident&origin=purchased\
                 &arrived_on=2026-01-01";
    let flock = send(&r, "POST", "/animals", CREW, flock).await.1.expect("flock page");
    let cat = "name=Mochi&species=cat&head_count=1&sex=female&status=adopted&origin=trapped&arrived_on=2026-01-01";
    let cat = send(&r, "POST", "/animals", CREW, cat).await.1.expect("cat page");
    assert_eq!(
        send(&r, "POST", &format!("{flock}/home"), CREW, &format!("home={coop}")).await.0,
        StatusCode::SEE_OTHER
    );
    // Someone who's been adopted doesn't take up room.
    send(&r, "POST", &format!("{cat}/home"), CREW, &format!("home={coop}")).await;

    let place = format!("/locations/{coop}");
    let (_, _, page) = send(&r, "GET", &place, CREW, "").await;
    assert!(page.contains("26 animals live here") && page.contains("Holds (head)"), "{page}");
    assert_eq!(send(&r, "POST", &format!("{place}/capacity"), CREW, "capacity=18").await.0, StatusCode::SEE_OTHER);
    let (_, _, today) = send(&r, "GET", "/", CREW, "").await;
    assert!(today.contains("Over capacity") && today.contains("Main coop: 26 living there, room for 18"), "{today}");
    let (_, _, animals) = send(&r, "GET", "/animals", VOLUNTEER, "").await;
    assert!(animals.contains("8 too many"), "{animals}");
    assert!(send(&r, "GET", &flock, CREW, "").await.2.contains("lives in Main coop"));

    // Volunteers see it but can't change it; bad input is refused.
    assert_eq!(send(&r, "POST", &format!("{place}/capacity"), VOLUNTEER, "capacity=40").await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        send(&r, "POST", &format!("{place}/capacity"), CREW, "capacity=0").await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    // The new coop: room again, and the warning goes.
    send(&r, "POST", &format!("{place}/capacity"), CREW, "capacity=30").await;
    let (_, _, today) = send(&r, "GET", "/", CREW, "").await;
    assert!(!today.contains("Over capacity"));
    assert!(send(&r, "GET", "/animals", CREW, "").await.2.contains("4 more"));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_colony_counts_down_to_every_cat_fixed(pool: PgPool) {
    common::person(&pool, "Cam", &[Role::Crew], CREW).await;
    common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    let store = Store::from_pool(pool);
    let r = app(AppState::new(store.clone()));
    let colony = "name=Laundromat+lot&area=behind+the+laundromat&caretaker_name=Marisol\
                  &caretaker_contact=555-0100&estimated_size=6";
    assert_eq!(send(&r, "POST", "/colonies", CREW, colony).await.0, StatusCode::SEE_OTHER);
    assert_eq!(send(&r, "POST", "/colonies", CREW, colony).await.0, StatusCode::CONFLICT, "names are unique");
    let id = store.colony_progress().await.expect("colonies")[0].colony.id;

    let cat = |name: &str| {
        format!("name={name}&species=cat&head_count=1&sex=female&status=in_care&origin=trapped&arrived_on=2026-09-01")
    };
    let tipped = send(&r, "POST", "/animals", CREW, &cat("Tip")).await.1.expect("cat");
    let spayed = send(&r, "POST", "/animals", CREW, &cat("Spay")).await.1.expect("cat");
    let wild = send(&r, "POST", "/animals", CREW, &cat("Wild")).await.1.expect("cat");
    send(&r, "POST", &format!("{tipped}/tnr"), CREW, &format!("colony={id}&ear_tipped=on")).await;
    send(&r, "POST", &format!("{spayed}/tnr"), CREW, &format!("colony={id}")).await;
    send(&r, "POST", &format!("{spayed}/care"), CREW, "occurred_on=2026-09-10&kind=spay_neuter").await;
    send(&r, "POST", &format!("{wild}/tnr"), CREW, &format!("colony={id}")).await;

    let (_, _, lead) = send(&r, "GET", "/animals", CREW, "").await;
    assert!(lead.contains("2 of 3 recorded") && lead.contains("4 of about 6"), "{lead}");
    assert!(lead.contains("555-0100"), "crew can reach the caretaker");
    let (_, _, volunteer) = send(&r, "GET", "/animals", VOLUNTEER, "").await;
    assert!(volunteer.contains("Laundromat lot") && !volunteer.contains("555-0100"), "{volunteer}");
    assert!(send(&r, "GET", &tipped, CREW, "").await.2.contains("from the Laundromat lot colony · ear-tipped"));

    // The caretaker's new count moves the target.
    send(&r, "POST", &format!("/colonies/{id}/estimate"), CREW, "estimated_size=9").await;
    assert!(send(&r, "GET", "/animals", CREW, "").await.2.contains("7 of about 9"));

    // Only cats belong to a colony, and only crew and up assign one.
    let hen = "name=Hen&species=chicken&head_count=1&sex=female&status=resident&origin=purchased&arrived_on=2026-01-01";
    let hen = send(&r, "POST", "/animals", CREW, hen).await.1.expect("hen");
    assert_eq!(send(&r, "POST", &format!("{hen}/tnr"), CREW, &format!("colony={id}")).await.0, StatusCode::CONFLICT);
    assert_eq!(send(&r, "POST", &format!("{wild}/tnr"), VOLUNTEER, "ear_tipped=on").await.0, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn today_lists_care_that_is_due_and_one_tap_logs_it(pool: PgPool) {
    common::person(&pool, "Cam", &[Role::Crew], CREW).await;
    common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    let store = Store::from_pool(pool);
    let today = store.today().await.expect("today");
    let r = app(AppState::new(store));
    let cat = "name=Mochi&species=cat&head_count=1&sex=female&status=fostered&origin=trapped&arrived_on=2026-09-01";
    let mochi = send(&r, "POST", "/animals", CREW, cat).await.1.expect("cat");
    let course = format!("kind=medication&every_days=1&first_due_on={today}&details=Clavamox+0.5+ml");
    assert_eq!(send(&r, "POST", &format!("{mochi}/schedules"), CREW, &course).await.0, StatusCode::SEE_OTHER);
    assert_eq!(send(&r, "POST", &format!("{mochi}/schedules"), VOLUNTEER, &course).await.0, StatusCode::FORBIDDEN);

    let (_, _, page) = send(&r, "GET", "/", CREW, "").await;
    assert!(page.contains("Care due") && page.contains("(with their foster): medication — Clavamox 0.5 ml"), "{page}");
    assert!(!send(&r, "GET", "/", VOLUNTEER, "").await.2.contains("Care due"), "vet records stay medical-only");

    // "Given" logs today's dose, which moves the course on to tomorrow.
    let given = format!("occurred_on={today}&kind=medication&details=Clavamox+0.5+ml");
    assert_eq!(send(&r, "POST", &format!("{mochi}/care"), CREW, &given).await.0, StatusCode::SEE_OTHER);
    assert!(!send(&r, "GET", "/", CREW, "").await.2.contains("Care due"));
    let (_, _, detail) = send(&r, "GET", &mochi, CREW, "").await;
    let tomorrow = today.next_day().expect("date");
    assert!(detail.contains(&tomorrow.to_string()) && detail.contains("Coming up"), "{detail}");
}

async fn public(r: &Router, method: &str, uri: &str, form: &str) -> (StatusCode, Option<String>, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form.to_owned()))
        .expect("req");
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let loc = resp.headers().get(header::LOCATION).map(|v| v.to_str().expect("ascii").to_owned());
    let status = resp.status();
    (status, loc, String::from_utf8_lossy(&resp.into_body().collect().await.expect("body").to_bytes()).into_owned())
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_public_adoption_page_shows_little_and_takes_applications(pool: PgPool) {
    common::person(&pool, "Cam", &[Role::Crew], CREW).await;
    common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    let r = app(AppState::new(Store::from_pool(pool.clone())));
    let cat = |name: &str, status: &str| {
        format!(
            "name={name}&species=cat&head_count=1&sex=female&status={status}&origin=trapped&arrived_on=2026-09-01\
             &born_on=2025-03-01&notes=FIV+positive%2C+trapped+on+Elm+St"
        )
    };
    let mochi = send(&r, "POST", "/animals", CREW, &cat("Mochi", "adoptable")).await.1.expect("cat");
    let mochi_id = mochi.trim_start_matches("/animals/").to_owned();
    let kept = send(&r, "POST", "/animals", CREW, &cat("Kept", "in_care")).await.1.expect("cat");
    send(&r, "POST", &format!("{mochi}/blurb"), CREW, "blurb=Loves+a+sunny+windowsill").await;

    // No account needed; only what adopters should see.
    let (status, _, page) = public(&r, "GET", "/adopt", "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Mochi") && page.contains("Loves a sunny windowsill"), "{page}");
    assert!(page.contains("cat · female · about "), "an age from the birthday (exact wording: domain tests)");
    assert!(!page.contains("FIV") && !page.contains("Elm St"), "notes never reach the public page");
    assert!(!page.contains("Kept"), "only adoptable animals are listed");

    let application =
        format!("animal_id={mochi_id}&applicant_name=Randy&email=Randy%40example.org&about=One+other+cat");
    let (status, to, body) = public(&r, "POST", "/adopt", &application).await;
    assert_eq!((status, to.as_deref()), (StatusCode::SEE_OTHER, Some("/adopt?sent=1")), "{body}");
    public(&r, "POST", "/adopt", &application).await; // a double-submit
    public(&r, "POST", "/adopt", "applicant_name=Bot&email=bot%40spam.test&website=http%3A%2F%2Fspam").await;
    let kept_id = kept.trim_start_matches("/animals/");
    let sneaky = format!("animal_id={kept_id}&applicant_name=A&email=a%40b.org");
    assert_eq!(public(&r, "POST", "/adopt", &sneaky).await.0, StatusCode::NOT_FOUND, "not adoptable, not applicable");
    let rows: i64 = sqlx::query_scalar("select count(*) from homestead.adoption_applications")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(rows, 1, "one real application: no duplicate, no bot");

    // Leads are told the animal, never the applicant.
    let payload: serde_json::Value =
        sqlx::query_scalar("select payload from homestead.events where kind = 'adoption.applied'")
            .fetch_one(&pool)
            .await
            .expect("event");
    assert_eq!(payload, serde_json::json!({ "animal_name": "Mochi" }));

    assert_eq!(send(&r, "GET", "/applications", VOLUNTEER, "").await.0, StatusCode::FORBIDDEN);
    let (_, _, list) = send(&r, "GET", "/applications", CREW, "").await;
    assert!(list.contains("randy@example.org") && list.contains("One other cat"), "{list}");
    let id = &list[list.find("/applications/").expect("decide form") + "/applications/".len()..][..36];
    let decided = send(&r, "POST", &format!("/applications/{id}/decide"), CREW, "status=approved").await;
    assert_eq!(decided.0, StatusCode::SEE_OTHER);
    assert_eq!(
        send(&r, "POST", &format!("/applications/{id}/decide"), CREW, "status=declined").await.0,
        StatusCode::CONFLICT
    );
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn egg_labels_print_what_california_asks_for(pool: PgPool) {
    common::person(&pool, "Cam", &[Role::Crew], CREW).await;
    common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    let r = app(AppState::new(Store::from_pool(pool)));
    let uri = "/stand/egg-labels?packed_on=2026-09-29&size=extra_large&grade=aa&sell_by_days=30&count=12&copies=2";
    let (status, _, page) = send(&r, "GET", uri, CREW, "").await;
    assert_eq!(status, StatusCode::OK);
    for must in [
        "Extra Large Grade AA",
        "Shell Egg Food Safety Compliant",
        "One dozen",
        " · Packed 272 · Sell by 2026-10-29",
        "Sun Brewed Homestead Inc",
        "11333 Moorpark St PMB 216, Studio City, CA 91602", // oss-publish:allow: the org's IRS-listed mailbox
        "KEEP REFRIGERATED",
        "SAFE HANDLING INSTRUCTIONS: To prevent illness from bacteria",
    ] {
        assert!(page.contains(must), "missing {must:?}");
    }
    assert_eq!(page.matches("class=\"egg\"").count(), 2, "copies");
    assert_eq!(send(&r, "GET", "/stand/egg-labels?size=huge", CREW, "").await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(send(&r, "GET", "/stand/egg-labels", VOLUNTEER, "").await.0, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_stand_sign_names_egg_size_and_grade_and_shows_pay_codes(pool: PgPool) {
    common::person(&pool, "Cam", &[Role::Crew], CREW).await;
    let state = AppState {
        pay_links: std::sync::Arc::new([("Venmo @example".to_owned(), "https://venmo.com/u/example".to_owned())]),
        ..AppState::new(Store::from_pool(pool.clone()))
    };
    let r = app(state);
    let (status, _, sign) = send(&r, "GET", "/stand/sign?size=extra_large&grade=aa", CREW, "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(sign.contains("Eggs, Extra Large, Grade AA"), "an egg price names size and grade: {sign}");
    assert!(sign.contains("Pay: Venmo @example") && sign.matches("<svg").count() == 2, "pay + adopt codes");
    assert!(!sign.contains("No pay links are set"));
    let bare = app(AppState::new(Store::from_pool(pool)));
    assert!(send(&bare, "GET", "/stand/sign", CREW, "").await.2.contains("No pay links are set"));
}

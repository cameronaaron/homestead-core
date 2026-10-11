//! QR tags: printed by people who manage the thing, used by anyone holding one.

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

struct Reply {
    status: StatusCode,
    location: Option<String>,
    referrer: Option<String>,
    body: String,
}

async fn send(r: &Router, method: &str, uri: &str, cookie: Option<&str>, form: &str) -> Reply {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "farm.test")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, format!("sbh_session={c}"));
    }
    let resp = r.clone().oneshot(req.body(Body::from(form.to_owned())).expect("req")).await.expect("infallible");
    let h = |n| resp.headers().get(n).map(|v: &header::HeaderValue| v.to_str().expect("ascii").to_owned());
    let (status, location, referrer) = (resp.status(), h(header::LOCATION), h(header::REFERRER_POLICY));
    let body = String::from_utf8_lossy(&resp.into_body().collect().await.expect("body").to_bytes()).into_owned();
    Reply { status, location, referrer, body }
}

/// The code printed under the first label on the sheet.
fn first_code(sheet: &str) -> String {
    let at = sheet.find("/t/").expect("a tag link on the sheet") + 3;
    sheet[at..at + 10].to_owned()
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn anyone_holding_a_tools_tag_can_take_it_return_it_and_report_it(pool: PgPool) {
    common::person(&pool, "Cam", &[Role::Crew], CREW).await;
    let r = app(AppState::new(Store::from_pool(pool.clone())));
    let made = send(&r, "POST", "/equipment", Some(CREW), "name=Trap+3&category=traps&condition=good").await;
    let tool = made.location.expect("tool page");
    let tool_id = tool.trim_start_matches("/equipment/").to_owned();

    // Crew make the tag from the tool's page and get a printable sheet.
    let page = send(&r, "GET", &tool, Some(CREW), "").await;
    assert!(page.body.contains("QR tag"), "make-a-tag button");
    let made = send(&r, "POST", "/tags", Some(CREW), &format!("kind=equipment&target={tool_id}")).await;
    assert_eq!(made.location.as_deref(), Some("/tags?kind=equipment"));
    let sheet = send(&r, "GET", "/tags", Some(CREW), "").await.body;
    assert!(sheet.contains("<svg") && sheet.contains("Trap 3"), "a QR code on the sheet");
    assert!(sheet.contains("https://farm.test/t/"), "no public_url: this host, https since cookies are Secure");
    let code = first_code(&sheet);

    // A stranger scans it: no account, just a name.
    let scan = send(&r, "GET", &format!("/t/{code}"), None, "").await;
    assert_eq!(scan.status, StatusCode::OK);
    assert_eq!(scan.referrer.as_deref(), Some("no-referrer"), "the code stays out of other sites' logs");
    assert!(scan.body.contains("Check it out") && scan.body.contains("Your name"));
    assert!(!scan.body.contains("Open it in the app"), "no link into the app for strangers");
    let nameless = send(&r, "POST", &format!("/t/{code}/checkout"), None, "name=+").await;
    assert_eq!(nameless.status, StatusCode::UNPROCESSABLE_ENTITY);
    let out = send(&r, "POST", &format!("/t/{code}/checkout"), None, "name=Sam").await;
    assert_eq!(out.status, StatusCode::SEE_OTHER, "{}", out.body);
    let scan = send(&r, "GET", &out.location.expect("back to the tag"), None, "").await.body;
    assert!(scan.contains("Checked out to you") && scan.contains("With <strong>Sam</strong>"), "{scan}");
    let taken = send(&r, "POST", &format!("/t/{code}/checkout"), None, "name=Ava").await;
    assert_eq!(taken.status, StatusCode::CONFLICT);

    // The tool's page shows who has it.
    assert!(send(&r, "GET", &tool, Some(CREW), "").await.body.contains("Out with <strong>Sam</strong>"));
    assert!(send(&r, "GET", "/equipment", Some(CREW), "").await.body.contains("with Sam"));

    // A signed-in person returns it and reports a problem under their own name.
    let back = send(&r, "POST", &format!("/t/{code}/return"), Some(CREW), "note=door+sticks").await;
    assert_eq!(back.status, StatusCode::SEE_OTHER, "{}", back.body);
    let broke = send(&r, "POST", &format!("/t/{code}/problem"), None, "name=Ava&problem=spring+snapped").await;
    assert_eq!(broke.status, StatusCode::SEE_OTHER, "{}", broke.body);
    let tasks = send(&r, "GET", "/tasks", Some(CREW), "").await.body;
    assert!(tasks.contains("Fix Trap 3"), "a fix-it task: {tasks}");

    let who: Vec<(String, String)> = sqlx::query_as(
        "select resource_type, actor_label from homestead.audit_entries
         where resource_type in ('equipment_loans', 'tasks') order by id",
    )
    .fetch_all(&pool)
    .await
    .expect("audit");
    let tagged = format!("(tag {code})");
    assert_eq!(who[0], ("equipment_loans".into(), format!("Sam {tagged}")), "a stranger by the name they gave");
    assert_eq!(who[1], ("equipment_loans".into(), "Cam".into()), "a signed-in person as themselves");
    assert_eq!(who[2], ("tasks".into(), format!("Ava {tagged}")));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_retired_or_made_up_code_does_nothing(pool: PgPool) {
    common::person(&pool, "Cam", &[Role::Crew], CREW).await;
    let r = app(AppState::new(Store::from_pool(pool.clone())));
    let tool = send(&r, "POST", "/equipment", Some(CREW), "name=Loppers&category=hand+tools&condition=good")
        .await
        .location
        .expect("tool");
    let id = tool.trim_start_matches("/equipment/");
    send(&r, "POST", "/tags", Some(CREW), &format!("kind=equipment&target={id}")).await;
    let code = first_code(&send(&r, "GET", "/tags", Some(CREW), "").await.body);

    assert_eq!(send(&r, "GET", "/t/2222222222", None, "").await.status, StatusCode::NOT_FOUND);
    assert_eq!(send(&r, "GET", "/t/not-a-code", None, "").await.status, StatusCode::UNPROCESSABLE_ENTITY);
    // Codes read off a label are forgiving about case and dashes.
    let lower = format!("{}-{}", &code[..5], &code[5..]).to_lowercase();
    assert_eq!(send(&r, "GET", &format!("/t/{lower}"), None, "").await.status, StatusCode::OK);

    let retired = send(&r, "POST", &format!("/tags/{code}/retire"), Some(CREW), "").await;
    assert_eq!(retired.status, StatusCode::SEE_OTHER);
    assert_eq!(send(&r, "GET", &format!("/t/{code}"), None, "").await.status, StatusCode::NOT_FOUND);
    let out = send(&r, "POST", &format!("/t/{code}/checkout"), None, "name=Sam").await;
    assert_eq!(out.status, StatusCode::NOT_FOUND);
    let loans: i64 =
        sqlx::query_scalar("select count(*) from homestead.equipment_loans").fetch_one(&pool).await.expect("count");
    assert_eq!(loans, 0);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn only_people_who_manage_a_thing_can_tag_it(pool: PgPool) {
    common::person(&pool, "Cam", &[Role::Crew], CREW).await;
    common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    let r = app(AppState::new(Store::from_pool(pool)));
    let tool = send(&r, "POST", "/equipment", Some(CREW), "name=Saw&category=hand+tools&condition=good")
        .await
        .location
        .expect("tool");
    let id = tool.trim_start_matches("/equipment/");
    send(&r, "POST", "/tags", Some(CREW), &format!("kind=equipment&target={id}")).await;

    let tried = send(&r, "POST", "/tags", Some(VOLUNTEER), &format!("kind=equipment&target={id}")).await;
    assert_eq!(tried.status, StatusCode::FORBIDDEN);
    let sheet = send(&r, "GET", "/tags", Some(VOLUNTEER), "").await;
    assert_eq!(sheet.status, StatusCode::OK);
    assert!(!sheet.body.contains("/t/"), "a volunteer's sheet lists no codes");
    // Crew can't run the community side, so no gate tag either.
    let gate = send(&r, "POST", "/tags", Some(CREW), "kind=gate").await;
    assert_eq!(gate.status, StatusCode::FORBIDDEN);
}

mod things {
    use homestead_domain::animals::{AnimalOrigin, AnimalStatus, NewAnimal, Sex, Species};
    use homestead_domain::{LocationKind, NewInventoryItem, NewLocation, StockAdjustment};
    use homestead_store::Store;
    use rust_decimal::Decimal;
    use time::macros::date;
    use uuid::Uuid;

    pub async fn animal(store: &Store, name: &str, species: Species, head_count: i32) -> Uuid {
        let a = NewAnimal {
            species,
            name: name.into(),
            head_count,
            sex: Sex::Unknown,
            status: AnimalStatus::Resident,
            origin: AnimalOrigin::Purchased,
            arrived_on: date!(2026 - 01 - 01),
            born_on: None,
            microchip: String::new(),
            notes: String::new(),
        };
        store.create_animal(a).await.expect("animal").id
    }

    pub async fn place(store: &Store, name: &str, kind: LocationKind) -> Uuid {
        store.create_location(NewLocation::named(name, kind)).await.expect("place").id
    }

    pub async fn stocked(store: &Store, name: &str, count: i64) -> Uuid {
        let item = NewInventoryItem { name: name.into(), unit: "bag".into(), reorder_level: Decimal::from(2) };
        let id = store.create_inventory_item(item).await.expect("item").id;
        store
            .adjust_stock(id, StockAdjustment { delta: Decimal::from(count), reason: "count".into() })
            .await
            .expect("in");
        id
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_bin_a_coop_and_the_compost_pile_each_take_their_own_small_record(pool: PgPool) {
    use homestead_domain::LocationKind;
    use homestead_domain::animals::Species;
    use homestead_domain::tags::TagKind;
    common::person(&pool, "Cam", &[Role::Crew], CREW).await;
    let store = Store::from_pool(pool.clone());
    let r = app(AppState::new(store.clone()));

    // Feed bin: "used 2" comes off the ledger and can trip the reorder.
    let feed = things::stocked(&store, "Layer feed", 3).await;
    let bin = store.ensure_tag(TagKind::InventoryItem, Some(feed), "FEED234567").await.expect("tag").code;
    let page = send(&r, "GET", &format!("/t/{bin}"), None, "").await.body;
    assert!(page.contains("3 bag") && page.contains("Used some?"), "{page}");
    let used = send(&r, "POST", &format!("/t/{bin}/use"), None, "name=Sam&quantity=2").await;
    assert_eq!(used.status, StatusCode::SEE_OTHER, "{}", used.body);
    assert_eq!(store.get_inventory_item(feed).await.expect("item").on_hand, rust_decimal::Decimal::from(1));
    let page = send(&r, "GET", &format!("/t/{bin}"), None, "").await.body;
    assert!(page.contains("running low"), "{page}");
    let greedy = send(&r, "POST", &format!("/t/{bin}/use"), None, "name=Sam&quantity=5").await;
    assert_eq!(greedy.status, StatusCode::CONFLICT, "can't use what isn't there");
    let silly = send(&r, "POST", &format!("/t/{bin}/use"), None, "name=Sam&quantity=-1").await;
    assert_eq!(silly.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Coop: two collections in a day add up.
    let flock = things::animal(&store, "Laying flock", Species::Chicken, 26).await;
    let coop = store.ensure_tag(TagKind::Animal, Some(flock), "HENS234567").await.expect("tag").code;
    for (who, n) in [("Sam", 6), ("Ava", 5)] {
        let logged =
            send(&r, "POST", &format!("/t/{coop}/eggs"), None, &format!("name={who}&collected={n}&damaged=1")).await;
        assert_eq!(logged.status, StatusCode::SEE_OTHER, "{}", logged.body);
    }
    let page = send(&r, "GET", &format!("/t/{coop}"), None, "").await.body;
    assert!(page.contains("11 eggs") && page.contains("(2 cracked)"), "{page}");
    let typo = send(&r, "POST", &format!("/t/{coop}/eggs"), None, "name=Sam&collected=2000").await;
    assert_eq!(typo.status, StatusCode::UNPROCESSABLE_ENTITY);

    // A cat's tag takes no eggs, but does take a concern.
    let cat = things::animal(&store, "Mochi", Species::Cat, 1).await;
    let collar = store.ensure_tag(TagKind::Animal, Some(cat), "CATS234567").await.expect("tag").code;
    assert!(!send(&r, "GET", &format!("/t/{collar}"), None, "").await.body.contains("Collected eggs"));
    let eggs = send(&r, "POST", &format!("/t/{collar}/eggs"), None, "name=Sam&collected=1").await;
    assert_eq!(eggs.status, StatusCode::CONFLICT);
    let worry = send(&r, "POST", &format!("/t/{collar}/problem"), None, "name=Sam&problem=not+eating").await;
    assert_eq!(worry.status, StatusCode::SEE_OTHER, "{}", worry.body);

    // The compost pile logs drop-offs; a bed doesn't, but takes a concern.
    let pile = things::place(&store, "Compost pile", LocationKind::Compost).await;
    let pile_tag = store.ensure_tag(TagKind::Location, Some(pile), "HEAP234567").await.expect("tag").code;
    let dropped = send(&r, "POST", &format!("/t/{pile_tag}/compost"), None, "name=Neighbour&weight_lb=4.5").await;
    assert_eq!(dropped.status, StatusCode::SEE_OTHER, "{}", dropped.body);
    let bed = things::place(&store, "Bed 1", LocationKind::Bed).await;
    let bed_tag = store.ensure_tag(TagKind::Location, Some(bed), "BEDS234567").await.expect("tag").code;
    assert!(!send(&r, "GET", &format!("/t/{bed_tag}"), None, "").await.body.contains("Dropping off"));
    let wrong = send(&r, "POST", &format!("/t/{bed_tag}/compost"), None, "name=Neighbour").await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    let leak = send(&r, "POST", &format!("/t/{bed_tag}/problem"), None, "name=Sam&problem=drip+line+leaking").await;
    assert_eq!(leak.status, StatusCode::SEE_OTHER, "{}", leak.body);
    // A bin's tag can't be used to log eggs.
    let cross = send(&r, "POST", &format!("/t/{bin}/eggs"), None, "name=Sam&collected=1").await;
    assert_eq!(cross.status, StatusCode::UNPROCESSABLE_ENTITY);

    let tasks = send(&r, "GET", "/tasks", Some(CREW), "").await.body;
    assert!(tasks.contains("Check on Mochi") && tasks.contains("Look at Bed 1"), "{tasks}");
    let (count, pounds) = store
        .compost_totals(time::macros::date!(2000 - 01 - 01), time::macros::date!(2100 - 01 - 01))
        .await
        .expect("totals");
    assert_eq!((count, pounds), (1, "4.5".parse().expect("dec")));
    let who: String =
        sqlx::query_scalar("select actor_label from homestead.audit_entries where resource_type = 'compost_dropoffs'")
            .fetch_one(&pool)
            .await
            .expect("audit");
    assert_eq!(who, format!("Neighbour (tag {pile_tag})"));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_gate_tag_signs_people_in_and_out_and_leads_see_who_is_here(pool: PgPool) {
    common::steward(&pool).await;
    let r = app(AppState::new(Store::from_pool(pool.clone())));
    let made = send(&r, "POST", "/tags", Some(common::STEWARD), "kind=gate").await;
    assert_eq!(made.location.as_deref(), Some("/tags?kind=gate"));
    let code = first_code(&send(&r, "GET", "/tags?kind=gate", Some(common::STEWARD), "").await.body);

    let gate = send(&r, "GET", &format!("/t/{code}"), None, "").await.body;
    assert!(gate.contains("Sign in") && gate.contains("Kiki's Cat Club"), "{gate}");
    let arrived = send(&r, "POST", &format!("/t/{code}/in"), None, "name=Sam&group=cat_club").await;
    assert_eq!(arrived.status, StatusCode::SEE_OTHER, "{}", arrived.body);
    let community = send(&r, "GET", "/community", Some(common::STEWARD), "").await.body;
    assert!(community.contains("On site now") && community.contains("Sam"), "{community}");
    assert!(community.contains("none on file under this name"), "no waiver for Sam yet");
    // The public page never says whose waiver is on file.
    assert!(!send(&r, "GET", &format!("/t/{code}"), None, "").await.body.contains("on file"));

    let left = send(&r, "POST", &format!("/t/{code}/out"), None, "name=sam").await;
    assert_eq!(left.location.as_deref(), Some(format!("/t/{code}?done=left").as_str()), "{}", left.body);
    let community = send(&r, "GET", "/community", Some(common::STEWARD), "").await.body;
    assert!(community.contains("Nobody is signed in"), "{community}");
    let twice = send(&r, "POST", &format!("/t/{code}/out"), None, "name=Sam").await;
    assert_eq!(twice.status, StatusCode::CONFLICT);
    let shifts: i64 = sqlx::query_scalar("select count(*) from homestead.volunteer_shifts where person_name = 'Sam'")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(shifts, 1);
}

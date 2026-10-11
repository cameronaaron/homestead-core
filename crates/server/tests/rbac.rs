//! Authorization: nobody reaches a capability they don't hold.

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

const NOBODY: &str = "0000000000000000000000000000000000000000000000000000000000000001";
const NOBODY_API: &str = "0000000000000000000000000000000000000000000000000000000000000002";
const VOLUNTEER: &str = "0000000000000000000000000000000000000000000000000000000000000003";
const BOARD: &str = "0000000000000000000000000000000000000000000000000000000000000004";
const FARM_LEAD: &str = "0000000000000000000000000000000000000000000000000000000000000005";
const CREW: &str = "0000000000000000000000000000000000000000000000000000000000000006";

async fn send(r: &Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let status = resp.status();
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

fn as_person(method: &str, uri: &str, cookie: &str, form: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, format!("sbh_session={cookie}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form.to_owned()))
        .expect("req")
}

fn as_service(method: &str, uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("idempotency-key", uuid::Uuid::new_v4().to_string())
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .expect("req")
}

/// Every `(method, path, is_api)` the server registers, read from its source
/// so a new route is covered the moment it's added.
fn routes() -> Vec<(String, String, bool)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("src") {
        let path = entry.expect("entry").path();
        let src = std::fs::read_to_string(&path).expect("read");
        let api_file = path.file_name().is_some_and(|n| n == "api.rs");
        for (i, _) in src.match_indices(".route(\"") {
            // API routes live in api.rs or in a module's `fn api_routes`.
            let enclosing = src[..i].rfind("\npub fn ").map(|f| &src[f + 8..]).unwrap_or_default();
            let is_api = api_file || enclosing.starts_with("api_routes");
            let rest = &src[i + ".route(\"".len()..];
            let route = &rest[..rest.find('"').expect("closing quote")];
            let end = [rest.find(")\n"), rest.find(".route(")].into_iter().flatten().min().unwrap_or(rest.len());
            let call = &rest[..end];
            for method in ["get", "post"] {
                if call.contains(&format!(" {method}(")) || call.contains(&format!(".{method}(")) {
                    let prefix = if is_api { "/api/v1" } else { "" };
                    out.push((method.to_uppercase(), format!("{prefix}{route}"), is_api));
                }
            }
        }
    }
    assert!(out.len() > 50, "route scan found only {} routes; is the parser broken?", out.len());
    out
}

/// Reachable without a capability: signing in, pages about yourself, and
/// QR tags, where the printed code is the credential (a made-up or retired
/// code does nothing: `tags::a_retired_or_made_up_code_does_nothing`).
const OPEN: &[&str] = &[
    "/adopt",
    // The Instagram join link: name and email in (bounded, honeypot), the
    // welcome agreement out, and Discord OAuth only once it's signed. The
    // callback needs the person's secret token and admits one account.
    "/join",
    "/join/discord",
    "/join/{token}",
    // Their page's "Try again": needs the token, and only resends a failed
    // or cut-off send of that one join.
    "/join/{token}/retry",
    "/t/{code}",
    "/t/{code}/checkout",
    "/t/{code}/return",
    "/t/{code}/problem",
    "/t/{code}/use",
    "/t/{code}/eggs",
    "/t/{code}/compost",
    "/t/{code}/in",
    "/t/{code}/out",
    "/",
    "/me",
    "/login",
    "/logout",
    "/setup/{token}",
    "/healthz",
    "/discord/interactions",
    "/hooks/documenso",
    "/hooks/zeffy",
    // The installable shell: public by design (see `pwa`).
    "/manifest.webmanifest",
    "/sw.js",
    "/icon.svg",
    "/offline",
    // Read before anyone has an account (see `legal`).
    "/terms",
    "/privacy",
    // Your own first-run page and notification settings.
    "/welcome",
    "/me/notifications",
];

fn concrete(path: &str) -> String {
    path.replace("{id}", "8f14e45f-ceea-467a-9575-7d8f7c1c2a11")
        .replace("{event_id}", "1")
        .replace("{sink}", "discord")
        .replace("{key}", "thank-you")
        .replace("{owner_id}", "8f14e45f-ceea-467a-9575-7d8f7c1c2a11")
        .replace("{owner}", "animal")
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_caller_without_capabilities_is_refused_everywhere(pool: PgPool) {
    common::person(&pool, "Nemo", &[Role::Volunteer], NOBODY).await;
    common::service(&pool, "empty", NOBODY_API).await;
    // Strip every role: signed in, but holding no capability at all.
    sqlx::query("delete from homestead.actor_roles").execute(&pool).await.expect("strip roles");
    let r = app(AppState::new(Store::from_pool(pool)));

    let mut leaks = Vec::new();
    for (method, path, is_api) in routes() {
        if OPEN.contains(&path.as_str()) {
            continue;
        }
        let uri = concrete(&path);
        let req = if is_api { as_service(&method, &uri, NOBODY_API) } else { as_person(&method, &uri, NOBODY, "") };
        let (status, _) = send(&r, req).await;
        if status != StatusCode::FORBIDDEN {
            leaks.push(format!("{method} {path} → {status}"));
        }
    }
    assert!(leaks.is_empty(), "reachable without the capability:\n{}", leaks.join("\n"));
}

/// Only sign-in, webhooks and health checks may use the unattributed
/// system store; everything else writes as the person asking.
#[test]
fn only_unauthenticated_paths_use_the_system_store() {
    let allowed = ["lib.rs", "auth.rs", "chatops.rs", "zeffy.rs", "idempotency.rs", "main.rs"];
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    for entry in std::fs::read_dir(dir).expect("src") {
        let path = entry.expect("entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        let src = std::fs::read_to_string(&path).expect("read");
        let uses = src.matches("system_store").count();
        match name.as_str() {
            n if allowed.contains(&n) => {}
            // The Documenso webhook settles a release as `system`.
            "releases.rs" => assert_eq!(uses, 1, "releases.rs: only the webhook may use system_store"),
            _ => assert_eq!(uses, 0, "{name} uses system_store; take a Can<…> and use its store"),
        }
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn roles_open_exactly_their_own_pages(pool: PgPool) {
    common::steward(&pool).await;
    common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    common::person(&pool, "Bo", &[Role::Board], BOARD).await;
    common::person(&pool, "Fern", &[Role::FarmLead], FARM_LEAD).await;
    let r = app(AppState::new(Store::from_pool(pool.clone())));
    let get = |who: &str, uri: &str| as_person("GET", uri, who, "");

    // A volunteer sees and ticks off tasks, but not money, people or safety.
    let (status, _) = send(&r, as_person("POST", "/tasks", common::STEWARD, "title=Feed+hens&priority=normal")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, page) = send(&r, get(VOLUNTEER, "/tasks")).await;
    assert!(page.contains("Feed hens") && page.contains("/complete"), "volunteers can complete tasks");
    for uri in ["/gifts", "/contacts", "/contacts/export.csv", "/grants", "/people", "/audit", "/ops"] {
        assert_eq!(send(&r, get(VOLUNTEER, uri)).await.0, StatusCode::FORBIDDEN, "volunteer {uri}");
    }
    let (_, today) = send(&r, get(VOLUNTEER, "/")).await;
    assert!(!today.contains("href=\"/gifts\"") && !today.contains("failed integration"), "nav hides the rest");
    // A group's heading shows only when one of its links does.
    assert!(today.contains("aria-hidden=\"true\">Animals</span>"), "{today}");
    assert!(!today.contains(">People</span>"), "a volunteer opens nothing under People");

    // The board reads the books but doesn't write them, or run the farm.
    assert_eq!(send(&r, get(BOARD, "/gifts")).await.0, StatusCode::OK);
    assert_eq!(send(&r, get(BOARD, "/audit")).await.0, StatusCode::OK);
    let gift = "donor_name=A&kind=cash&received_on=2026-09-01&amount=5&fund=General";
    assert_eq!(send(&r, as_person("POST", "/gifts", BOARD, gift)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(send(&r, as_person("POST", "/tasks", BOARD, "title=x&priority=low")).await.0, StatusCode::FORBIDDEN);
    let (_, tasks) = send(&r, get(BOARD, "/tasks")).await;
    assert!(!tasks.contains("/complete"), "no Done button without tasks.write");

    // The farm lead runs operations but can't manage people or see donors.
    assert_eq!(send(&r, get(FARM_LEAD, "/inventory")).await.0, StatusCode::OK);
    assert_eq!(send(&r, get(FARM_LEAD, "/people")).await.0, StatusCode::FORBIDDEN);
    assert_eq!(send(&r, get(FARM_LEAD, "/gifts")).await.0, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn an_animals_vet_record_needs_animals_medical(pool: PgPool) {
    common::steward(&pool).await;
    common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    let r = app(AppState::new(Store::from_pool(pool)));
    let cat = "name=Mochi&species=cat&head_count=1&sex=female&status=in_care&origin=trapped&arrived_on=2026-09-01";
    send(&r, as_person("POST", "/animals", common::STEWARD, cat)).await;
    let (_, list) = send(&r, as_person("GET", "/animals", common::STEWARD, "")).await;
    let start = list.find("href=\"/animals/").expect("an animal's link") + "href=\"/animals/".len();
    let id = &list[start..start + 36];
    let care = "occurred_on=2026-09-02&kind=vaccine&details=FVRCP+%231&provider=Clinic&cost=40";
    send(&r, as_person("POST", &format!("/animals/{id}/care"), common::STEWARD, care)).await;

    let (_, page) = send(&r, as_person("GET", &format!("/animals/{id}"), common::STEWARD, "")).await;
    assert!(page.contains("FVRCP #1"));
    let (status, page) = send(&r, as_person("GET", &format!("/animals/{id}"), VOLUNTEER, "")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Mochi") && !page.contains("FVRCP") && !page.contains("Log care"), "{page}");
    let (status, _) = send(&r, as_person("POST", &format!("/animals/{id}/care"), VOLUNTEER, care)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn every_ui_and_api_write_names_who_made_it(pool: PgPool) {
    let kiki = common::steward(&pool).await;
    let bot = common::service(&pool, "Irrigation controller", NOBODY_API).await;
    let r = app(AppState::new(Store::from_pool(pool.clone())));
    send(&r, as_person("POST", "/inventory", common::STEWARD, "name=Layer+feed&unit=bag&reorder_level=2")).await;
    let req = Request::post("/api/v1/tasks")
        .header(header::AUTHORIZATION, format!("Bearer {NOBODY_API}"))
        .header("idempotency-key", "k1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"title":"Check drip line"}"#))
        .expect("req");
    assert_eq!(send(&r, req).await.0, StatusCode::CREATED);

    let rows: Vec<(String, Option<uuid::Uuid>)> = sqlx::query_as(
        "select resource_type, actor_id from homestead.audit_entries
         where action = 'insert' and resource_type in ('inventory_items', 'tasks') order by id",
    )
    .fetch_all(&pool)
    .await
    .expect("audit");
    assert_eq!(rows, vec![("inventory_items".into(), Some(kiki.id)), ("tasks".into(), Some(bot.id))]);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn revoked_tokens_and_disabled_people_are_locked_out(pool: PgPool) {
    let kiki = common::steward(&pool).await;
    let sam = common::person(&pool, "Sam", &[Role::Crew], VOLUNTEER).await;
    common::service(&pool, "cron", NOBODY_API).await;
    let r = app(AppState::new(Store::from_pool(pool.clone())));
    assert_eq!(send(&r, as_service("GET", "/api/v1/tasks", NOBODY_API)).await.0, StatusCode::OK);
    assert_eq!(send(&r, as_person("GET", "/tasks", VOLUNTEER, "")).await.0, StatusCode::OK);

    let steward =
        Store::from_pool(pool.clone()).acting_as(homestead_store::Actor { id: Some(kiki.id), label: "Kiki".into() });
    for t in steward.list_api_tokens().await.expect("tokens") {
        steward.revoke_api_token(t.id).await.expect("revoke");
    }
    let (status, _) = send(&r, as_person("POST", &format!("/people/{}/disable", sam.id), common::STEWARD, "")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(send(&r, as_service("GET", "/api/v1/tasks", NOBODY_API)).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(send(&r, as_person("GET", "/tasks", VOLUNTEER, "")).await.0, StatusCode::SEE_OTHER);
}

/// Every `<form method="post">` target on a page.
fn post_forms(page: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, _) in page.match_indices("<form") {
        let tag = &page[i..i + page[i..].find('>').unwrap_or(0)];
        if !tag.contains("method=\"post\"") {
            continue;
        }
        if let Some(a) = tag.find("action=\"") {
            let rest = &tag[a + 8..];
            out.push(rest[..rest.find('"').unwrap_or(0)].replace("&amp;", "&"));
        }
    }
    out
}

/// Every in-app link on a page (not the navigation, which is filtered
/// already, nor other sites).
fn links(page: &str) -> Vec<String> {
    let body = &page[page.find("<main").unwrap_or(0)..];
    let mut out: Vec<String> = body
        .match_indices("href=\"/")
        .map(|(i, _)| {
            let rest = &body[i + 6..];
            rest[..rest.find('"').unwrap_or(0)].replace("&amp;", "&")
        })
        .filter(|l| !l.starts_with("/t/") && l != "/adopt")
        .collect();
    out.sort();
    out.dedup();
    out
}

/// A page must not offer a form or link its viewer would be refused for:
/// they fill it in, or follow it, and get "your account can't do that".
/// Each role opens every page it may; every form there is submitted
/// (empty) and every link followed as that role, and a 403 means it
/// shouldn't have been shown.
#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn no_page_offers_a_form_or_link_its_viewer_cannot_use(pool: PgPool) {
    common::steward(&pool).await;
    common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    common::person(&pool, "Bo", &[Role::Board], BOARD).await;
    common::person(&pool, "Fern", &[Role::FarmLead], FARM_LEAD).await;
    common::person(&pool, "Cam", &[Role::Crew], CREW).await;
    let r = app(AppState::new(Store::from_pool(pool.clone())));
    // Something on the detail pages.
    let seed = [
        (
            "/animals",
            "name=Mochi&species=cat&head_count=1&sex=female&status=in_care&origin=trapped&arrived_on=2026-09-01",
        ),
        ("/equipment", "name=Loppers&category=hand+tools&condition=good"),
        ("/inventory", "name=Layer+feed&unit=bag&reorder_level=2"),
        ("/locations", "name=Bed+1&kind=bed"),
        ("/tasks", "title=Feed+hens&priority=normal"),
    ];
    let mut pages: Vec<String> = routes()
        .into_iter()
        .filter(|(m, p, api)| m == "GET" && !api && !p.contains('{') && !OPEN.contains(&p.as_str()))
        .map(|(_, p, _)| p)
        .filter(|p| !p.ends_with(".csv") && !p.ends_with(".json"))
        .collect();
    for (list, form) in seed {
        let (status, _) = send(&r, as_person("POST", list, common::STEWARD, form)).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "seed {list}");
    }
    // Their detail pages, found from the lists.
    for (list, prefix) in [
        ("/animals", "/animals/"),
        ("/equipment", "/equipment/"),
        ("/inventory", "/inventory/"),
        ("/garden", "/locations/"),
    ] {
        let (_, page) = send(&r, as_person("GET", list, common::STEWARD, "")).await;
        let at = page.find(&format!("href=\"{prefix}")).unwrap_or_else(|| panic!("a link on {list}")) + 6;
        pages.push(page[at..at + prefix.len() + 36].to_owned());
    }

    let mut offered = Vec::new();
    for (who, token) in [("volunteer", VOLUNTEER), ("board", BOARD), ("crew", CREW), ("farm lead", FARM_LEAD)] {
        for page in &pages {
            let (status, html) = send(&r, as_person("GET", page, token, "")).await;
            if status != StatusCode::OK {
                continue;
            }
            for action in post_forms(&html) {
                if action == "/logout" {
                    continue;
                }
                let (status, _) = send(&r, as_person("POST", &action, token, "")).await;
                if status == StatusCode::FORBIDDEN {
                    offered.push(format!("{who} on {page}: form to {action}"));
                }
            }
            for link in links(&html) {
                let (status, _) = send(&r, as_person("GET", &link, token, "")).await;
                if status == StatusCode::FORBIDDEN {
                    offered.push(format!("{who} on {page}: link to {link}"));
                }
            }
        }
    }
    offered.sort();
    offered.dedup();
    assert!(offered.is_empty(), "forms and links shown to people who'd be refused:\n{}", offered.join("\n"));
}

//! HTTP behaviour of the full router against a real database.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use ed25519_dalek::{Signer, SigningKey};
use hmac::{Hmac, KeyInit, Mac};
use homestead_integrations::discord::{InteractionVerifier, command_definitions};
use homestead_server::{AppState, app, chatops};
use homestead_store::Store;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::PgPool;
use tower::ServiceExt;

mod common;

/// The legacy API credential; the UI no longer accepts it.
const TOKEN: &str = "test-admin-token-0123456789abcdef0123";
const ZEFFY_SECRET: &str = "zeffy-test-secret";

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[42u8; 32])
}

async fn router(pool: PgPool) -> Router {
    common::steward(&pool).await;
    bare_router(pool)
}

/// The app with nobody signed in yet.
fn bare_router(pool: PgPool) -> Router {
    let verifier = InteractionVerifier::from_hex(&hex::encode(signing_key().verifying_key().as_bytes())).expect("key");
    app(AppState {
        admin_token: Some(TOKEN.into()),
        discord: Some(Arc::new(verifier)),
        zeffy_webhook_secret: Some(ZEFFY_SECRET.into()),
        ..AppState::new(Store::from_pool(pool))
    })
}

async fn send(r: &Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let (parts, body) = resp.into_parts();
    let bytes = body.collect().await.expect("body").to_bytes();
    (parts.status, parts.headers, String::from_utf8_lossy(&bytes).into_owned())
}

fn api(method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri).header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    if method != "GET" && method != "HEAD" {
        b = b.header("idempotency-key", uuid::Uuid::new_v4().to_string());
    }
    match body {
        Some(v) => b.header(header::CONTENT_TYPE, "application/json").body(Body::from(v.to_string())),
        None => b.body(Body::empty()),
    }
    .expect("request")
}

fn ui(method: &str, uri: &str, form: Option<&str>) -> Request<Body> {
    let b = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, format!("other=1; sbh_session={}", common::STEWARD));
    match form {
        Some(f) => b.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded").body(Body::from(f.to_owned())),
        None => b.body(Body::empty()),
    }
    .expect("request")
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn every_protected_route_rejects_anonymous_and_wrong_credentials(pool: PgPool) {
    let r = router(pool).await;
    for path in [
        "/",
        "/tasks",
        "/garden",
        "/inventory",
        "/grants",
        "/animals",
        "/gifts",
        "/community",
        "/stand",
        "/contacts",
        "/contacts/export.csv",
        "/ops",
    ] {
        let (status, headers, _) = send(&r, Request::get(path).body(Body::empty()).expect("req")).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{path}");
        assert_eq!(headers[header::LOCATION], "/login");
        let wrong = Request::get(path).header(header::COOKIE, "sbh_session=nope").body(Body::empty()).expect("req");
        assert_eq!(send(&r, wrong).await.0, StatusCode::SEE_OTHER, "{path} with wrong cookie");
    }
    // Mutating UI routes too, not just pages.
    let post = Request::post("/tasks").body(Body::from("title=x&priority=normal")).expect("req");
    assert_eq!(send(&r, post).await.0, StatusCode::SEE_OTHER);

    for path in ["/api/v1/tasks", "/api/v1/inventory", "/api/v1/events", "/api/v1/bus"] {
        assert_eq!(send(&r, Request::get(path).body(Body::empty()).expect("r")).await.0, StatusCode::UNAUTHORIZED);
        let bad = Request::get(path).header(header::AUTHORIZATION, "Bearer wrong").body(Body::empty()).expect("r");
        assert_eq!(send(&r, bad).await.0, StatusCode::UNAUTHORIZED);
        // The session cookie is a UI credential; the API wants the header.
        let cookie_only = Request::get(path).header(header::COOKIE, format!("sbh_session={TOKEN}")).body(Body::empty());
        assert_eq!(send(&r, cookie_only.expect("r")).await.0, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(send(&r, Request::get("/healthz").body(Body::empty()).expect("r")).await.0, StatusCode::OK);
}

fn form(uri: &str, body: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body.to_owned()))
        .expect("req")
}

fn session_from(headers: &axum::http::HeaderMap) -> String {
    let cookie = headers[header::SET_COOKIE].to_str().expect("ascii");
    for attr in ["HttpOnly", "SameSite=Strict", "Secure", "Path=/"] {
        assert!(cookie.contains(attr), "cookie missing {attr}: {cookie}");
    }
    cookie.split(';').next().expect("name=value").to_owned()
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_new_person_sets_a_password_from_their_link_then_signs_in(pool: PgPool) {
    let r = router(pool.clone()).await;
    // A steward adds Sam; the one-time link is shown once, never stored.
    let (status, headers, page) =
        send(&r, ui("POST", "/people", Some("kind=human&display_name=Sam+Reyes&email=Sam%40Example.org&role=crew")))
            .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let start = page.find("/setup/").expect("setup link shown");
    let link = page[start..start + "/setup/".len() + 64].to_owned();
    let stored: i64 = sqlx::query_scalar("select count(*) from homestead.setup_links where token_hash = $1")
        .bind(&link["/setup/".len()..])
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(stored, 0, "only the hash of the link is stored");

    let (status, headers, _) = send(&r, Request::get(link.as_str()).body(Body::empty()).expect("req")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
    let short = send(&r, form(&link, "password=short&confirm=short")).await;
    assert_eq!(short.0, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, headers, _) =
        send(&r, form(&link, "password=pumpkin+patch+sunrise&confirm=pumpkin+patch+sunrise")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = session_from(&headers);
    let get = |c: &str| Request::get("/tasks").header(header::COOKIE, c).body(Body::empty()).expect("req");
    assert_eq!(send(&r, get(&cookie)).await.0, StatusCode::OK);
    assert_eq!(
        send(&r, Request::get(link.as_str()).body(Body::empty()).expect("r")).await.0,
        StatusCode::NOT_FOUND,
        "used once"
    );

    // Sign out ends that session for real, not just in this browser.
    let out = Request::post("/logout").header(header::COOKIE, &cookie).body(Body::empty()).expect("req");
    send(&r, out).await;
    assert_eq!(send(&r, get(&cookie)).await.0, StatusCode::SEE_OTHER);

    // Wrong passwords and unknown emails look the same; the right one works.
    for body in ["email=sam%40example.org&password=wrong+wrong+wrong", "email=nobody%40example.org&password=x"] {
        let (status, headers, _) = send(&r, form("/login", body)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert!(headers.get(header::SET_COOKIE).is_none());
    }
    let (status, headers, _) = send(&r, form("/login", "email=SAM%40example.org&password=pumpkin+patch+sunrise")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = session_from(&headers);
    let (_, _, me) =
        send(&r, Request::get("/me").header(header::COOKIE, &cookie).body(Body::empty()).expect("r")).await;
    assert!(me.contains("Sam Reyes") && me.contains("tasks.write") && !me.contains("gifts.read"), "{me}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_shared_admin_token_no_longer_signs_anyone_in_to_the_ui(pool: PgPool) {
    let r = router(pool).await;
    let as_cookie = Request::get("/tasks").header(header::COOKIE, format!("sbh_session={TOKEN}")).body(Body::empty());
    assert_eq!(send(&r, as_cookie.expect("r")).await.0, StatusCode::SEE_OTHER);
    let old_form = form("/login", &format!("token={TOKEN}"));
    assert_ne!(send(&r, old_form).await.0, StatusCode::SEE_OTHER);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn api_creates_resources_and_maps_errors_to_statuses(pool: PgPool) {
    let r = router(pool.clone()).await;
    let (status, _, body) =
        send(&r, api("POST", "/api/v1/tasks", Some(json!({"title": "Prune figs", "due_on": "2026-11-01"})))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(task["priority"], "normal");
    let task_id = task["id"].as_str().expect("task id");
    let kiki: uuid::Uuid = sqlx::query_scalar("select id from homestead.actors where display_name = 'Kiki'")
        .fetch_one(&pool)
        .await
        .expect("steward");
    let assign = format!("/api/v1/tasks/{task_id}/assign");
    let (status, _, body) = send(&r, api("POST", &assign, Some(json!({"assigned_to": kiki})))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(serde_json::from_str::<Value>(&body).expect("json")["assigned_to"], kiki.to_string());
    let (status, _, _) = send(&r, api("POST", &assign, Some(json!({})))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "missing assigned_to must not silently unassign");
    let (status, _, _) = send(&r, api("POST", &assign, Some(json!({"assignee": null})))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "a misspelling must not silently unassign");
    let (status, _, body) = send(&r, api("POST", &assign, Some(json!({"assigned_to": null})))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(serde_json::from_str::<Value>(&body).expect("json")["assigned_to"].is_null());

    let (status, _, body) = send(&r, api("POST", "/api/v1/tasks", Some(json!({"title": "   "})))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("must not be blank"));

    let (status, _, _) = send(&r, api("POST", &format!("/api/v1/tasks/{}/complete", uuid::Uuid::nil()), None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (_, _, item) = send(&r, api("POST", "/api/v1/inventory", Some(json!({"name": "Straw", "unit": "bale"})))).await;
    let id = serde_json::from_str::<Value>(&item).expect("json")["id"].as_str().expect("id").to_owned();
    let (status, _, _) = send(
        &r,
        api("POST", &format!("/api/v1/inventory/{id}/adjust"), Some(json!({"delta": "-1", "reason": "bedding"}))),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "overdraw");

    let (status, _, body) =
        send(&r, api("POST", "/api/v1/locations", Some(json!({"name": "Bed 1", "kind": "bed"})))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, _, _) =
        send(&r, api("POST", "/api/v1/locations", Some(json!({"name": "Bed 1", "kind": "bed"})))).await;
    assert_eq!(status, StatusCode::CONFLICT, "duplicate name");

    let (_, _, events) = send(&r, api("GET", "/api/v1/events", None)).await;
    let kinds: Vec<String> = serde_json::from_str::<Vec<Value>>(&events)
        .expect("json")
        .iter()
        .map(|e| e["kind"].as_str().expect("k").to_owned())
        .collect();
    assert_eq!(kinds, ["task.created"], "only committed domain writes emit events");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn api_writes_require_and_replay_idempotency_keys(pool: PgPool) {
    let r = router(pool.clone()).await;
    let body = json!({"title": "Feed hens"}).to_string();
    let request = || {
        Request::post("/api/v1/tasks")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", "feed-hens-2026-09-28")
            .body(Body::from(body.clone()))
            .expect("request")
    };
    let (first_status, _, first_body) = send(&r, request()).await;
    assert_eq!(first_status, StatusCode::CREATED);
    let (second_status, second_headers, second_body) = send(&r, request()).await;
    assert_eq!(second_status, StatusCode::CREATED);
    assert_eq!(second_headers["idempotency-replayed"], "true");
    assert_eq!(second_body, first_body);
    let tasks: i64 = sqlx::query_scalar("select count(*) from homestead.tasks").fetch_one(&pool).await.expect("count");
    assert_eq!(tasks, 1);

    let missing = Request::post("/api/v1/tasks")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("request");
    assert_eq!(send(&r, missing).await.0, StatusCode::BAD_REQUEST);

    let changed = Request::post("/api/v1/tasks")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("idempotency-key", "feed-hens-2026-09-28")
        .body(Body::from(json!({"title": "Different"}).to_string()))
        .expect("request");
    assert_eq!(send(&r, changed).await.0, StatusCode::CONFLICT);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn zeffy_webhooks_are_verified_and_deduplicated(pool: PgPool) {
    let r = router(pool.clone()).await;
    let now = time::OffsetDateTime::now_utc();
    let body = json!({
        "id": uuid::Uuid::new_v4(),
        "type": "payment.completed",
        "version": 1,
        "dispatchedAt": now,
        "data": {"id": uuid::Uuid::new_v4(), "status": "succeeded", "amount": 2500}
    })
    .to_string();
    let timestamp = now.unix_timestamp();
    let mut mac = Hmac::<Sha256>::new_from_slice(ZEFFY_SECRET.as_bytes()).expect("secret");
    mac.update(format!("{timestamp}.{body}").as_bytes());
    let signature = format!("t={timestamp},v1={}", hex::encode(mac.finalize().into_bytes()));
    let request = || {
        Request::post("/hooks/zeffy")
            .header(header::CONTENT_TYPE, "application/json")
            .header("zeffy-signature", &signature)
            .body(Body::from(body.clone()))
            .expect("request")
    };
    assert_eq!(send(&r, request()).await.2, "recorded");
    assert_eq!(send(&r, request()).await.2, "duplicate");
    let count: i64 = sqlx::query_scalar("select count(*) from homestead.zeffy_webhook_events")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(count, 1);

    let forged =
        Request::post("/hooks/zeffy").header("zeffy-signature", signature).body(Body::from("{}")).expect("request");
    assert_eq!(send(&r, forged).await.0, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn ui_forms_handle_blank_optionals_and_escape_user_text(pool: PgPool) {
    let r = router(pool).await;
    let form = "title=%3Cscript%3Ealert(1)%3C%2Fscript%3E&details=&priority=high&due_on=&location_id=";
    let (status, headers, body) = send(&r, ui("POST", "/tasks", Some(form))).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    assert_eq!(headers[header::LOCATION], "/tasks");

    let (_, _, page) = send(&r, ui("GET", "/tasks", None)).await;
    assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!page.contains("<script>alert"), "user text rendered unescaped");

    let (status, _, page) = send(&r, ui("POST", "/tasks", Some("title=x&priority=whenever"))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(page.contains("unknown value"));
    let (status, _, _) = send(&r, ui("POST", "/tasks", Some("title=x&priority=low&due_on=next+tuesday"))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn ui_garden_and_inventory_flow(pool: PgPool) {
    let r = router(pool.clone()).await;
    assert_eq!(send(&r, ui("POST", "/locations", Some("name=Bed+2&kind=bed&notes="))).await.0, StatusCode::SEE_OTHER);
    let loc: uuid::Uuid = sqlx::query_scalar("select id from homestead.locations").fetch_one(&pool).await.expect("loc");
    let plant = format!("location_id={loc}&crop=Garlic&variety=&sown_on=2026-10-15&expected_harvest_on=");
    assert_eq!(send(&r, ui("POST", "/plantings", Some(&plant))).await.0, StatusCode::SEE_OTHER);
    let pid: uuid::Uuid = sqlx::query_scalar("select id from homestead.plantings").fetch_one(&pool).await.expect("p");
    let harvest = format!("planting_id={pid}&harvested_on=2026-06-01&quantity=2.5&unit=lb&notes=");
    assert_eq!(send(&r, ui("POST", "/harvests", Some(&harvest))).await.0, StatusCode::SEE_OTHER);
    let (_, _, page) = send(&r, ui("GET", "/garden", None)).await;
    assert!(page.contains("Garlic") && page.contains("harvesting"));

    assert_eq!(
        send(&r, ui("POST", "/inventory", Some("name=Feed&unit=bag&reorder_level=2"))).await.0,
        StatusCode::SEE_OTHER
    );
    let item: uuid::Uuid =
        sqlx::query_scalar("select id from homestead.inventory_items").fetch_one(&pool).await.expect("i");
    let adjust = |d: &str| format!("delta={d}&reason=test");
    assert_eq!(
        send(&r, ui("POST", &format!("/inventory/{item}/adjust"), Some(&adjust("%2B5")))).await.0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        send(&r, ui("POST", &format!("/inventory/{item}/adjust"), Some(&adjust("-9")))).await.0,
        StatusCode::CONFLICT
    );
    let on_hand: rust_decimal::Decimal =
        sqlx::query_scalar("select on_hand from homestead.inventory_items").fetch_one(&pool).await.expect("q");
    assert_eq!(on_hand, rust_decimal::Decimal::from(5), "'+5' is accepted as a positive delta");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn grant_ui_generates_preview_then_requires_approval(pool: PgPool) {
    let r = router(pool.clone()).await;
    let template = "name=Food+fund&subject=Hello+%7B%7Borganization%7D%7D&body=Dear+%7B%7Bcontact_name%7D%7D%2C%0A%0AOur+request+is+%7B%7Bamount%7D%7D.";
    assert_eq!(send(&r, ui("POST", "/grants/templates", Some(template))).await.0, StatusCode::SEE_OTHER);
    let recipient = "organization=Valley+Fund&contact_name=Avery&email=avery%40example.org&program_name=Resilience&source=foundation+website&custom_fields=%7B%22amount%22%3A%22%2425%2C000%22%7D";
    assert_eq!(send(&r, ui("POST", "/grants/recipients", Some(recipient))).await.0, StatusCode::SEE_OTHER);
    let template_id: uuid::Uuid =
        sqlx::query_scalar("select id from homestead.grant_templates").fetch_one(&pool).await.expect("template");
    let campaign = format!("name=Spring+outreach&template_id={template_id}");
    let (status, headers, _) = send(&r, ui("POST", "/grants/campaigns", Some(&campaign))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let location = headers[header::LOCATION].to_str().expect("location").to_owned();
    let (_, _, preview) = send(&r, ui("GET", &location, None)).await;
    assert!(preview.contains("Dear Avery"));
    assert!(preview.contains("$25,000"));
    assert!(preview.contains("Approve exact copies"));

    let id = location.rsplit('/').next().expect("campaign id");
    assert_eq!(
        send(&r, ui("POST", &format!("/grants/campaigns/{id}/send"), None)).await.0,
        StatusCode::FORBIDDEN,
        "delivery is locked without provider configuration"
    );
    assert_eq!(send(&r, ui("POST", &format!("/grants/campaigns/{id}/approve"), None)).await.0, StatusCode::SEE_OTHER);
    let status: String =
        sqlx::query_scalar("select status from homestead.grant_campaigns").fetch_one(&pool).await.expect("status");
    assert_eq!(status, "approved");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn grant_library_templates_can_be_saved_and_edited(pool: PgPool) {
    let r = router(pool.clone()).await;
    let (status, headers, _) = send(&r, ui("POST", "/grants/templates/library/community-partnership", None)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let edit_url = headers[header::LOCATION].to_str().expect("location").to_owned();
    let (_, _, edit_page) = send(&r, ui("GET", &edit_url, None)).await;
    assert!(edit_page.contains("Community partnership"));
    assert!(edit_page.contains("Meadow green"));

    let update = "name=Our+partnership+letter&subject=Hello+%7B%7Borganization%7D%7D&body=Dear+%7B%7Bcontact_name%7D%7D%2C%0A%0AA+saved+letter.&design=orchard";
    assert_eq!(send(&r, ui("POST", &edit_url, Some(update))).await.0, StatusCode::SEE_OTHER);
    let saved: (String, String, String) =
        sqlx::query_as("select name, body_template, design from homestead.grant_templates")
            .fetch_one(&pool)
            .await
            .expect("saved template");
    assert_eq!(
        saved,
        ("Our partnership letter".into(), "Dear {{contact_name}},\n\nA saved letter.".into(), "orchard".into())
    );
}

// ── Discord interactions ─────────────────────────────────────────────────────

fn signed(body: &Value) -> Request<Body> {
    let ts = time::OffsetDateTime::now_utc().unix_timestamp().to_string();
    let raw = body.to_string();
    let sig = hex::encode(signing_key().sign(format!("{ts}{raw}").as_bytes()).to_bytes());
    Request::post("/discord/interactions")
        .header("x-signature-ed25519", sig)
        .header("x-signature-timestamp", ts)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(raw))
        .expect("req")
}

fn command(name: &str, options: Value) -> Value {
    json!({ "type": 2, "data": { "name": name, "options": options },
            "member": { "user": { "id": "4242", "username": "sunny" } } })
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn interactions_require_a_valid_signature(pool: PgPool) {
    let r = router(pool).await;
    let unsigned = Request::post("/discord/interactions").body(Body::from(r#"{"type":1}"#)).expect("req");
    assert_eq!(send(&r, unsigned).await.0, StatusCode::UNAUTHORIZED);

    let mut forged = signed(&json!({"type": 1}));
    *forged.body_mut() = Body::from(r#"{"type":2,"data":{"name":"task","options":[]}}"#);
    assert_eq!(send(&r, forged).await.0, StatusCode::UNAUTHORIZED, "body swapped after signing");

    let (status, _, body) = send(&r, signed(&json!({"type": 1}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(serde_json::from_str::<Value>(&body).expect("json"), json!({"type": 1}));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn slash_commands_add_and_list_tasks(pool: PgPool) {
    let r = router(pool).await;
    let add =
        command("task", json!([{"name": "title", "value": "Collect eggs"}, {"name": "priority", "value": "high"}]));
    let (_, _, body) = send(&r, signed(&add)).await;
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["type"], 4);
    assert_eq!(v["data"]["content"], "📝 Added high task: Collect eggs");
    assert_eq!(v["data"]["allowed_mentions"]["parse"], json!([]));

    let (_, _, body) = send(&r, signed(&command("tasks", json!([])))).await;
    let v: Value = serde_json::from_str(&body).expect("json");
    assert!(v["data"]["content"].as_str().expect("s").contains("[high] Collect eggs"));

    let (_, _, body) = send(&r, signed(&command("task", json!([{"name": "title", "value": " "}])))).await;
    let v: Value = serde_json::from_str(&body).expect("json");
    assert!(v["data"]["content"].as_str().expect("s").contains("must not be blank"));
    assert_eq!(v["data"]["flags"], 64, "errors are only shown to the invoker");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn slash_commands_act_as_the_linked_person_or_a_named_volunteer(pool: PgPool) {
    let r = router(pool.clone()).await;
    let who = |pool: PgPool| async move {
        sqlx::query_as::<_, (String, Option<uuid::Uuid>)>(
            "select actor_label, actor_id from homestead.audit_entries where resource_type = 'tasks' order by id desc limit 1",
        )
        .fetch_one(&pool)
        .await
        .expect("audit")
    };
    send(&r, signed(&command("task", json!([{"name": "title", "value": "Fix gate"}])))).await;
    assert_eq!(who(pool.clone()).await, ("discord:sunny".to_owned(), None));

    // Once linked, the same Discord user acts as their account, with its
    // capabilities: a board member can read tasks but not add them.
    let board = common::person(&pool, "Bo", &[homestead_domain::auth::Role::Board], &"b0".repeat(32)).await;
    Store::from_pool(pool.clone()).set_external_subject(board.id, Some("discord:4242")).await.expect("link");
    let (_, _, body) = send(&r, signed(&command("task", json!([{"name": "title", "value": "Paint barn"}])))).await;
    assert!(body.contains("tasks.write"), "{body}");
    let (_, _, body) = send(&r, signed(&command("tasks", json!([])))).await;
    assert!(body.contains("Fix gate"), "{body}");

    let anonymous = json!({ "type": 2, "data": { "name": "tasks", "options": [] } });
    let (_, _, body) = send(&r, signed(&anonymous)).await;
    assert!(body.contains("couldn't tell who asked"), "{body}");
}

#[test]
fn every_registered_command_is_handled() {
    let registered: Vec<String> = command_definitions()
        .as_array()
        .expect("array")
        .iter()
        .map(|c| c["name"].as_str().expect("name").to_owned())
        .collect();
    let mut a = registered.clone();
    let mut b: Vec<String> = chatops::HANDLED.iter().map(|s| (*s).to_owned()).collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "registered slash commands and handler diverge");
}

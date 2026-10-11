//! Legal releases end to end: UI → Documenso (mocked) → webhook → outbox.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_integrations::documenso::{Client, Spec, external_id, load_layouts};
use homestead_integrations::http_client;
use homestead_server::releases::Releases;
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use serde_json::json;
use sqlx::PgPool;
use tower::ServiceExt;
use wiremock::matchers::{header as h, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Like real Documenso, every created envelope gets its own id.
#[derive(Default)]
struct UniqueEnvelope(std::sync::atomic::AtomicUsize);

impl wiremock::Respond for UniqueEnvelope {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        let n = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        ResponseTemplate::new(200).set_body_json(json!({ "id": format!("envelope_{n}") }))
    }
}

mod common;
use common::STEWARD as TOKEN;
const SECRET: &str = "whsec-test";

async fn router(pool: PgPool, documenso: &MockServer, approved: bool) -> Router {
    app(state(pool, documenso, approved).await)
}

async fn state(pool: PgPool, documenso: &MockServer, approved: bool) -> AppState {
    common::steward(&pool).await;
    let mut spec = Spec::parse(include_str!("../../../documenso/releases.toml")).expect("spec");
    spec.approved = approved;
    let pdfs = spec.releases.iter().map(|r| (r.key.clone(), b"%PDF-1.7 test".to_vec())).collect();
    let layouts = load_layouts(&spec, std::path::Path::new("../..")).expect("layouts");
    AppState {
        releases: Some(Arc::new(Releases {
            client: Client::new(http_client(), &documenso.uri(), "api_test".into()),
            spec,
            pdfs,
            layouts,
            webhook_secret: SECRET.into(),
        })),
        ..AppState::new(Store::from_pool(pool))
    }
}

async fn send(r: &Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let status = resp.status();
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

fn form(body: &str) -> Request<Body> {
    Request::post("/releases")
        .header(header::COOKIE, format!("sbh_session={TOKEN}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body.to_owned()))
        .expect("req")
}

fn hook(secret: &str, event: &str, ext: serde_json::Value) -> Request<Body> {
    Request::post("/hooks/documenso")
        .header("x-documenso-secret", secret)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "event": event, "payload": { "id": 1, "externalId": ext } }).to_string()))
        .expect("req")
}

async fn events(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("select kind from homestead.events order by id").fetch_all(pool).await.expect("events")
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn drafts_cannot_be_sent_until_approved(pool: PgPool) {
    let documenso = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).expect(0).mount(&documenso).await;
    let r = router(pool.clone(), &documenso, false).await;
    let (status, body) = send(&r, form("name=Jane+Doe&email=jane%40example.org&r_liability-waiver=on")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("haven't been approved for use"));
    let page = send(
        &r,
        Request::get("/releases")
            .header(header::COOKIE, format!("sbh_session={TOKEN}"))
            .body(Body::empty())
            .expect("r"),
    )
    .await;
    assert!(page.1.contains("drafts, so sending is turned off"));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn approved_releases_are_sent_then_settled_once_by_webhook(pool: PgPool) {
    let documenso = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/envelope/create"))
        .and(h("Authorization", "api_test"))
        .respond_with(UniqueEnvelope::default())
        .expect(2)
        .mount(&documenso)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/envelope/distribute"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "success": true, "recipients": [
            { "email": "jane@example.org", "signingUrl": "https://sign.example/sign/tok_jane" }
        ]})))
        .expect(2)
        .mount(&documenso)
        .await;
    let r = router(pool.clone(), &documenso, true).await;

    let (status, _) =
        send(&r, form("name=Jane+Doe&email=Jane%40Example.org&r_liability-waiver=on&r_photo-release=on")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let rows: Vec<(uuid::Uuid, String, String)> =
        sqlx::query_as("select id, release_key, signer_email from homestead.release_requests order by release_key")
            .fetch_all(&pool)
            .await
            .expect("rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].2, "jane@example.org");

    // Documenso isn't emailing, so the sender gets the links to pass on.
    let page = send(
        &r,
        Request::get("/releases")
            .header(header::COOKIE, format!("sbh_session={TOKEN}"))
            .body(Body::empty())
            .expect("r"),
    )
    .await
    .1;
    assert!(page.contains("doesn't email signers yet"), "{page}");
    assert_eq!(page.matches(r#"value="https://sign.example/sign/tok_jane""#).count(), 2);

    let waiver = rows.iter().find(|x| x.1 == "liability-waiver").expect("waiver").0;
    assert_eq!(
        send(&r, hook("wrong", "DOCUMENT_COMPLETED", json!(external_id(waiver)))).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(send(&r, hook(SECRET, "DOCUMENT_COMPLETED", json!(external_id(waiver)))).await.0, StatusCode::OK);
    assert_eq!(
        send(&r, hook(SECRET, "DOCUMENT_COMPLETED", json!(external_id(waiver)))).await.0,
        StatusCode::OK,
        "retry is fine"
    );
    assert_eq!(send(&r, hook(SECRET, "DOCUMENT_COMPLETED", json!("someone-elses-doc"))).await.0, StatusCode::OK);
    assert_eq!(events(&pool).await, ["release.signed"], "announced exactly once");

    let status: String = sqlx::query_scalar("select status from homestead.release_requests where id = $1")
        .bind(waiver)
        .fetch_one(&pool)
        .await
        .expect("status");
    assert_eq!(status, "signed");
    let page = send(
        &r,
        Request::get("/releases")
            .header(header::COOKIE, format!("sbh_session={TOKEN}"))
            .body(Body::empty())
            .expect("r"),
    )
    .await
    .1;
    assert_eq!(
        page.matches(r#"value="https://sign.example/sign/tok_jane""#).count(),
        1,
        "the signed waiver's link is gone, the photo release's stays"
    );

    // The board can see who signed what, but a link would let them sign.
    const BOARD: &str = "b0a4db0a4db0a4db0a4db0a4db0a4db0a4db0a4db0a4db0a4db0a4db0a4db0a4";
    common::person(&pool, "Bo Board", &[homestead_domain::auth::Role::Board], BOARD).await;
    let (status, page) = send(
        &r,
        Request::get("/releases")
            .header(header::COOKIE, format!("sbh_session={BOARD}"))
            .body(Body::empty())
            .expect("r"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Jane Doe") && !page.contains("tok_jane"), "{page}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_documenso_failure_is_reported_not_recorded(pool: PgPool) {
    let documenso = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad token"))
        .mount(&documenso)
        .await;
    let r = router(pool.clone(), &documenso, true).await;
    let (status, body) = send(&r, form("name=Omar&email=omar%40example.org&r_nda=on")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body.contains("didn't accept"));
    let n: i64 =
        sqlx::query_scalar("select count(*) from homestead.release_requests").fetch_one(&pool).await.expect("n");
    assert_eq!(n, 0);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn sender_details_travel_locked_and_a_missing_one_sends_nothing(pool: PgPool) {
    use wiremock::matchers::body_string_contains;
    let documenso = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/envelope/create"))
        // The multipart payload carries the locked, prefilled detail.
        .and(body_string_contains(r#""text":"Mochi, grey tabby""#))
        .and(body_string_contains(r#""readOnly":true"#))
        .respond_with(UniqueEnvelope::default())
        .expect(1)
        .mount(&documenso)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/envelope/distribute"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "success": true })))
        .expect(1)
        .mount(&documenso)
        .await;
    let r = router(pool.clone(), &documenso, true).await;

    // The waiver is fine, but the adoption lacks its fee: neither goes out.
    let (status, body) = send(
        &r,
        form("name=Ana&email=ana%40example.org&r_liability-waiver=on&r_cat-adoption=on&d_cat-adoption_cat=Mochi&d_cat-adoption_microchip=985"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("Adoption fee (USD)") && body.contains("Nothing was sent"), "{body}");
    let n: i64 =
        sqlx::query_scalar("select count(*) from homestead.release_requests").fetch_one(&pool).await.expect("n");
    assert_eq!(n, 0);

    let (status, _) = send(
        &r,
        form("name=Ana&email=ana%40example.org&r_cat-adoption=on&d_cat-adoption_cat=Mochi%2C+grey+tabby&d_cat-adoption_microchip=985&d_cat-adoption_fee=75"),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let title: String =
        sqlx::query_scalar("select title from homestead.release_requests").fetch_one(&pool).await.expect("title");
    assert_eq!(title, "Cat Adoption Agreement · Ana · Mochi, grey tabby · 985 · 75");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn bundles_preselect_what_each_kind_of_signer_needs(pool: PgPool) {
    let documenso = MockServer::start().await;
    let r = router(pool, &documenso, true).await;
    let page = |q: &'static str| {
        Request::get(format!("/releases{q}"))
            .header(header::COOKIE, format!("sbh_session={TOKEN}"))
            .body(Body::empty())
            .expect("req")
    };
    let checked = |html: &str, key: &str| {
        let at = html.find(&format!(r#"name="r_{key}""#)).expect(key);
        html[at..at + 60].contains("checked")
    };
    let (_, default) = send(&r, page("")).await;
    assert!(
        checked(&default, "onboarding") && !checked(&default, "liability-waiver") && !checked(&default, "nda"),
        "joining signs the one agreement, not its parts again"
    );
    let (_, visitor) = send(&r, page("?bundle=visitor")).await;
    assert!(checked(&visitor, "liability-waiver") && checked(&visitor, "photo-release"));
    assert!(!checked(&visitor, "onboarding") && !checked(&visitor, "nda"));
    let (_, adoption) = send(&r, page("?bundle=adoption")).await;
    assert!(checked(&adoption, "cat-adoption") && !checked(&adoption, "liability-waiver"));
    assert!(adoption.contains(r#"name="d_cat-adoption_fee""#), "sender details are asked for");
}

// ── Joining through Discord ──────────────────────────────────────────────────

fn onboard(user: &str, options: serde_json::Value) -> Request<Body> {
    use ed25519_dalek::Signer as _;
    let body = json!({
        "type": 2, "application_id": "app1", "token": format!("tok-{user}"),
        "data": { "name": "onboard", "options": options },
        "member": { "user": { "id": user, "username": format!("user{user}") } },
    })
    .to_string();
    let ts = time::OffsetDateTime::now_utc().unix_timestamp().to_string();
    let sig = hex::encode(discord_key().sign(format!("{ts}{body}").as_bytes()).to_bytes());
    Request::post("/discord/interactions")
        .header("x-signature-ed25519", sig)
        .header("x-signature-timestamp", ts)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("req")
}

fn discord_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[42u8; 32])
}

/// The deferred reply's edits Discord received for `user`, in order.
async fn replies(discord: &MockServer, user: &str) -> Vec<String> {
    let want = format!("/webhooks/app1/tok-{user}/messages/@original");
    for _ in 0..100 {
        let got: Vec<String> = discord
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path() == want)
            .map(|r| serde_json::from_slice::<serde_json::Value>(&r.body).expect("json")["content"].to_string())
            .collect();
        if !got.is_empty() {
            return got;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("no reply edited for {user}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn onboarding_sends_the_agreement_once_and_signing_it_grants_the_role(pool: PgPool) {
    use homestead_integrations::discord::InteractionVerifier;
    let (documenso, discord) = (MockServer::start().await, MockServer::start().await);
    Mock::given(method("POST"))
        .and(path("/api/v2/envelope/create"))
        .respond_with(UniqueEnvelope::default())
        .expect(1)
        .mount(&documenso)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/envelope/distribute"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "success": true, "recipients": [
            { "email": "jane@example.org", "signingUrl": "https://sign.example/sign/tok_jane" }
        ]})))
        .mount(&documenso)
        .await;
    Mock::given(method("PATCH")).respond_with(ResponseTemplate::new(200)).mount(&discord).await;
    Mock::given(method("GET"))
        .and(path("/guilds/g1/roles"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": "r1", "name": "Steward" }, { "id": "r2", "name": "Crew" }, { "id": "r3", "name": "Volunteer" },
        ])))
        .mount(&discord)
        .await;
    Mock::given(method("GET"))
        .and(path("/guilds/g1/members"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "user": { "id": "111111111111", "username": "user111111111111" }, "roles": ["r3"] },
            { "user": { "id": "333333333333", "username": "kiki" }, "roles": ["r2"] },
            { "user": { "id": "444444444444", "username": "steward" }, "roles": ["r1"] },
            { "user": { "id": "555555555555", "username": "helperbot", "bot": true }, "roles": ["r2"] },
            { "user": { "id": "666666666666", "username": "lurker" }, "roles": [] },
        ])))
        .mount(&discord)
        .await;
    let feed = homestead_server::feeds::DiscordFeed {
        reader: homestead_integrations::discord::feed::Reader::new(
            http_client(),
            Some(discord.uri()),
            "bot".into(),
            "g1".into(),
        ),
        channels: vec![],
        cache: Default::default(),
        holders: Default::default(),
    };
    let verifier = InteractionVerifier::from_hex(&hex::encode(discord_key().verifying_key().as_bytes())).expect("key");
    let r = app(AppState {
        discord: Some(Arc::new(verifier)),
        discord_api: discord.uri().into(),
        discord_feed: Some(Arc::new(feed)),
        ..state(pool.clone(), &documenso, true).await
    });
    let ask = |user: &str, role: Option<&str>| {
        let mut o =
            vec![json!({"name": "name", "value": " Jane Doe "}), json!({"name": "email", "value": "jane@example.org"})];
        if let Some(role) = role {
            o.push(json!({"name": "joining_as", "value": role}));
        }
        onboard(user, json!(o))
    };

    // Documenso may be asleep, so Discord hears "thinking…" at once, only
    // by the asker, and the link arrives as an edit.
    let (status, body) = send(&r, ask("111111111111", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).expect("json"),
        json!({"type": 5, "data": {"flags": 64}})
    );
    let edits = replies(&discord, "111111111111").await;
    assert!(edits[0].contains("https://sign.example/sign/tok_jane") && edits[0].contains("**Volunteer**"), "{edits:?}");
    let key: String =
        sqlx::query_scalar("select release_key from homestead.release_requests").fetch_one(&pool).await.expect("row");
    assert_eq!(key, "onboarding", "the one agreement, not its parts");

    // Asking again while it's out answers with the same link, straight
    // away, and creates no second envelope (create is expected once).
    let (_, body) = send(&r, ask("111111111111", None)).await;
    let again: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!((again["type"].as_u64(), again["data"]["flags"].as_u64()), (Some(4), Some(64)));
    assert!(again["data"]["content"].as_str().expect("s").contains("tok_jane"));

    // Signing it grants the role: the event names the member, not the email.
    let id: uuid::Uuid =
        sqlx::query_scalar("select id from homestead.release_requests").fetch_one(&pool).await.expect("id");
    assert_eq!(send(&r, hook(SECRET, "DOCUMENT_COMPLETED", json!(external_id(id)))).await.0, StatusCode::OK);
    assert_eq!(events(&pool).await, ["release.signed", "member.onboarded"]);
    let page = send(
        &r,
        Request::get("/releases")
            .header(header::COOKIE, format!("sbh_session={TOKEN}"))
            .body(Body::empty())
            .expect("r"),
    )
    .await
    .1;
    assert!(page.contains("@user111111111111") && page.contains("1 signed · 0 still to sign"), "{page}");
    // Kiki was given Crew by hand and never signed; the steward, the bot
    // and the roleless lurker aren't asked to.
    assert!(page.contains("1 in Discord hold a role without a matching signed agreement"), "{page}");
    assert!(page.contains("<li>@kiki · Crew</li>"), "{page}");
    assert!(!page.contains("@steward") && !page.contains("@helperbot") && !page.contains("@lurker"), "{page}");

    // Once signed, another role needs no second signature.
    let (_, body) = send(&r, ask("111111111111", Some("Community"))).await;
    assert!(body.contains("already signed") && body.contains("Community"), "{body}");
    assert_eq!(events(&pool).await, ["release.signed", "member.onboarded", "member.onboarded"]);

    // Roles above Volunteer can't be asked for.
    let (_, body) = send(&r, ask("222222222222", Some("Crew"))).await;
    assert!(body.contains("Crew"), "{body}");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from homestead.onboardings").fetch_one(&pool).await.expect("n"),
        1
    );
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn onboarding_is_off_until_the_agreement_is_approved(pool: PgPool) {
    use homestead_integrations::discord::InteractionVerifier;
    let documenso = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).expect(0).mount(&documenso).await;
    let verifier = InteractionVerifier::from_hex(&hex::encode(discord_key().verifying_key().as_bytes())).expect("key");
    let r = app(AppState { discord: Some(Arc::new(verifier)), ..state(pool, &documenso, false).await });
    let (_, body) = send(
        &r,
        onboard(
            "111111111111",
            json!([{"name": "name", "value": "Jane"}, {"name": "email", "value": "j@example.org"}]),
        ),
    )
    .await;
    assert!(body.contains("isn't switched on yet"), "{body}");
}

fn public(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).expect("req")
}

/// Their page, once the background send has finished.
async fn settled_page(r: &Router, page: &str) -> String {
    for _ in 0..200 {
        let (status, body) = send(r, public(page)).await;
        assert_eq!(status, StatusCode::OK);
        if !body.contains("Getting your welcome agreement ready") {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the agreement was never sent");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn joining_from_the_link_signs_then_oauth_joins_and_grants_the_role(pool: PgPool) {
    use homestead_integrations::discord::feed::Reader;
    let documenso = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/envelope/create"))
        .respond_with(UniqueEnvelope::default())
        .expect(1)
        .mount(&documenso)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/envelope/distribute"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "recipients": [
            { "email": "sam@example.org", "signingUrl": "https://sign.example/sign/tok_sam" }
        ]})))
        .mount(&documenso)
        .await;
    let discord = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "access_token": "person-token" })))
        .mount(&discord)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/@me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "77", "username": "sam.grows" })))
        .up_to_n_times(1)
        .mount(&discord)
        .await;
    // Whoever holds the link next tries it with a second account.
    Mock::given(method("GET"))
        .and(path("/users/@me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "88", "username": "tagalong" })))
        .mount(&discord)
        .await;
    Mock::given(method("PUT"))
        .and(path("/guilds/g1/members/88"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
        .expect(0)
        .mount(&discord)
        .await;
    Mock::given(method("GET"))
        .and(path("/guilds/g1/roles"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "id": "55", "name": "Volunteer" }])))
        .mount(&discord)
        .await;
    Mock::given(method("PUT"))
        .and(path("/guilds/g1/members/77"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
        .expect(1)
        .mount(&discord)
        .await;
    Mock::given(method("PUT"))
        .and(path("/guilds/g1/members/77/roles/55"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&discord)
        .await;
    let mut s = state(pool.clone(), &documenso, true).await;
    s.public_url = Some("https://core.example".into());
    s.discord_join = Some(Arc::new(homestead_server::join::DiscordJoin {
        reader: Reader::new(http_client(), Some(discord.uri()), "bot".into(), "g1".into()),
        application_id: "app1".into(),
        client_secret: "secret".into(),
        redirect_uri: "https://core.example/join/discord".into(),
    }));
    let r = app(s);

    let (status, page) = send(&r, public("/join")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains(r#"action="/join""#), "{page}");

    let start = Request::post("/join")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("name=Sam+Sprout&email=Sam%40Example.org"))
        .expect("req");
    let resp = r.clone().oneshot(start).await.expect("infallible");
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let mine = resp.headers()[header::LOCATION].to_str().expect("location").to_owned();
    assert!(mine.starts_with("/join/") && mine.len() == "/join/".len() + 64, "{mine}");

    let page = settled_page(&r, &mine).await;
    assert!(page.contains("https://sign.example/sign/tok_sam"), "{page}");
    assert!(!page.contains("discord.com/oauth2"), "no Discord access before signing: {page}");
    let received = documenso.received_requests().await.expect("recorded");
    let paths: Vec<&str> = received.iter().map(|r| r.url.path()).collect();
    assert_eq!(paths[..2], ["/api/health", "/api/v2/envelope/create"], "a sleeping Documenso is woken first");
    let created = &received[1];
    let body = String::from_utf8_lossy(&created.body);
    assert!(
        body.contains(&format!(r#""redirectUrl":"https://core.example{mine}?signed""#)),
        "signing returns them: {body}"
    );
    let (_, back) = send(&r, public(&format!("{mine}?signed"))).await;
    assert!(
        back.contains("Confirming your signature") && !back.contains("https://sign.example/sign/tok_sam"),
        "back from signing before the webhook: no second invitation to sign: {back}"
    );

    let id: uuid::Uuid =
        sqlx::query_scalar("select release_request_id from homestead.joins").fetch_one(&pool).await.expect("join");
    let join: uuid::Uuid = sqlx::query_scalar("select id from homestead.joins").fetch_one(&pool).await.expect("id");
    let early = Store::from_pool(pool.clone()).claim_join_discord(join, "bad", "sneaky").await;
    assert!(early.is_err(), "no Discord identity is recorded for an unsigned agreement");

    assert_eq!(send(&r, hook(SECRET, "DOCUMENT_COMPLETED", json!(external_id(id)))).await.0, StatusCode::OK);
    let page = send(&r, public(&mine)).await.1;
    assert!(page.contains("discord.com/oauth2/authorize") && page.contains("guilds.join"), "{page}");
    let state = mine.trim_start_matches("/join/");
    let cancelled = r.clone().oneshot(public(&format!("/join/discord?error=access_denied&state={state}"))).await;
    let cancelled = cancelled.expect("cancel");
    assert_eq!(cancelled.status(), StatusCode::SEE_OTHER, "Cancel on Discord goes back, not to an error");
    assert_eq!(cancelled.headers()[header::LOCATION], mine.as_str());
    let callback = format!("/join/discord?code=oauth-code&state={state}");
    let response = r.clone().oneshot(public(&callback)).await.expect("callback");
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let again = send(&r, public(&mine)).await.1;
    assert!(again.contains("Welcome, @sam.grows") && !again.contains("oauth2/authorize"), "{again}");
    let second = r.clone().oneshot(public(&callback)).await.expect("second callback");
    assert_ne!(second.status(), StatusCode::SEE_OTHER, "one signed agreement admits one Discord account");
    let after = send(&r, public(&mine)).await.1;
    assert!(after.contains("Welcome, @sam.grows") && !after.contains("tagalong"), "{after}");
    assert_eq!(events(&pool).await, ["release.signed"], "no Discord account to grant a role to directly");

    let (_, stranger) = send(&r, public(&format!("/join/{}", "0".repeat(64)))).await;
    assert!(stranger.contains("couldn't find that link") && !stranger.contains("oauth2/authorize"), "{stranger}");

    // Stewards see who joined from the link.
    let releases = send(
        &r,
        Request::get("/releases")
            .header(header::COOKIE, format!("sbh_session={TOKEN}"))
            .body(Body::empty())
            .expect("r"),
    )
    .await
    .1;
    assert!(releases.contains("Sam Sprout") && releases.contains("@sam.grows"), "{releases}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_join_page_is_closed_until_releases_and_discord_are_ready(pool: PgPool) {
    let documenso = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).expect(0).mount(&documenso).await;
    let r = router(pool.clone(), &documenso, true).await; // no Discord bot
    let (_, page) = send(&r, public("/join")).await;
    assert!(page.contains("isn't open right now") && !page.contains(r#"action="/join""#), "{page}");
    let start = Request::post("/join")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("name=Sam&email=sam%40example.org"))
        .expect("req");
    assert_eq!(send(&r, start).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let joins: i64 = sqlx::query_scalar("select count(*) from homestead.joins").fetch_one(&pool).await.expect("n");
    assert_eq!(joins, 0);
}

/// A router with joining open: releases approved and a Discord bot to add
/// signers. Discord itself isn't called before signing.
async fn open_join(pool: &PgPool, documenso: &MockServer) -> Router {
    use homestead_integrations::discord::feed::Reader;
    let mut s = state(pool.clone(), documenso, true).await;
    s.public_url = Some("https://core.example".into());
    s.discord_join = Some(Arc::new(homestead_server::join::DiscordJoin {
        reader: Reader::new(http_client(), Some("http://127.0.0.1:9".into()), "bot".into(), "g1".into()),
        application_id: "app1".into(),
        client_secret: "secret".into(),
        redirect_uri: "https://core.example/join/discord".into(),
    }));
    app(s)
}

fn join_form(body: &str) -> Request<Body> {
    Request::post("/join")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body.to_owned()))
        .expect("req")
}

async fn location(r: &Router, req: Request<Body>) -> String {
    let resp = r.clone().oneshot(req).await.expect("infallible");
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    resp.headers()[header::LOCATION].to_str().expect("location").to_owned()
}

async fn count(pool: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.expect("count")
}

fn calls(received: &[wiremock::Request], to: &str) -> usize {
    received.iter().filter(|r| r.url.path() == to).count()
}

// On 2026-09-30 a core rollout cut three joins between Documenso's create
// and distribute. Their page said to start again, which made yet another
// envelope. Now the envelope is recorded between the two calls, and "Try
// again" distributes that same envelope.
#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_failed_join_is_retried_with_the_envelope_it_already_has(pool: PgPool) {
    let documenso = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/envelope/create"))
        .respond_with(UniqueEnvelope::default())
        .mount(&documenso)
        .await;
    // A refusal, not a 5xx: it isn't retried within the send.
    Mock::given(method("POST"))
        .and(path("/api/v2/envelope/distribute"))
        .respond_with(ResponseTemplate::new(400).set_body_string("not now"))
        .up_to_n_times(1)
        .mount(&documenso)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/envelope/distribute"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "recipients": [
            { "email": "sam@example.org", "signingUrl": "https://sign.example/sign/tok_sam" }
        ]})))
        .mount(&documenso)
        .await;
    let r = open_join(&pool, &documenso).await;

    let mine = location(&r, join_form("name=Sam+Sprout&email=sam%40example.org")).await;
    let failed = settled_page(&r, &mine).await;
    let retry = format!("{mine}/retry");
    assert!(
        failed.contains("couldn't prepare") && failed.contains(&format!(r#"action="{retry}""#)),
        "a failed send offers to try the same join again: {failed}"
    );

    assert_eq!(location(&r, Request::post(&retry).body(Body::empty()).expect("req")).await, mine);
    let page = settled_page(&r, &mine).await;
    assert!(page.contains("https://sign.example/sign/tok_sam"), "{page}");
    let received = documenso.received_requests().await.expect("recorded");
    assert_eq!(calls(&received, "/api/v2/envelope/create"), 1, "the retry doesn't make a second envelope");
    assert_eq!(calls(&received, "/api/v2/envelope/distribute"), 2);
    let (envelope, same_id): (String, bool) = sqlx::query_as(
        "select r.envelope_id, r.id = j.release_id
         from homestead.joins j join homestead.release_requests r on r.id = j.release_request_id",
    )
    .fetch_one(&pool)
    .await
    .expect("sent");
    assert_eq!(envelope, "envelope_0");
    assert!(same_id, "the webhook's id is the one the envelope was created with");

    // Once sent, "Try again" (an old tab, a second tap) sends nothing.
    location(&r, Request::post(&retry).body(Body::empty()).expect("req")).await;
    let received = documenso.received_requests().await.expect("recorded");
    assert_eq!(calls(&received, "/api/v2/envelope/distribute"), 2);
}

// A send cut off by a restart records no error: without a timeout its page
// would say "getting ready" for ever.
#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_send_cut_off_by_a_restart_can_be_retried(pool: PgPool) {
    let documenso = MockServer::start().await;
    let r = open_join(&pool, &documenso).await;
    let store = Store::from_pool(pool.clone());
    let (token, hash) = ("a".repeat(64), homestead_server::auth::hash_secret(&"a".repeat(64)));
    let job = store
        .start_join(&hash, "Sam", "sam@example.org", homestead_domain::onboarding::OnboardingRole::Volunteer)
        .await
        .expect("start")
        .expect("new");
    store.record_join_envelope(job.id, "envelope_cut").await.expect("created");
    let mine = format!("/join/{token}");

    let running = send(&r, public(&mine)).await.1;
    assert!(running.contains("Getting your welcome agreement ready"), "{running}");
    assert!(store.retry_join(job.id).await.expect("retry").is_none(), "a send still running isn't retried");

    sqlx::query("update homestead.joins set send_started_at = now() - interval '6 minutes'")
        .execute(&pool)
        .await
        .expect("age");
    let stalled = send(&r, public(&mine)).await.1;
    assert!(stalled.contains("couldn't prepare") && stalled.contains("/retry"), "{stalled}");
    let claimed = store.retry_join(job.id).await.expect("retry").expect("claimed");
    assert_eq!(claimed.envelope_id.as_deref(), Some("envelope_cut"), "the retry resumes the created envelope");
    assert!(store.retry_join(job.id).await.expect("again").is_none(), "of two taps, one retries");
}

// Two envelopes 100 ms apart on 2026-09-30: the form was sent twice.
#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn sending_the_join_form_twice_makes_one_join(pool: PgPool) {
    let documenso = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/envelope/create"))
        .respond_with(UniqueEnvelope::default())
        .mount(&documenso)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/envelope/distribute"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "recipients": [
            { "email": "sam@example.org", "signingUrl": "https://sign.example/sign/tok_sam" }
        ]})))
        .mount(&documenso)
        .await;
    let r = open_join(&pool, &documenso).await;

    let token_of = |page: &str| {
        let at = page.find(r#"name="t" value=""#).expect("the form carries its token") + r#"name="t" value=""#.len();
        page[at..at + 64].to_owned()
    };
    let shown = r.clone().oneshot(public("/join")).await.expect("page");
    assert_eq!(shown.headers()[header::CACHE_CONTROL], "no-store", "a cached form would share one join");
    let first = token_of(&send(&r, public("/join")).await.1);
    assert_ne!(first, token_of(&send(&r, public("/join")).await.1), "every form gets its own token");

    let body = format!("name=Sam&email=sam%40example.org&t={first}");
    let (a, b) = (location(&r, join_form(&body)).await, location(&r, join_form(&body)).await);
    assert_eq!(a, format!("/join/{first}"));
    assert_eq!(a, b, "the second tap lands on the same page");
    settled_page(&r, &a).await;
    assert_eq!(count(&pool, "select count(*) from homestead.joins").await, 1);
    let received = documenso.received_requests().await.expect("recorded");
    assert_eq!(calls(&received, "/api/v2/envelope/create"), 1);

    // A token that isn't one of ours isn't used as one.
    let odd = location(&r, join_form("name=Kim&email=kim%40example.org&t=short")).await;
    assert!(odd.len() == "/join/".len() + 64 && !odd.ends_with("short"), "{odd}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_distribute_that_fails_in_passing_is_retried(pool: PgPool) {
    let documenso = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/envelope/create"))
        .respond_with(UniqueEnvelope::default())
        .mount(&documenso)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/envelope/distribute"))
        .respond_with(ResponseTemplate::new(503).set_body_string("Signing is starting up."))
        .up_to_n_times(1)
        .mount(&documenso)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/envelope/distribute"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "recipients": [
            { "email": "sam@example.org", "signingUrl": "https://sign.example/sign/tok_sam" }
        ]})))
        .mount(&documenso)
        .await;
    let r = open_join(&pool, &documenso).await;
    let mine = location(&r, join_form("name=Sam&email=sam%40example.org")).await;
    let page = settled_page(&r, &mine).await;
    assert!(page.contains("https://sign.example/sign/tok_sam"), "{page}");
    let received = documenso.received_requests().await.expect("recorded");
    assert_eq!(calls(&received, "/api/v2/envelope/distribute"), 2);
}

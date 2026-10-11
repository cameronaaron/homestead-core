//! The Terms of Service and Privacy Policy: reachable signed out, linked
//! from every page, and saying what the code actually does.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_server::auth::{COOKIE, SESSION_TTL};
use homestead_server::config::File;
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;

mod common;

fn router(pool: PgPool) -> Router {
    app(AppState::new(Store::from_pool(pool)))
}

async fn get(r: &Router, uri: &str, cookie: Option<&str>) -> (StatusCode, String) {
    let mut b = Request::builder().uri(uri);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, format!("{COOKIE}={c}"));
    }
    let resp = r.clone().oneshot(b.body(Body::empty()).expect("request")).await.expect("infallible");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn links_both(page: &str) -> bool {
    page.contains(r#"href="/terms""#) && page.contains(r#"href="/privacy""#)
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn both_pages_are_public_and_linked_from_every_page(pool: PgPool) {
    common::steward(&pool).await;
    let r = router(pool);

    let (status, terms) = get(&r, "/terms", None).await;
    assert_eq!(status, StatusCode::OK, "{terms}");
    assert!(terms.contains("<h1>Terms of Service</h1>"), "{terms}");
    let (status, privacy) = get(&r, "/privacy", None).await;
    assert_eq!(status, StatusCode::OK, "{privacy}");
    assert!(privacy.contains("<h1>Privacy Policy</h1>"), "{privacy}");

    // Signed out (where Discord's reviewers and joiners land) and signed in.
    for (uri, cookie) in [("/login", None), ("/join", None), ("/adopt", None), ("/", Some(common::STEWARD))] {
        let (status, page) = get(&r, uri, cookie).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {page}");
        assert!(links_both(&page), "{uri} has no Terms/Privacy links");
    }
}

/// The policy states facts about the code. Each one is checked against its
/// source, so changing the behaviour without the text fails here.
#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_privacy_policy_matches_the_code(pool: PgPool) {
    let (_, privacy) = get(&router(pool), "/privacy", None).await;

    assert!(privacy.contains(&format!("<code>{COOKIE}</code>")), "cookie name");
    let days = SESSION_TTL.whole_days();
    assert!(privacy.contains(&format!("up to {days} days")), "session length {days}");

    let join = include_str!("../src/join.rs");
    assert!(join.contains(r#".append_pair("scope", "identify guilds.join")"#), "the Discord scopes changed");
    assert!(privacy.contains("<code>identify</code> and <code>guilds.join</code>"), "scopes in the policy");

    let config = File::parse(include_str!("../../../homestead.toml")).expect("homestead.toml");
    let retain = config.attachments.retain_deleted_days;
    assert!(privacy.contains(&format!("after {retain} days")), "attachment retention {retain}");
}

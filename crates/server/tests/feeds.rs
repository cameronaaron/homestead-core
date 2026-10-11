//! The Community page shows Discord and Buffer, read through fakes.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use homestead_integrations::buffer;
use homestead_integrations::discord::feed::Reader;
use homestead_server::feeds::{DiscordFeed, SocialFeed};
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use serde_json::json;
use sqlx::PgPool;
use tower::ServiceExt;
use wiremock::matchers::{body_string_contains, header as h, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;

async fn page(r: &Router) -> (StatusCode, String) {
    let req = Request::builder()
        .uri("/community")
        .header(header::COOKIE, format!("sbh_session={}", common::STEWARD))
        .body(Body::empty())
        .expect("request");
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn router(pool: PgPool, discord: &MockServer, buffer: &MockServer) -> Router {
    let reader = Reader::new(reqwest::Client::new(), Some(discord.uri()), "bot".into(), "g1".into());
    let state = AppState {
        discord_feed: Some(Arc::new(DiscordFeed {
            reader,
            channels: vec!["announcements".into(), "general".into()],
            cache: Default::default(),
            holders: Default::default(),
        })),
        social_feed: Some(Arc::new(SocialFeed {
            client: buffer::Client::new(reqwest::Client::new(), Some(buffer.uri()), "buf".into()),
            organization_id: "org1".into(),
            cache: Default::default(),
        })),
        ..AppState::new(Store::from_pool(pool))
    };
    app(state)
}

async fn discord_server() -> MockServer {
    let d = MockServer::start().await;
    let ok = |v: serde_json::Value| ResponseTemplate::new(200).set_body_json(v);
    Mock::given(method("GET"))
        .and(path("/guilds/g1/channels"))
        .and(h("authorization", "Bot bot"))
        .respond_with(ok(json!([
            { "id": "10", "name": "announcements", "type": 0 },
            { "id": "11", "name": "general", "type": 0 },
            { "id": "12", "name": "leadership", "type": 0 }
        ])))
        .expect(1) // the second view is served from the cache
        .mount(&d)
        .await;
    Mock::given(method("GET"))
        .and(path("/channels/10/messages"))
        .respond_with(ok(json!([{
            "id": "900", "type": 0, "timestamp": "2026-09-28T18:00:00.000000+00:00",
            "content": "Work day Saturday: coop build!", "author": { "username": "photomancer", "global_name": null },
            "mentions": [], "attachments": [], "embeds": []
        }])))
        .mount(&d)
        .await;
    Mock::given(method("GET"))
        .and(path("/channels/11/messages"))
        .respond_with(ok(
            json!([{ "id": "901", "type": 7, "content": "", "timestamp": "2026-09-28T07:24:38.365000+00:00",
                                  "author": { "username": "bot" }, "mentions": [], "attachments": [], "embeds": [] }]),
        ))
        .mount(&d)
        .await;
    Mock::given(method("GET"))
        .and(path("/channels/12/messages"))
        .respond_with(ok(json!([])))
        .expect(0) // not a feed channel: never read
        .mount(&d)
        .await;
    Mock::given(method("GET"))
        .and(path("/guilds/g1/scheduled-events"))
        .respond_with(ok(json!([{
            "id": "e1", "name": "CRFG small-space gardening talk", "description": "", "status": 1,
            "scheduled_start_time": "2026-11-24T02:00:00+00:00", "entity_metadata": { "location": "The homestead" },
            "channel_id": null, "user_count": 6
        }])))
        .mount(&d)
        .await;
    d
}

async fn buffer_server(connected: bool) -> MockServer {
    let b = MockServer::start().await;
    let ok = |v: serde_json::Value| ResponseTemplate::new(200).set_body_json(json!({ "data": v }));
    Mock::given(method("POST"))
        .and(body_string_contains("account"))
        .respond_with(ok(json!({ "account": { "organizations": [{ "id": "org1", "name": "Sun Brewed Homestead" }] } })))
        .mount(&b)
        .await;
    let channels = if connected {
        json!([{ "name": "sunbrewedhomestead", "displayName": "sunbrewedhomestead", "service": "instagram", "isQueuePaused": false }])
    } else {
        json!([])
    };
    Mock::given(method("POST"))
        .and(body_string_contains("isQueuePaused"))
        .respond_with(ok(json!({ "channels": channels })))
        .mount(&b)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("ideas("))
        .respond_with(ok(json!({
            "posts": { "edges": if connected { json!([{ "node": {
                "text": "Fresh eggs at the stand this Saturday", "status": "scheduled",
                "dueAt": "2099-10-03T16:00:00.000Z", "channelService": "instagram",
                "channel": { "displayName": "sunbrewedhomestead" }, "error": null } }]) } else { json!([]) } },
            "ideas": { "edges": [
                { "node": { "content": { "title": "📸 Needs photo · Fresh harvest: Kale" }, "createdAt": 1790700000 } },
                { "node": { "content": { "title": "This is a place to plan ✍️ your content" }, "createdAt": 1790666520 } }
            ] }
        })))
        .mount(&b)
        .await;
    b
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn community_shows_discord_and_social_media(pool: PgPool) {
    common::steward(&pool).await;
    let (discord, buffer) = (discord_server().await, buffer_server(true).await);
    let r = router(pool, &discord, &buffer);
    let (status, html) = page(&r).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Work day Saturday: coop build!") && html.contains("photomancer"), "a real post");
    assert!(html.contains("https://discord.com/channels/g1/10/900"), "links the post");
    assert!(html.contains("Nothing posted yet in #general"), "a join notice isn't a post");
    assert!(html.contains("CRFG small-space gardening talk") && html.contains("6 interested"));
    assert!(html.contains("Fresh eggs at the stand this Saturday") && html.contains("scheduled"));
    assert!(html.contains("1 draft") && html.contains("Fresh harvest: Kale"));
    assert!(!html.contains("place to plan"), "Buffer's sample ideas aren't Homestead's drafts");
    page(&r).await; // cached: the channel list `expect(1)` checks it
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn an_unconnected_buffer_and_a_broken_discord_say_so(pool: PgPool) {
    common::steward(&pool).await;
    let discord = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"message": "401: Unauthorized", "code": 0}"#))
        .mount(&discord)
        .await;
    let buffer = buffer_server(false).await;
    let (status, html) = page(&router(pool, &discord, &buffer)).await;
    assert_eq!(status, StatusCode::OK, "the rest of the page still works");
    assert!(html.contains("Couldn't read Discord: Discord HTTP 401"), "{html}");
    assert!(html.contains("No Instagram or Facebook account is connected in Buffer yet"));
    assert!(html.contains(r#"href="/ops/social""#));
}

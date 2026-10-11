//! Wire-level behaviour of the Discord REST client against a mock server.

use homestead_integrations::discord::provision::http::DiscordHttp;
use homestead_integrations::discord::provision::{Api, ChannelBody, Overwrite};
use homestead_integrations::http_client;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn api(server: &MockServer) -> DiscordHttp {
    DiscordHttp::new(http_client(), server.uri(), "bot-token".into(), "100".into())
}

#[tokio::test]
async fn parses_permission_strings_beyond_2_pow_53() {
    let server = MockServer::start().await;
    let big: u64 = (1 << 49) | (1 << 44) | 1;
    Mock::given(method("GET"))
        .and(path("/guilds/100/roles"))
        .and(header("Authorization", "Bot bot-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": "100", "name": "@everyone", "permissions": big.to_string(), "color": 0,
              "hoist": false, "mentionable": false, "position": 0, "managed": false }
        ])))
        .mount(&server)
        .await;
    let roles = api(&server).roles().await.expect("roles");
    assert_eq!(roles[0].permissions, big);
}

#[tokio::test]
async fn honours_rate_limits_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/guilds/100/channels"))
        .respond_with(
            ResponseTemplate::new(429)
                .set_body_json(json!({ "message": "slow", "retry_after": 0.05, "global": false })),
        )
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/guilds/100/channels"))
        .and(body_partial_json(json!({
            "name": "farm-feed", "type": 0, "parent_id": "300", "topic": "t", "rate_limit_per_user": 5,
            "permission_overwrites": [{ "id": "100", "type": 0, "allow": "0", "deny": "1024" }]
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "301", "name": "farm-feed", "type": 0, "parent_id": "300", "topic": "t", "position": 0,
            "permission_overwrites": [{ "id": "100", "type": 0, "allow": "0", "deny": "1024" }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let body = ChannelBody {
        name: "farm-feed".into(),
        kind: 0,
        parent_id: Some("300".into()),
        topic: Some("t".into()),
        overwrites: vec![Overwrite { id: "100".into(), kind: 0, allow: 0, deny: 1024 }],
        // Settings travel with the typed fields; `name` here must not win.
        settings: [("rate_limit_per_user".to_owned(), json!(5)), ("name".to_owned(), json!("hijack"))]
            .into_iter()
            .collect(),
    };
    let created = api(&server).create_channel(&body).await.expect("created after retries");
    assert_eq!(created.id, "301");
    assert_eq!(created.overwrites, body.overwrites);
}

#[tokio::test]
async fn permission_errors_explain_the_fix() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/guilds/100"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "message": "Missing Access", "code": 50001 })))
        .mount(&server)
        .await;
    let err = api(&server).guild().await.expect_err("403");
    assert!(err.contains("needs Administrator"), "{err}");
}

/// Members come in pages of 1000 keyed by the last user id.
#[tokio::test]
async fn members_are_read_page_by_page() {
    use wiremock::matchers::query_param;
    let server = MockServer::start().await;
    let page: Vec<_> = (1..=1000)
        .map(|n| json!({ "user": { "id": n.to_string(), "username": format!("u{n}") }, "roles": [] }))
        .collect();
    Mock::given(method("GET"))
        .and(path("/guilds/100/members"))
        .and(query_param("after", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/guilds/100/members"))
        .and(query_param("after", "1000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "user": { "id": "1001", "username": "kiki" }, "roles": ["55"] },
            { "user": { "id": "1002", "username": "homestead", "bot": true }, "roles": [] }
        ])))
        .mount(&server)
        .await;
    let all = api(&server).members().await.expect("members");
    assert_eq!(all.len(), 1002);
    assert_eq!((all[1000].username.as_str(), all[1000].roles.as_slice()), ("kiki", ["55".to_owned()].as_slice()));
    assert!(all[1001].bot && !all[1000].bot);
}

#[tokio::test]
async fn missing_members_intent_says_how_to_fix_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/guilds/100/members"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "message": "Missing Access", "code": 50001 })))
        .mount(&server)
        .await;
    let err = api(&server).members().await.expect_err("403");
    assert!(err.contains("Server Members Intent"), "{err}");
}

#[tokio::test]
async fn member_roles_are_granted_and_revoked_one_at_a_time() {
    let server = MockServer::start().await;
    for verb in ["PUT", "DELETE"] {
        Mock::given(method(verb))
            .and(path("/guilds/100/members/9/roles/55"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
    }
    api(&server).add_member_role("9", "55").await.expect("add");
    api(&server).remove_member_role("9", "55").await.expect("remove");
}

#[tokio::test]
async fn signed_join_oauth_identifies_adds_and_grants_the_exact_member() {
    use homestead_integrations::discord::feed::Reader;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "access_token": "user-token" })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/@me"))
        .and(header("authorization", "Bearer user-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "9", "username": "riley" })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/guilds/100/roles"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": "100", "name": "@everyone" }, { "id": "55", "name": "Volunteer" }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/guilds/100/members/9"))
        .and(body_partial_json(json!({ "access_token": "user-token", "roles": ["55"] })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/guilds/100/members/9/roles/55"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let reader = Reader::new(http_client(), Some(server.uri()), "bot-token".into(), "100".into());
    let grant =
        reader.oauth_identify("app", "secret", "https://core.test/join/discord", "code").await.expect("identify");
    assert_eq!((grant.user.id.as_str(), grant.user.username.as_str()), ("9", "riley"));
    reader.add_joiner(&grant, "Volunteer").await.expect("add");
}

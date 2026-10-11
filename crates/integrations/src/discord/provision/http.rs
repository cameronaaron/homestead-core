//! [`Api`] over Discord's REST API v10, authenticated as a bot.

use std::time::Duration;

use homestead_bus::BoxFuture;
use reqwest::Method;
use serde_json::{Value, json};

use super::*;

pub const DEFAULT_BASE: &str = "https://discord.com/api/v10";

/// Longest rate-limit wait we'll honour before giving up.
const MAX_WAIT: Duration = Duration::from_secs(60);
const MAX_RETRIES: usize = 6;

pub struct DiscordHttp {
    client: reqwest::Client,
    base: String,
    token: String,
    guild_id: String,
}

impl DiscordHttp {
    pub fn new(client: reqwest::Client, base: String, token: String, guild_id: String) -> Self {
        Self { client, base, token, guild_id }
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> ApiResult<Value> {
        let url = format!("{}{path}", self.base);
        for attempt in 0..=MAX_RETRIES {
            let mut req = self
                .client
                .request(method.clone(), &url)
                .header("Authorization", format!("Bot {}", self.token))
                .header("X-Audit-Log-Reason", "homestead-provision");
            if let Some(b) = &body {
                req = req.json(b);
            }
            let resp = req.send().await.map_err(|e| format!("{method} {path}: {e}"))?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status.as_u16() == 429 && attempt < MAX_RETRIES {
                // Discord reports the wait in seconds (fractional) in the body.
                let secs =
                    serde_json::from_str::<Value>(&text).ok().and_then(|v| v["retry_after"].as_f64()).unwrap_or(1.0);
                tokio::time::sleep(Duration::from_secs_f64(secs.max(0.0)).min(MAX_WAIT)).await;
                continue;
            }
            if status.is_server_error() && attempt < 2 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            if !status.is_success() {
                let hint = match status.as_u16() {
                    401 => " (check DISCORD_BOT_TOKEN)",
                    403 => " (the bot needs Administrator, and its role must be above the roles it manages)",
                    404 => " (check DISCORD_GUILD_ID, and that the bot has been added to that server)",
                    _ => "",
                };
                let snippet: String = text.chars().take(400).collect();
                return Err(format!("{method} {path} → HTTP {status}{hint}: {snippet}"));
            }
            return Ok(if text.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&text).map_err(|e| e.to_string())?
            });
        }
        Err(format!("{method} {path}: still rate limited after {MAX_RETRIES} retries"))
    }

    fn g(&self) -> &str {
        &self.guild_id
    }
}

fn s(v: &Value) -> String {
    v.as_str().map(str::to_owned).unwrap_or_default()
}

fn opt_s(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}

/// Permission fields are decimal strings (they exceed 2^53).
fn bits(v: &Value) -> u64 {
    v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_u64()).unwrap_or(0)
}

/// A JSON object's fields, for settings pass-through.
fn fields(v: &Value) -> Settings {
    v.as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default()
}

/// Settings go in first, so a typed field can never be overridden by them.
fn merged(settings: &Settings, typed: Value) -> Value {
    let mut out: serde_json::Map<String, Value> = settings.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    out.extend(typed.as_object().cloned().unwrap_or_default());
    Value::Object(out)
}

fn small(v: &Value) -> u8 {
    v.as_u64().and_then(|n| u8::try_from(n).ok()).unwrap_or(0)
}

pub(crate) fn parse_role(v: &Value) -> RoleState {
    RoleState {
        id: s(&v["id"]),
        name: s(&v["name"]),
        permissions: bits(&v["permissions"]),
        color: v["color"].as_u64().and_then(|c| u32::try_from(c).ok()).unwrap_or(0),
        hoist: v["hoist"].as_bool().unwrap_or(false),
        mentionable: v["mentionable"].as_bool().unwrap_or(false),
        position: v["position"].as_i64().unwrap_or(0),
        managed: v["managed"].as_bool().unwrap_or(false),
        bot_id: v["tags"]["bot_id"].as_str().map(str::to_owned),
    }
}

pub(crate) fn parse_channel(v: &Value) -> ChannelState {
    ChannelState {
        id: s(&v["id"]),
        name: s(&v["name"]),
        kind: small(&v["type"]),
        parent_id: opt_s(&v["parent_id"]),
        topic: opt_s(&v["topic"]),
        position: v["position"].as_i64().unwrap_or(0),
        overwrites: v["permission_overwrites"]
            .as_array()
            .map(|os| {
                os.iter()
                    .map(|o| Overwrite {
                        id: s(&o["id"]),
                        kind: small(&o["type"]),
                        allow: bits(&o["allow"]),
                        deny: bits(&o["deny"]),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        fields: fields(v),
    }
}

fn role_json(b: &RoleBody) -> Value {
    json!({
        "name": b.name, "permissions": b.permissions.to_string(), "color": b.color,
        "hoist": b.hoist, "mentionable": b.mentionable,
    })
}

pub(crate) fn channel_json(b: &ChannelBody, create: bool) -> Value {
    let overwrites: Vec<Value> = b
        .overwrites
        .iter()
        .map(|o| json!({ "id": o.id, "type": o.kind, "allow": o.allow.to_string(), "deny": o.deny.to_string() }))
        .collect();
    let mut v = json!({ "name": b.name, "parent_id": b.parent_id, "permission_overwrites": overwrites });
    if create {
        v["type"] = json!(b.kind);
    }
    if let Some(t) = &b.topic {
        v["topic"] = json!(t);
    }
    merged(&b.settings, v)
}

pub(crate) fn message_json(body: &MessageBody) -> Value {
    // Welcome/rules text must never ping anyone. Editing to an embed-only
    // post must also clear any old content, so both are always sent.
    let embeds: Vec<Value> = body
        .embed
        .iter()
        .map(|e| json!({ "title": e.title, "description": e.description, "color": e.color }))
        .collect();
    json!({ "content": body.content, "embeds": embeds, "allowed_mentions": { "parse": [] } })
}

pub(crate) fn parse_message(m: &Value) -> MessageState {
    let e = &m["embeds"][0];
    let embed = e.is_object().then(|| Embed {
        title: s(&e["title"]),
        description: s(&e["description"]),
        color: e["color"].as_u64().and_then(|c| u32::try_from(c).ok()).unwrap_or(0),
    });
    MessageState {
        id: s(&m["id"]),
        author_id: s(&m["author"]["id"]),
        kind: small(&m["type"]),
        body: MessageBody { content: s(&m["content"]), embed },
        pinned: m["pinned"].as_bool().unwrap_or(false),
    }
}

pub(crate) fn parse_member(m: &Value) -> MemberState {
    MemberState {
        user_id: s(&m["user"]["id"]),
        username: s(&m["user"]["username"]),
        roles: m["roles"].as_array().map(|a| a.iter().map(s).collect()).unwrap_or_default(),
        bot: m["user"]["bot"].as_bool().unwrap_or(false),
    }
}

fn positions_json(p: &[(String, i64)]) -> Value {
    Value::Array(p.iter().map(|(id, pos)| json!({ "id": id, "position": pos })).collect())
}

impl Api for DiscordHttp {
    fn bot_user(&self) -> BoxFuture<'_, ApiResult<BotUser>> {
        Box::pin(async move {
            let v = self.call(Method::GET, "/users/@me", None).await?;
            Ok(BotUser { id: s(&v["id"]), avatar: opt_s(&v["avatar"]) })
        })
    }

    fn guild(&self) -> BoxFuture<'_, ApiResult<GuildInfo>> {
        Box::pin(async move {
            let v = self.call(Method::GET, &format!("/guilds/{}", self.g()), None).await?;
            Ok(GuildInfo {
                id: s(&v["id"]),
                name: s(&v["name"]),
                verification_level: small(&v["verification_level"]),
                default_message_notifications: small(&v["default_message_notifications"]),
                explicit_content_filter: small(&v["explicit_content_filter"]),
                system_channel_id: opt_s(&v["system_channel_id"]),
                icon: opt_s(&v["icon"]),
                fields: fields(&v),
            })
        })
    }

    fn roles(&self) -> BoxFuture<'_, ApiResult<Vec<RoleState>>> {
        Box::pin(async move {
            let v = self.call(Method::GET, &format!("/guilds/{}/roles", self.g()), None).await?;
            Ok(v.as_array().map(|a| a.iter().map(parse_role).collect()).unwrap_or_default())
        })
    }

    fn channels(&self) -> BoxFuture<'_, ApiResult<Vec<ChannelState>>> {
        Box::pin(async move {
            let v = self.call(Method::GET, &format!("/guilds/{}/channels", self.g()), None).await?;
            Ok(v.as_array().map(|a| a.iter().map(parse_channel).collect()).unwrap_or_default())
        })
    }

    fn messages<'a>(&'a self, channel_id: &'a str) -> BoxFuture<'a, ApiResult<Vec<MessageState>>> {
        Box::pin(async move {
            let v = self.call(Method::GET, &format!("/channels/{channel_id}/messages?limit=50"), None).await?;
            Ok(v.as_array().map(|a| a.iter().map(parse_message).collect()).unwrap_or_default())
        })
    }

    fn webhooks<'a>(&'a self, channel_id: &'a str) -> BoxFuture<'a, ApiResult<Vec<WebhookState>>> {
        Box::pin(async move {
            let v = self.call(Method::GET, &format!("/channels/{channel_id}/webhooks"), None).await?;
            Ok(v.as_array()
                .map(|a| {
                    a.iter()
                        .map(|w| WebhookState { id: s(&w["id"]), name: s(&w["name"]), token: opt_s(&w["token"]) })
                        .collect()
                })
                .unwrap_or_default())
        })
    }

    fn create_role<'a>(&'a self, b: &'a RoleBody) -> BoxFuture<'a, ApiResult<RoleState>> {
        Box::pin(async move {
            Ok(parse_role(&self.call(Method::POST, &format!("/guilds/{}/roles", self.g()), Some(role_json(b))).await?))
        })
    }

    fn modify_role<'a>(&'a self, id: &'a str, b: &'a RoleBody) -> BoxFuture<'a, ApiResult<()>> {
        Box::pin(async move {
            self.call(Method::PATCH, &format!("/guilds/{}/roles/{id}", self.g()), Some(role_json(b))).await.map(drop)
        })
    }

    fn set_everyone_permissions(&self, p: u64) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            let body = json!({ "permissions": p.to_string() });
            self.call(Method::PATCH, &format!("/guilds/{0}/roles/{0}", self.g()), Some(body)).await.map(drop)
        })
    }

    fn reorder_roles<'a>(&'a self, p: &'a [(String, i64)]) -> BoxFuture<'a, ApiResult<()>> {
        Box::pin(async move {
            self.call(Method::PATCH, &format!("/guilds/{}/roles", self.g()), Some(positions_json(p))).await.map(drop)
        })
    }

    fn create_channel<'a>(&'a self, b: &'a ChannelBody) -> BoxFuture<'a, ApiResult<ChannelState>> {
        Box::pin(async move {
            let v =
                self.call(Method::POST, &format!("/guilds/{}/channels", self.g()), Some(channel_json(b, true))).await?;
            Ok(parse_channel(&v))
        })
    }

    fn modify_channel<'a>(&'a self, id: &'a str, b: &'a ChannelBody) -> BoxFuture<'a, ApiResult<()>> {
        Box::pin(async move {
            self.call(Method::PATCH, &format!("/channels/{id}"), Some(channel_json(b, false))).await.map(drop)
        })
    }

    fn reorder_channels<'a>(&'a self, p: &'a [(String, i64)]) -> BoxFuture<'a, ApiResult<()>> {
        Box::pin(async move {
            self.call(Method::PATCH, &format!("/guilds/{}/channels", self.g()), Some(positions_json(p))).await.map(drop)
        })
    }

    fn delete_channel<'a>(&'a self, id: &'a str) -> BoxFuture<'a, ApiResult<()>> {
        Box::pin(async move { self.call(Method::DELETE, &format!("/channels/{id}"), None).await.map(drop) })
    }

    fn modify_guild<'a>(&'a self, b: &'a GuildBody) -> BoxFuture<'a, ApiResult<()>> {
        Box::pin(async move {
            let body = json!({
                "name": b.name,
                "verification_level": b.verification_level,
                "default_message_notifications": b.default_message_notifications,
                "explicit_content_filter": b.explicit_content_filter,
                "system_channel_id": b.system_channel_id,
            });
            let body = merged(&b.settings, body);
            self.call(Method::PATCH, &format!("/guilds/{}", self.g()), Some(body)).await.map(drop)
        })
    }

    fn create_webhook<'a>(&'a self, channel_id: &'a str, name: &'a str) -> BoxFuture<'a, ApiResult<WebhookState>> {
        Box::pin(async move {
            let v = self
                .call(Method::POST, &format!("/channels/{channel_id}/webhooks"), Some(json!({ "name": name })))
                .await?;
            Ok(WebhookState { id: s(&v["id"]), name: s(&v["name"]), token: opt_s(&v["token"]) })
        })
    }

    fn create_message<'a>(
        &'a self,
        channel_id: &'a str,
        body: &'a MessageBody,
    ) -> BoxFuture<'a, ApiResult<MessageState>> {
        Box::pin(async move {
            let v =
                self.call(Method::POST, &format!("/channels/{channel_id}/messages"), Some(message_json(body))).await?;
            Ok(parse_message(&v))
        })
    }

    fn edit_message<'a>(
        &'a self,
        channel_id: &'a str,
        id: &'a str,
        body: &'a MessageBody,
    ) -> BoxFuture<'a, ApiResult<()>> {
        Box::pin(async move {
            self.call(Method::PATCH, &format!("/channels/{channel_id}/messages/{id}"), Some(message_json(body)))
                .await
                .map(drop)
        })
    }

    fn set_message_pinned<'a>(
        &'a self,
        channel_id: &'a str,
        message_id: &'a str,
        pinned: bool,
    ) -> BoxFuture<'a, ApiResult<()>> {
        Box::pin(async move {
            let method = if pinned { Method::PUT } else { Method::DELETE };
            self.call(method, &format!("/channels/{channel_id}/messages/pins/{message_id}"), None).await.map(drop)
        })
    }

    fn set_guild_icon<'a>(&'a self, image: &'a str) -> BoxFuture<'a, ApiResult<()>> {
        Box::pin(async move {
            self.call(Method::PATCH, &format!("/guilds/{}", self.g()), Some(json!({ "icon": image }))).await.map(drop)
        })
    }

    fn set_bot_avatar<'a>(&'a self, image: &'a str) -> BoxFuture<'a, ApiResult<()>> {
        Box::pin(
            async move { self.call(Method::PATCH, "/users/@me", Some(json!({ "avatar": image }))).await.map(drop) },
        )
    }

    fn members(&self) -> BoxFuture<'_, ApiResult<Vec<MemberState>>> {
        Box::pin(async move {
            // Pages of up to 1000, ordered by user id; `after` is the last id.
            // Without the privileged Server Members intent this is a 403.
            let mut out: Vec<MemberState> = Vec::new();
            loop {
                let after = out.last().map_or("0", |m| m.user_id.as_str());
                let path = format!("/guilds/{}/members?limit=1000&after={after}", self.g());
                let v = self.call(Method::GET, &path, None).await.map_err(|e| {
                    if e.contains("50001") {
                        format!("{e}\n  Turn on Server Members Intent: Discord Developer Portal → your app → Bot.")
                    } else {
                        e
                    }
                })?;
                let page: Vec<MemberState> = v.as_array().into_iter().flatten().map(parse_member).collect();
                let full = page.len() == 1000;
                out.extend(page);
                if !full {
                    return Ok(out);
                }
            }
        })
    }

    fn add_member_role<'a>(&'a self, user_id: &'a str, role_id: &'a str) -> BoxFuture<'a, ApiResult<()>> {
        Box::pin(async move {
            let path = format!("/guilds/{}/members/{user_id}/roles/{role_id}", self.g());
            self.call(Method::PUT, &path, None).await.map(drop)
        })
    }

    fn remove_member_role<'a>(&'a self, user_id: &'a str, role_id: &'a str) -> BoxFuture<'a, ApiResult<()>> {
        Box::pin(async move {
            let path = format!("/guilds/{}/members/{user_id}/roles/{role_id}", self.g());
            self.call(Method::DELETE, &path, None).await.map(drop)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What we send must read back equal, or every run would re-edit the
    /// post forever. Discord echoes the message with extra fields.
    #[test]
    fn posted_embed_reads_back_equal() {
        let body = MessageBody {
            content: String::new(),
            embed: Some(Embed { title: "Welcome".into(), description: "Hi <#1>".into(), color: 0xE2963B }),
        };
        let mut echoed = message_json(&body);
        echoed["id"] = json!("9");
        echoed["author"] = json!({ "id": "900", "bot": true });
        echoed["embeds"][0]["type"] = json!("rich");
        assert_eq!(parse_message(&echoed).body, body);
        let plain = MessageBody { content: "hello".into(), embed: None };
        assert_eq!(parse_message(&message_json(&plain)).body, plain);
    }
}

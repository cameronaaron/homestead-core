//! Discord: outbound notifications via a channel webhook, and inbound slash
//! commands via the Interactions endpoint.

pub mod feed;
pub mod provision;

use std::collections::{HashMap, HashSet};

use ed25519_dalek::{Signature, VerifyingKey};
use homestead_bus::{BoxFuture, DeliveryError, Sink};
use homestead_domain::Event;
use homestead_domain::animals::Species;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{classify, transport};

/// Discord rejects message content over 2000 characters.
const MAX_CONTENT: usize = 2000;

/// Posts every event's summary to one channel webhook.
pub struct WebhookSink {
    client: reqwest::Client,
    url: String,
    mentions: Option<std::sync::Arc<dyn MentionSource>>,
    /// Linear projects whose deadline reminders a [`ChannelSink`] posts
    /// instead, so they aren't said twice.
    elsewhere: HashSet<String>,
}

impl WebhookSink {
    pub fn new(client: reqwest::Client, url: String) -> Self {
        Self { client, url, mentions: None, elsewhere: HashSet::new() }
    }

    pub fn with_mentions(mut self, mentions: std::sync::Arc<dyn MentionSource>) -> Self {
        self.mentions = Some(mentions);
        self
    }

    pub fn except_projects(mut self, projects: impl IntoIterator<Item = String>) -> Self {
        self.elsewhere.extend(projects);
        self
    }
}

/// Message body with mention parsing disabled. `users` are Discord
/// snowflakes the caller already decided may be pinged; record text is
/// never scanned for mentions.
pub fn message_to(content: &str, users: &[String]) -> Value {
    let users: Vec<&str> = users
        .iter()
        .map(String::as_str)
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        .collect();
    let suffix = users.iter().map(|id| format!("<@{id}>")).collect::<Vec<_>>().join(" ");
    let room = MAX_CONTENT.saturating_sub(if suffix.is_empty() { 0 } else { suffix.chars().count() + 1 });
    let mut body: String = if content.chars().count() > room {
        content.chars().take(room.saturating_sub(1)).chain(std::iter::once('…')).collect()
    } else {
        content.to_owned()
    };
    if !suffix.is_empty() {
        if !body.is_empty() {
            body.push(' ');
        }
        body.push_str(&suffix);
    }
    json!({ "content": body, "allowed_mentions": { "parse": [], "users": users } })
}

/// A message that pings nobody.
pub fn message(content: &str) -> Value {
    message_to(content, &[])
}

/// Who to ping when a farm event is posted. Looked up at delivery time so
/// a preference change applies to the next attempt, including a retry.
pub trait MentionSource: Send + Sync {
    fn user_ids<'a>(&'a self, kind: &'a str) -> homestead_bus::BoxFuture<'a, Vec<String>>;
}

fn emoji(e: &Event) -> &'static str {
    match e {
        Event::TaskCreated { .. } => "📝",
        Event::TaskCompleted { .. } => "✅",
        Event::PlantingCreated { .. } => "🌱",
        Event::HarvestLogged { .. } => "🧺",
        Event::LowStock { .. } => "⚠️",
        Event::ReleaseSigned { .. } => "✍️",
        Event::ReleaseDeclined { .. } => "📄",
        Event::MemberOnboarded { .. } => "👋",
        Event::AnimalAdopted { species: Species::Cat, .. } => "🐈",
        Event::AnimalAdopted { .. } => "🏡",
        Event::AdoptionApplied { .. } => "📝",
        Event::IncidentReported { .. } => "⚠️",
        Event::TaskDeadlines { .. } => "⏰",
        Event::DailyDigest { .. } => "☀️",
    }
}

impl Sink for WebhookSink {
    fn name(&self) -> &'static str {
        "discord"
    }

    fn accepts(&self, event: &Event) -> bool {
        !matches!(event, Event::TaskDeadlines { project, .. } if self.elsewhere.contains(project))
    }

    fn deliver<'a>(&'a self, _event_id: i64, event: &'a Event) -> BoxFuture<'a, Result<(), DeliveryError>> {
        Box::pin(async move {
            let users = match &self.mentions {
                Some(source) => source.user_ids(event.kind()).await,
                None => Vec::new(),
            };
            let body = message_to(&format!("{} {}", emoji(event), event.summary()), &users);
            let resp =
                self.client.post(&self.url).query(&[("wait", "true")]).json(&body).send().await.map_err(transport)?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            classify(status, &text)
        })
    }
}

/// Posts deadline reminders for chosen Linear projects to their own
/// channel, as the bot (grant deadlines to #grant-deadlines). A webhook
/// posts to one channel only; the bot can post anywhere it can see.
pub struct ChannelSink {
    client: reqwest::Client,
    base: String,
    token: String,
    guild_id: String,
    /// Linear project name → channel name.
    routes: HashMap<String, String>,
    /// Channel name → id, learned on first use.
    ids: std::sync::Mutex<HashMap<String, String>>,
}

/// Refuses deadline channels that aren't in the spec, or that the whole
/// community can read: reminders carry internal task titles, which
/// otherwise go only to crew and leads (#farm-feed).
pub fn check_deadline_channels<'a>(
    spec: &provision::Spec,
    names: impl IntoIterator<Item = &'a String>,
) -> Result<(), String> {
    let mut bad = Vec::new();
    for name in names {
        match spec.channels().find(|(_, ch)| &ch.name == name) {
            None => bad.push(format!("#{name} is not in the Discord spec")),
            Some((cat, ch)) if ch.access(cat).can_read(feed::BASELINE_ROLE) => {
                bad.push(format!("#{name} is readable by {}; deadlines are for the team", feed::BASELINE_ROLE))
            }
            Some(_) => {}
        }
    }
    if bad.is_empty() { Ok(()) } else { Err(bad.join("; ")) }
}

impl ChannelSink {
    pub fn new(
        client: reqwest::Client,
        base: Option<String>,
        token: String,
        guild_id: String,
        routes: HashMap<String, String>,
    ) -> Self {
        let base = base.unwrap_or_else(|| feed::API.to_owned());
        Self { client, base, token, guild_id, routes, ids: Default::default() }
    }

    fn cached(&self, name: &str) -> Option<String> {
        self.ids.lock().expect("channel cache").get(name).cloned()
    }

    async fn channel_id(&self, name: &str) -> Result<String, DeliveryError> {
        if let Some(id) = self.cached(name) {
            return Ok(id);
        }
        let resp = self
            .client
            .get(format!("{}/guilds/{}/channels", self.base, self.guild_id))
            .header("Authorization", format!("Bot {}", self.token))
            .send()
            .await
            .map_err(transport)?;
        let status = resp.status();
        let text = resp.text().await.map_err(transport)?;
        classify(status, &text)?;
        let channels: Vec<Value> = serde_json::from_str(&text)
            .map_err(|e| DeliveryError::Retryable(format!("reading the channel list: {e}")))?;
        // Text channels only (type 0): a voice channel can share the name.
        let id = channels
            .iter()
            .find(|c| c["name"] == name && c["type"] == 0)
            .and_then(|c| c["id"].as_str())
            .ok_or_else(|| DeliveryError::Permanent(format!("no #{name} channel; run just discord-provision")))?
            .to_owned();
        self.ids.lock().expect("channel cache").insert(name.to_owned(), id.clone());
        Ok(id)
    }
}

impl Sink for ChannelSink {
    fn name(&self) -> &'static str {
        "discord-channels"
    }

    fn accepts(&self, event: &Event) -> bool {
        matches!(event, Event::TaskDeadlines { project, .. } if self.routes.contains_key(project))
    }

    fn deliver<'a>(&'a self, _event_id: i64, event: &'a Event) -> BoxFuture<'a, Result<(), DeliveryError>> {
        Box::pin(async move {
            let Event::TaskDeadlines { project, .. } = event else {
                return Err(DeliveryError::Permanent(format!("{} isn't posted to a channel", event.kind())));
            };
            let name = self
                .routes
                .get(project)
                .ok_or_else(|| DeliveryError::Permanent(format!("no channel for the Linear project {project:?}")))?;
            let id = self.channel_id(name).await?;
            let resp = self
                .client
                .post(format!("{}/channels/{id}/messages", self.base))
                .header("Authorization", format!("Bot {}", self.token))
                .json(&message(&format!("{} {}", emoji(event), event.summary())))
                .send()
                .await
                .map_err(transport)?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status == reqwest::StatusCode::NOT_FOUND {
                // The channel was deleted or remade. Retry with a fresh
                // lookup, which fails for good if it's really gone.
                self.ids.lock().expect("channel cache").remove(name);
                return Err(DeliveryError::Retryable(format!("#{name} not found; looking it up again")));
            }
            classify(status, &text)
        })
    }
}

/// Gives someone who signed the welcome agreement the role they asked for
/// (`member.onboarded`). Granting is idempotent, so a retry is harmless.
pub struct RoleSink {
    client: reqwest::Client,
    base: String,
    token: String,
    guild_id: String,
    /// Role name → id, looked up once.
    ids: std::sync::Mutex<HashMap<String, String>>,
}

impl RoleSink {
    pub fn new(client: reqwest::Client, base: Option<String>, token: String, guild_id: String) -> Self {
        let base = base.unwrap_or_else(|| feed::API.to_owned());
        Self { client, base, token, guild_id, ids: Default::default() }
    }

    async fn role_id(&self, name: &str) -> Result<String, DeliveryError> {
        if let Some(id) = self.ids.lock().expect("role cache").get(name).cloned() {
            return Ok(id);
        }
        let resp = self
            .client
            .get(format!("{}/guilds/{}/roles", self.base, self.guild_id))
            .header("Authorization", format!("Bot {}", self.token))
            .send()
            .await
            .map_err(transport)?;
        let status = resp.status();
        let text = resp.text().await.map_err(transport)?;
        classify(status, &text)?;
        let roles: Vec<Value> =
            serde_json::from_str(&text).map_err(|e| DeliveryError::Retryable(format!("reading the role list: {e}")))?;
        let id = roles
            .iter()
            .find(|r| r["name"] == name)
            .and_then(|r| r["id"].as_str())
            .ok_or_else(|| DeliveryError::Permanent(format!("no {name:?} role; run just discord-provision")))?
            .to_owned();
        self.ids.lock().expect("role cache").insert(name.to_owned(), id.clone());
        Ok(id)
    }
}

impl Sink for RoleSink {
    fn name(&self) -> &'static str {
        "discord-roles"
    }

    fn accepts(&self, event: &Event) -> bool {
        matches!(event, Event::MemberOnboarded { .. })
    }

    fn deliver<'a>(&'a self, _event_id: i64, event: &'a Event) -> BoxFuture<'a, Result<(), DeliveryError>> {
        Box::pin(async move {
            let Event::MemberOnboarded { discord_user_id, role, .. } = event else {
                return Err(DeliveryError::Permanent(format!("{} grants no role", event.kind())));
            };
            if discord_user_id.is_empty() || !discord_user_id.bytes().all(|b| b.is_ascii_digit()) {
                return Err(DeliveryError::Permanent(format!("{discord_user_id:?} is not a Discord user id")));
            }
            let role_id = self.role_id(role.as_str()).await?;
            let resp = self
                .client
                .put(format!("{}/guilds/{}/members/{discord_user_id}/roles/{role_id}", self.base, self.guild_id))
                .header("Authorization", format!("Bot {}", self.token))
                .header("X-Audit-Log-Reason", "Signed the welcome agreement")
                .send()
                .await
                .map_err(transport)?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status == reqwest::StatusCode::NOT_FOUND && text.contains("10011") {
                // Unknown Role: it was deleted or remade. Look it up again.
                self.ids.lock().expect("role cache").remove(role.as_str());
                return Err(DeliveryError::Retryable(format!("the {role} role changed; looking it up again")));
            }
            if status == reqwest::StatusCode::NOT_FOUND {
                return Err(DeliveryError::Permanent(format!(
                    "{discord_user_id} is no longer in the server; they'll need to /onboard again after rejoining"
                )));
            }
            classify(status, &text)
        })
    }
}

// ── Inbound interactions ─────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("missing or malformed signature headers")]
    Malformed,
    #[error("signature does not match")]
    BadSignature,
    #[error("request timestamp outside the accepted window")]
    Stale,
}

/// Verifies Discord's Ed25519 request signature (over `timestamp || body`).
/// Discord probes endpoints with deliberately bad signatures and disables
/// any endpoint that accepts them, so this must run before anything else.
pub struct InteractionVerifier {
    key: VerifyingKey,
    /// Rejects replays older/newer than this many seconds.
    max_skew_secs: i64,
}

impl InteractionVerifier {
    pub fn from_hex(public_key_hex: &str) -> Result<Self, String> {
        let bytes: [u8; 32] = hex::decode(public_key_hex.trim())
            .map_err(|e| format!("public key is not hex: {e}"))?
            .try_into()
            .map_err(|_| "public key must be 32 bytes".to_owned())?;
        let key = VerifyingKey::from_bytes(&bytes).map_err(|e| format!("invalid Ed25519 key: {e}"))?;
        Ok(Self { key, max_skew_secs: 300 })
    }

    pub fn verify(&self, signature_hex: &str, timestamp: &str, body: &[u8], now_unix: i64) -> Result<(), VerifyError> {
        let sig: [u8; 64] = hex::decode(signature_hex)
            .map_err(|_| VerifyError::Malformed)?
            .try_into()
            .map_err(|_| VerifyError::Malformed)?;
        let sig = Signature::from_bytes(&sig);
        let mut msg = Vec::with_capacity(timestamp.len() + body.len());
        msg.extend_from_slice(timestamp.as_bytes());
        msg.extend_from_slice(body);
        self.key.verify_strict(&msg, &sig).map_err(|_| VerifyError::BadSignature)?;
        let ts: i64 = timestamp.parse().map_err(|_| VerifyError::Malformed)?;
        if (now_unix - ts).abs() > self.max_skew_secs {
            return Err(VerifyError::Stale);
        }
        Ok(())
    }
}

/// The subset of an Interaction payload the server acts on.
#[derive(Debug, Deserialize)]
pub struct Interaction {
    #[serde(rename = "type")]
    pub kind: u8,
    /// With `token`, where a deferred reply is edited (for 15 minutes).
    #[serde(default)]
    pub application_id: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub data: Option<CommandData>,
    /// Who ran the command, in a server.
    #[serde(default)]
    pub member: Option<Member>,
    /// Who ran the command, in a DM.
    #[serde(default)]
    pub user: Option<User>,
}

#[derive(Debug, Deserialize)]
pub struct Member {
    pub user: User,
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
}

impl Interaction {
    /// The Discord user behind the interaction, if Discord said.
    pub fn invoker(&self) -> Option<&User> {
        self.member.as_ref().map(|m| &m.user).or(self.user.as_ref())
    }
}

#[derive(Debug, Deserialize)]
pub struct CommandData {
    pub name: String,
    #[serde(default)]
    pub options: Vec<CommandOption>,
}

#[derive(Debug, Deserialize)]
pub struct CommandOption {
    pub name: String,
    pub value: Value,
}

impl CommandData {
    pub fn str_option(&self, name: &str) -> Option<&str> {
        self.options.iter().find(|o| o.name == name).and_then(|o| o.value.as_str())
    }
}

pub const PING: u8 = 1;
pub const APPLICATION_COMMAND: u8 = 2;

pub fn pong() -> Value {
    json!({ "type": 1 })
}

/// `CHANNEL_MESSAGE_WITH_SOURCE`, visible only to the invoking user when
/// `ephemeral` (flag 64).
pub fn reply(content: &str, ephemeral: bool) -> Value {
    let mut data = message(content);
    if ephemeral {
        data["flags"] = json!(64);
    }
    json!({ "type": 4, "data": data })
}

/// `DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE`: "thinking…", for work that may
/// outlast Discord's three-second window. Finish it with [`edit_reply`].
pub fn deferred(ephemeral: bool) -> Value {
    if ephemeral { json!({ "type": 5, "data": { "flags": 64 } }) } else { json!({ "type": 5 }) }
}

/// Replaces a deferred reply with `content`. Needs no bot token: the
/// interaction token is the credential.
pub async fn edit_reply(
    client: &reqwest::Client,
    base: &str,
    application_id: &str,
    token: &str,
    content: &str,
) -> Result<(), String> {
    let resp = client
        .patch(format!("{base}/webhooks/{application_id}/{token}/messages/@original"))
        .json(&message(content))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if status.is_success() { Ok(()) } else { Err(format!("HTTP {status}: {}", resp.text().await.unwrap_or_default())) }
}

/// The slash commands this server answers. Registered with
/// `homestead discord-register`; keep in sync with the server's handler
/// (the server's tests assert every name here is handled).
pub fn command_definitions() -> Value {
    json!([
        { "name": "tasks", "description": "List open farm tasks", "type": 1 },
        { "name": "lowstock", "description": "List inventory at or below its reorder level", "type": 1 },
        {
            "name": "task",
            "description": "Add a farm task",
            "type": 1,
            "options": [
                { "type": 3, "name": "title", "description": "What needs doing", "required": true, "max_length": 200 },
                { "type": 3, "name": "priority", "description": "How urgent", "required": false,
                  "choices": [
                    { "name": "low", "value": "low" }, { "name": "normal", "value": "normal" },
                    { "name": "high", "value": "high" }, { "name": "urgent", "value": "urgent" }
                  ] }
            ]
        },
        {
            "name": "onboard",
            "description": "Sign the welcome agreement to get your role and open the channels",
            "type": 1,
            "options": [
                { "type": 3, "name": "name", "description": "Your full legal name, as you'll sign it",
                  "required": true, "max_length": 100 },
                { "type": 3, "name": "email", "description": "Where your signed copy goes (never shown in Discord)",
                  "required": true, "max_length": 200 },
                { "type": 3, "name": "joining_as", "description": "How you're joining (default: volunteer)",
                  "required": false,
                  "choices": [
                    { "name": "Volunteer: work days and shifts", "value": "Volunteer" },
                    { "name": "Community: neighbour, member or supporter", "value": "Community" }
                  ] }
            ]
        }
    ])
}

/// Bulk-overwrites the application's global commands (idempotent).
pub async fn register_commands(client: &reqwest::Client, application_id: &str, bot_token: &str) -> Result<(), String> {
    let url = format!("https://discord.com/api/v10/applications/{application_id}/commands");
    let resp = client
        .put(url)
        .header("Authorization", format!("Bot {bot_token}"))
        .json(&command_definitions())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if status.is_success() { Ok(()) } else { Err(format!("HTTP {status}: {}", resp.text().await.unwrap_or_default())) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn keypair() -> (SigningKey, InteractionVerifier) {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let v = InteractionVerifier::from_hex(&hex::encode(sk.verifying_key().as_bytes())).expect("key");
        (sk, v)
    }

    #[test]
    fn accepts_genuine_and_rejects_tampered_requests() {
        let (sk, v) = keypair();
        let (ts, body) = ("1790000000", br#"{"type":1}"#.as_slice());
        let sig = hex::encode(sk.sign(&[ts.as_bytes(), body].concat()).to_bytes());
        assert_eq!(v.verify(&sig, ts, body, 1_790_000_010), Ok(()));
        assert_eq!(v.verify(&sig, ts, br#"{"type":2}"#, 1_790_000_010), Err(VerifyError::BadSignature));
        assert_eq!(v.verify(&sig, "1790000001", body, 1_790_000_010), Err(VerifyError::BadSignature));
        assert_eq!(v.verify("zz", ts, body, 1_790_000_010), Err(VerifyError::Malformed));
        assert_eq!(v.verify(&sig[..64], ts, body, 1_790_000_010), Err(VerifyError::Malformed));
    }

    #[test]
    fn rejects_replayed_old_requests() {
        let (sk, v) = keypair();
        let (ts, body) = ("1790000000", b"{}".as_slice());
        let sig = hex::encode(sk.sign(&[ts.as_bytes(), body].concat()).to_bytes());
        assert_eq!(v.verify(&sig, ts, body, 1_790_000_000 + 301), Err(VerifyError::Stale));
    }

    #[test]
    fn messages_never_mention_and_fit_discords_limit() {
        let m = message(&"@everyone ".repeat(500));
        assert_eq!(m["allowed_mentions"]["parse"], json!([]));
        assert_eq!(m["allowed_mentions"]["users"], json!([]));
        assert_eq!(m["content"].as_str().expect("s").chars().count(), MAX_CONTENT);
        assert_eq!(message("hi")["content"], "hi");
        let ping = message_to("Low stock: lime", &["123".into(), "nope".into(), "456".into()]);
        assert_eq!(ping["content"], "Low stock: lime <@123> <@456>");
        assert_eq!(ping["allowed_mentions"]["parse"], json!([]));
        assert_eq!(ping["allowed_mentions"]["users"], json!(["123", "456"]));
    }

    #[test]
    fn rejects_malformed_public_keys() {
        assert!(InteractionVerifier::from_hex("abcd").is_err());
        assert!(InteractionVerifier::from_hex("not hex").is_err());
    }
}

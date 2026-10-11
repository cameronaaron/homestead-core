//! Declarative Discord server provisioning: make a guild match
//! `discord/server.toml`.
//!
//! [`plan`] is a pure diff of spec against live state. [`apply`] repeatedly
//! fetches state, plans, and executes until the plan is empty, which is how
//! forward references (a topic linking a channel created later in the same
//! pass) settle. Matching is by name. Nothing absent from the spec is ever
//! deleted; it is reported as unmanaged.
//!
//! Listed members (`[members]`) are matched by exact username. Their spec
//! roles are made to match the list; anyone listed who hasn't joined yet is
//! reported as pending, since a bot can't add people to a server. Unlisted
//! people are reported as unmanaged, with the spec roles they hold.
//!
//! Managed channels are fully owned: their permission overwrites are reset
//! to exactly what the spec implies, including removing ad-hoc per-member
//! overwrites someone added by hand.

pub mod http;
pub mod spec;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use homestead_bus::BoxFuture;

use serde_json::Value;
pub use spec::Spec;
use spec::{CATEGORY_TYPE, Category, Channel, EVERYONE, Kind, POST, Settings, VIEW};

// ── Live state (the subset of Discord objects we manage) ─────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuildInfo {
    pub id: String,
    pub name: String,
    pub verification_level: u8,
    pub default_message_notifications: u8,
    pub explicit_content_filter: u8,
    pub system_channel_id: Option<String>,
    /// Icon hash; `None` when the server has no icon.
    pub icon: Option<String>,
    /// Every field Discord reported, for `[server.settings]`.
    pub fields: Settings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotUser {
    pub id: String,
    pub avatar: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleState {
    pub id: String,
    pub name: String,
    pub permissions: u64,
    pub color: u32,
    pub hoist: bool,
    pub mentionable: bool,
    pub position: i64,
    /// Owned by an integration (e.g. the bot's own role): never touched.
    pub managed: bool,
    /// For a bot's own managed role, that bot's user id.
    pub bot_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Overwrite {
    pub id: String,
    /// 0 = role, 1 = member.
    pub kind: u8,
    pub allow: u64,
    pub deny: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelState {
    pub id: String,
    pub name: String,
    pub kind: u8,
    pub parent_id: Option<String>,
    pub topic: Option<String>,
    pub position: i64,
    pub overwrites: Vec<Overwrite>,
    /// Every field Discord reported, for a channel's `settings`.
    pub fields: Settings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageState {
    pub id: String,
    pub author_id: String,
    /// Discord message type. Managed bot posts are type 0; pin notices are
    /// type 6 and cannot be edited even when Discord attributes them to us.
    pub kind: u8,
    pub body: MessageBody,
    pub pinned: bool,
}

/// What a bot post shows: plain text, or an embed card.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MessageBody {
    pub content: String,
    pub embed: Option<Embed>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Embed {
    pub title: String,
    pub description: String,
    pub color: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookState {
    pub id: String,
    pub name: String,
    pub token: Option<String>,
}

impl WebhookState {
    pub fn url(&self) -> Option<String> {
        self.token.as_ref().map(|t| format!("https://discord.com/api/webhooks/{}/{t}", self.id))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberState {
    pub user_id: String,
    pub username: String,
    /// Role ids (Discord never lists @everyone here).
    pub roles: Vec<String>,
    pub bot: bool,
}

#[derive(Debug, Clone)]
pub struct GuildState {
    pub guild: GuildInfo,
    pub bot: BotUser,
    pub roles: Vec<RoleState>,
    pub channels: Vec<ChannelState>,
    pub messages: BTreeMap<String, Vec<MessageState>>,
    pub webhooks: BTreeMap<String, Vec<WebhookState>>,
    /// Everyone in the server, bots included.
    pub members: Vec<MemberState>,
}

// ── Request bodies ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleBody {
    pub name: String,
    pub permissions: u64,
    pub color: u32,
    pub hoist: bool,
    pub mentionable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelBody {
    pub name: String,
    pub kind: u8,
    pub parent_id: Option<String>,
    /// Only sent for text channels (categories and voice have none).
    pub topic: Option<String>,
    pub overwrites: Vec<Overwrite>,
    /// Extra Modify Channel fields, channel names already resolved to ids.
    pub settings: Settings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuildBody {
    pub name: String,
    pub verification_level: u8,
    pub default_message_notifications: u8,
    pub explicit_content_filter: u8,
    pub system_channel_id: Option<String>,
    /// Extra Modify Guild fields, channel names already resolved to ids.
    pub settings: Settings,
}

pub type ApiResult<T> = Result<T, String>;

/// One guild's worth of the Discord REST API. Implemented over HTTP by
/// [`http::DiscordHttp`] and in memory by the tests.
pub trait Api: Send + Sync {
    fn bot_user(&self) -> BoxFuture<'_, ApiResult<BotUser>>;
    fn guild(&self) -> BoxFuture<'_, ApiResult<GuildInfo>>;
    fn roles(&self) -> BoxFuture<'_, ApiResult<Vec<RoleState>>>;
    fn channels(&self) -> BoxFuture<'_, ApiResult<Vec<ChannelState>>>;
    fn messages<'a>(&'a self, channel_id: &'a str) -> BoxFuture<'a, ApiResult<Vec<MessageState>>>;
    fn webhooks<'a>(&'a self, channel_id: &'a str) -> BoxFuture<'a, ApiResult<Vec<WebhookState>>>;
    fn create_role<'a>(&'a self, body: &'a RoleBody) -> BoxFuture<'a, ApiResult<RoleState>>;
    fn modify_role<'a>(&'a self, id: &'a str, body: &'a RoleBody) -> BoxFuture<'a, ApiResult<()>>;
    fn set_everyone_permissions(&self, permissions: u64) -> BoxFuture<'_, ApiResult<()>>;
    fn reorder_roles<'a>(&'a self, positions: &'a [(String, i64)]) -> BoxFuture<'a, ApiResult<()>>;
    fn create_channel<'a>(&'a self, body: &'a ChannelBody) -> BoxFuture<'a, ApiResult<ChannelState>>;
    fn modify_channel<'a>(&'a self, id: &'a str, body: &'a ChannelBody) -> BoxFuture<'a, ApiResult<()>>;
    fn reorder_channels<'a>(&'a self, positions: &'a [(String, i64)]) -> BoxFuture<'a, ApiResult<()>>;
    fn delete_channel<'a>(&'a self, id: &'a str) -> BoxFuture<'a, ApiResult<()>>;
    fn modify_guild<'a>(&'a self, body: &'a GuildBody) -> BoxFuture<'a, ApiResult<()>>;
    fn create_webhook<'a>(&'a self, channel_id: &'a str, name: &'a str) -> BoxFuture<'a, ApiResult<WebhookState>>;
    fn create_message<'a>(
        &'a self,
        channel_id: &'a str,
        body: &'a MessageBody,
    ) -> BoxFuture<'a, ApiResult<MessageState>>;
    fn edit_message<'a>(
        &'a self,
        channel_id: &'a str,
        id: &'a str,
        body: &'a MessageBody,
    ) -> BoxFuture<'a, ApiResult<()>>;
    fn set_message_pinned<'a>(
        &'a self,
        channel_id: &'a str,
        message_id: &'a str,
        pinned: bool,
    ) -> BoxFuture<'a, ApiResult<()>>;
    /// `image` is a data URI (`data:image/png;base64,…`).
    fn set_guild_icon<'a>(&'a self, image: &'a str) -> BoxFuture<'a, ApiResult<()>>;
    fn set_bot_avatar<'a>(&'a self, image: &'a str) -> BoxFuture<'a, ApiResult<()>>;
    /// Everyone in the server. Needs the bot's Server Members intent.
    fn members(&self) -> BoxFuture<'_, ApiResult<Vec<MemberState>>>;
    fn add_member_role<'a>(&'a self, user_id: &'a str, role_id: &'a str) -> BoxFuture<'a, ApiResult<()>>;
    fn remove_member_role<'a>(&'a self, user_id: &'a str, role_id: &'a str) -> BoxFuture<'a, ApiResult<()>>;
}

pub async fn fetch(api: &dyn Api, spec: &Spec) -> ApiResult<GuildState> {
    let guild = api.guild().await?;
    let bot = api.bot_user().await?;
    let roles = api.roles().await?;
    let channels = api.channels().await?;
    let mut messages = BTreeMap::new();
    let mut webhooks = BTreeMap::new();
    for (_, ch) in spec.channels() {
        let Some(live) = find_channel(&channels, ch.kind.discord_type(), &ch.name) else { continue };
        if ch.message.is_some() {
            messages.insert(live.id.clone(), api.messages(&live.id).await?);
        }
        if ch.webhook.is_some() {
            webhooks.insert(live.id.clone(), api.webhooks(&live.id).await?);
        }
    }
    let members = api.members().await?;
    Ok(GuildState { guild, bot, roles, channels, messages, webhooks, members })
}

fn find_channel<'a>(channels: &'a [ChannelState], kind: u8, name: &str) -> Option<&'a ChannelState> {
    channels.iter().find(|c| c.kind == kind && c.name == name)
}

// ── The plan ──────────────────────────────────────────────────────────────────

/// Overwrite target by spec name, resolved to an id at execution time.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    Everyone,
    Role(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredChannel {
    pub name: String,
    pub kind: u8,
    pub parent: Option<String>,
    /// Text channels only; may contain `<#name>` links.
    pub topic: Option<String>,
    pub overwrites: Vec<(Target, u64, u64)>,
    /// May name channels in `*_channel_id` fields.
    pub settings: Settings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    CreateRole(RoleBody),
    UpdateRole {
        id: String,
        body: RoleBody,
        changed: Vec<&'static str>,
    },
    SetEveryonePermissions(u64),
    ReorderRoles(Vec<String>),
    CreateChannel(DesiredChannel),
    UpdateChannel {
        id: String,
        want: DesiredChannel,
        changed: Vec<String>,
    },
    ReorderChannels,
    UpdateServer {
        changed: Vec<String>,
    },
    CreateWebhook {
        channel: String,
        name: String,
    },
    DeleteChannel {
        id: String,
        name: String,
    },
    PostMessage {
        channel: String,
    },
    EditMessage {
        channel: String,
        message_id: String,
    },
    SetMessagePinned {
        channel: String,
        message_id: String,
        pinned: bool,
    },
    SetServerIcon,
    SetBotAvatar,
    /// The role is named, not an id: it may be created earlier in the pass.
    AddMemberRole {
        username: String,
        user_id: String,
        role: String,
    },
    RemoveMemberRole {
        username: String,
        user_id: String,
        role: String,
    },
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = |k: u8| match k {
            CATEGORY_TYPE => "category",
            2 => "voice channel",
            _ => "channel",
        };
        match self {
            Action::CreateRole(r) => write!(f, "+ role {:?}", r.name),
            Action::UpdateRole { body, changed, .. } => write!(f, "~ role {:?} ({})", body.name, changed.join(", ")),
            Action::SetEveryonePermissions(_) => write!(f, "~ @everyone base permissions"),
            Action::ReorderRoles(order) => write!(f, "~ role order → {}", order.join(" > ")),
            Action::CreateChannel(c) => write!(f, "+ {} \"{}\"", kind(c.kind), c.name),
            Action::UpdateChannel { want, changed, .. } => {
                write!(f, "~ {} \"{}\" ({})", kind(want.kind), want.name, changed.join(", "))
            }
            Action::ReorderChannels => write!(f, "~ channel order"),
            Action::UpdateServer { changed } => write!(f, "~ server settings ({})", changed.join(", ")),
            Action::CreateWebhook { channel, name } => write!(f, "+ webhook {name:?} in #{channel}"),
            Action::DeleteChannel { name, .. } => write!(f, "- starter channel \"{name}\""),
            Action::PostMessage { channel } => write!(f, "+ bot message in #{channel}"),
            Action::EditMessage { channel, .. } => write!(f, "~ bot message in #{channel}"),
            Action::SetMessagePinned { channel, pinned: true, .. } => write!(f, "+ pin bot message in #{channel}"),
            Action::SetMessagePinned { channel, pinned: false, .. } => write!(f, "- unpin bot message in #{channel}"),
            Action::SetServerIcon => write!(f, "+ server icon"),
            Action::SetBotAvatar => write!(f, "+ bot avatar"),
            Action::AddMemberRole { username, role, .. } => write!(f, "+ @{username} gets role {role:?}"),
            Action::RemoveMemberRole { username, role, .. } => write!(f, "- @{username} loses role {role:?}"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub actions: Vec<Action>,
    /// Roles/channels present in Discord but not in the spec (left alone).
    pub unmanaged: Vec<String>,
    /// Listed members who haven't joined the server yet.
    pub pending: Vec<String>,
}

/// Name → id lookups over live state, extended as things are created.
#[derive(Debug, Clone, Default)]
struct Ids {
    everyone: String,
    roles: BTreeMap<String, String>,
    channels: BTreeMap<(u8, String), String>,
}

impl Ids {
    fn from(state: &GuildState) -> Self {
        let mut roles = BTreeMap::new();
        // First role by position wins if names are duplicated.
        let mut sorted: Vec<_> = state.roles.iter().filter(|r| r.id != state.guild.id && !r.managed).collect();
        sorted.sort_by_key(|r| std::cmp::Reverse(r.position));
        for r in sorted {
            roles.entry(r.name.clone()).or_insert_with(|| r.id.clone());
        }
        let mut channels = BTreeMap::new();
        for c in &state.channels {
            channels.entry((c.kind, c.name.clone())).or_insert_with(|| c.id.clone());
        }
        Self { everyone: state.guild.id.clone(), roles, channels }
    }

    fn text(&self, name: &str) -> Option<&String> {
        self.channels.get(&(0, name.to_owned()))
    }

    fn overwrites(&self, want: &[(Target, u64, u64)]) -> Option<Vec<Overwrite>> {
        let mut out: Vec<Overwrite> = want
            .iter()
            .map(|(t, allow, deny)| {
                let id = match t {
                    Target::Everyone => Some(self.everyone.clone()),
                    Target::Role(n) => self.roles.get(n).cloned(),
                };
                id.map(|id| Overwrite { id, kind: 0, allow: *allow, deny: *deny })
            })
            .collect::<Option<_>>()?;
        out.sort();
        Some(out)
    }

    /// `<#name>` → `<#id>`; unknown names degrade to plain `#name`.
    fn render(&self, template: &str) -> String {
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(start) = rest.find("<#") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            match after.find('>') {
                Some(end) if !after[..end].chars().all(|c| c.is_ascii_digit()) => {
                    let name = &after[..end];
                    match self.text(name) {
                        Some(id) => out.push_str(&format!("<#{id}>")),
                        None => out.push_str(&format!("#{name}")),
                    }
                    rest = &after[end + 1..];
                }
                _ => {
                    out.push_str("<#");
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// Settings with `*_channel_id = "name"` resolved to the channel's id.
    fn settings(&self, settings: &Settings) -> Option<Settings> {
        settings
            .iter()
            .map(|(k, v)| {
                let v = match spec::channel_ref(k, v) {
                    Some(name) => {
                        Value::String(self.channels.iter().find(|((_, n), _)| n == name).map(|(_, id)| id.clone())?)
                    }
                    None => v.clone(),
                };
                Some((k.clone(), v))
            })
            .collect()
    }

    /// Settings keys whose live value differs; all of them while a named
    /// channel doesn't exist yet.
    fn settings_diff(&self, want: &Settings, live: &Settings) -> Vec<String> {
        let resolved = self.settings(want);
        want.keys()
            .filter(|k| {
                let live = live.get(*k).unwrap_or(&Value::Null);
                !resolved.as_ref().and_then(|r| r.get(*k)).is_some_and(|want| crate::settings::covers(want, live))
            })
            .cloned()
            .collect()
    }

    fn body(&self, want: &DesiredChannel) -> Option<ChannelBody> {
        let parent_id = match &want.parent {
            Some(p) => Some(self.channels.get(&(CATEGORY_TYPE, p.clone()))?.clone()),
            None => None,
        };
        Some(ChannelBody {
            name: want.name.clone(),
            kind: want.kind,
            parent_id,
            topic: want.topic.as_deref().map(|t| self.render(t)),
            overwrites: self.overwrites(&want.overwrites)?,
            settings: self.settings(&want.settings)?,
        })
    }
}

/// Overwrites implementing an [`spec::Access`]: hide from non-readers,
/// silence readers who aren't writers.
fn overwrites_for(access: &spec::Access) -> Vec<(Target, u64, u64)> {
    let target = |n: &str| if n == EVERYONE { Target::Everyone } else { Target::Role(n.to_owned()) };
    let restricted_writing = access.writers != access.readers;
    let mut acc: BTreeMap<Target, (u64, u64)> = BTreeMap::new();
    let everyone = acc.entry(Target::Everyone).or_default();
    if !access.readers.contains(EVERYONE) {
        everyone.1 |= VIEW;
    }
    if restricted_writing {
        everyone.1 |= POST;
    }
    for r in access.readers.iter().filter(|r| *r != EVERYONE) {
        acc.entry(target(r)).or_default().0 |= VIEW;
    }
    if restricted_writing {
        for w in &access.writers {
            let e = acc.entry(target(w)).or_default();
            e.0 |= POST;
            e.1 &= !POST;
        }
    }
    acc.into_iter().filter(|(_, (a, d))| *a != 0 || *d != 0).map(|(t, (a, d))| (t, a, d)).collect()
}

fn desired_category(c: &Category) -> DesiredChannel {
    DesiredChannel {
        name: c.name.clone(),
        kind: CATEGORY_TYPE,
        parent: None,
        topic: None,
        overwrites: overwrites_for(&c.access()),
        settings: Settings::new(),
    }
}

fn desired_channel(c: &Category, ch: &Channel) -> DesiredChannel {
    DesiredChannel {
        name: ch.name.clone(),
        kind: ch.kind.discord_type(),
        parent: Some(c.name.clone()),
        topic: (ch.kind == Kind::Text).then(|| ch.topic.trim().to_owned()),
        overwrites: overwrites_for(&ch.access(c)),
        settings: ch.settings.clone(),
    }
}

fn channel_diff(ids: &Ids, live: &ChannelState, want: &DesiredChannel) -> Vec<String> {
    let mut changed = Vec::new();
    let parent_ok = match &want.parent {
        Some(p) => ids.channels.get(&(CATEGORY_TYPE, p.clone())) == live.parent_id.as_ref(),
        None => live.parent_id.is_none(),
    };
    if !parent_ok {
        changed.push("category");
    }
    if let Some(t) = &want.topic
        && ids.render(t) != live.topic.clone().unwrap_or_default()
    {
        changed.push("topic");
    }
    let mut current: Vec<Overwrite> = live.overwrites.iter().filter(|o| o.allow != 0 || o.deny != 0).cloned().collect();
    current.sort();
    if ids.overwrites(&want.overwrites).as_ref() != Some(&current) {
        changed.push("permissions");
    }
    let mut changed: Vec<String> = changed.into_iter().map(str::to_owned).collect();
    changed.extend(ids.settings_diff(&want.settings, &live.fields));
    changed
}

fn role_body(r: &spec::Role) -> RoleBody {
    RoleBody {
        name: r.name.clone(),
        // Validated by `Spec::parse`.
        permissions: Spec::permission_bits(&r.permissions).unwrap_or(0),
        color: Spec::role_color(r).unwrap_or(0),
        hoist: r.hoist,
        mentionable: r.mentionable,
    }
}

fn desired_order(spec: &Spec) -> Vec<(DesiredChannel, Vec<DesiredChannel>)> {
    spec.categories
        .iter()
        .map(|c| (desired_category(c), c.channels.iter().map(|ch| desired_channel(c, ch)).collect()))
        .collect()
}

/// Channel ids in display order: (position, id).
fn ordered<'a>(chs: impl Iterator<Item = &'a ChannelState>) -> Vec<&'a ChannelState> {
    let mut v: Vec<_> = chs.collect();
    v.sort_by(|a, b| (a.position, a.id.len(), &a.id).cmp(&(b.position, b.id.len(), &b.id)));
    v
}

fn channel_order_ok(spec: &Spec, state: &GuildState) -> Option<bool> {
    let ids = Ids::from(state);
    let desired = desired_order(spec);
    let managed_cats: BTreeSet<&str> = desired.iter().map(|(c, _)| c.name.as_str()).collect();
    let live_cats: Vec<&str> = ordered(state.channels.iter().filter(|c| c.kind == CATEGORY_TYPE))
        .into_iter()
        .map(|c| c.name.as_str())
        .filter(|n| managed_cats.contains(n))
        .collect();
    let want_cats: Vec<&str> = desired.iter().map(|(c, _)| c.name.as_str()).collect();
    if live_cats.len() != want_cats.len() {
        return None; // not everything exists yet
    }
    let mut ok = live_cats == want_cats;
    for (cat, children) in &desired {
        let cat_id = ids.channels.get(&(CATEGORY_TYPE, cat.name.clone()))?;
        for kind in [0u8, 2] {
            let want: Vec<&str> = children.iter().filter(|c| c.kind == kind).map(|c| c.name.as_str()).collect();
            let live: Vec<&str> =
                ordered(state.channels.iter().filter(|c| c.kind == kind && c.parent_id.as_ref() == Some(cat_id)))
                    .into_iter()
                    .map(|c| c.name.as_str())
                    .filter(|n| want.contains(n))
                    .collect();
            if live.len() != want.len() {
                return None;
            }
            ok &= live == want;
        }
    }
    Some(ok)
}

/// What Discord puts in a brand-new server.
const STARTER_CATEGORIES: &[&str] = &["Text Channels", "Voice Channels"];
const STARTER_VOICE: &str = "General";

fn find_spec_voice<'a>(spec: &'a Spec, name: &str) -> Option<&'a Channel> {
    spec.channels().map(|(_, c)| c).find(|c| c.kind == Kind::Voice && c.name == name)
}

pub fn plan(spec: &Spec, state: &GuildState) -> Plan {
    let ids = Ids::from(state);
    let mut actions = Vec::new();

    // Roles.
    for r in &spec.roles {
        let want = role_body(r);
        match ids.roles.get(&r.name).and_then(|id| state.roles.iter().find(|l| &l.id == id)) {
            None => actions.push(Action::CreateRole(want)),
            Some(live) => {
                let mut changed = Vec::new();
                if live.permissions != want.permissions {
                    changed.push("permissions");
                }
                if live.color != want.color {
                    changed.push("color");
                }
                if live.hoist != want.hoist {
                    changed.push("hoist");
                }
                if live.mentionable != want.mentionable {
                    changed.push("mentionable");
                }
                if !changed.is_empty() {
                    actions.push(Action::UpdateRole { id: live.id.clone(), body: want, changed });
                }
            }
        }
    }
    let everyone_perms = Spec::permission_bits(&spec.server.everyone_permissions).unwrap_or(0);
    if state.roles.iter().find(|r| r.id == state.guild.id).map(|r| r.permissions) != Some(everyone_perms) {
        actions.push(Action::SetEveryonePermissions(everyone_perms));
    }
    // Ordering is only checked once every spec role exists.
    let ours: Option<Vec<&RoleState>> = spec
        .roles
        .iter()
        .map(|r| ids.roles.get(&r.name).and_then(|id| state.roles.iter().find(|l| &l.id == id)))
        .collect();
    if let Some(mut ours) = ours {
        ours.sort_by_key(|r| std::cmp::Reverse(r.position));
        let distinct: BTreeSet<i64> = ours.iter().map(|r| r.position).collect();
        let live: Vec<&str> = ours.iter().map(|r| r.name.as_str()).collect();
        let want: Vec<&str> = spec.roles.iter().map(|r| r.name.as_str()).collect();
        // Tied positions mean Discord has no defined order between them yet.
        if live != want || distinct.len() != ours.len() {
            actions.push(Action::ReorderRoles(want.into_iter().map(str::to_owned).collect()));
        }
    }

    // Categories, then channels.
    let desired = desired_order(spec);
    let all = desired.iter().map(|(c, _)| c).chain(desired.iter().flat_map(|(_, chs)| chs));
    for want in all {
        match find_channel(&state.channels, want.kind, &want.name) {
            None => actions.push(Action::CreateChannel(want.clone())),
            Some(live) => {
                let changed = channel_diff(&ids, live, want);
                if !changed.is_empty() {
                    actions.push(Action::UpdateChannel { id: live.id.clone(), want: want.clone(), changed });
                }
            }
        }
    }
    if channel_order_ok(spec, state) == Some(false) {
        actions.push(Action::ReorderChannels);
    }

    // Server settings.
    let s = &spec.server;
    let g = &state.guild;
    let mut changed = Vec::new();
    if g.name != s.name {
        changed.push("name");
    }
    if g.verification_level != s.verification_level {
        changed.push("verification level");
    }
    if g.default_message_notifications != s.default_message_notifications {
        changed.push("notifications");
    }
    if g.explicit_content_filter != s.explicit_content_filter {
        changed.push("content filter");
    }
    if s.system_channel.as_ref().map(|n| ids.text(n)) != g.system_channel_id.as_ref().map(Some) {
        changed.push("system channel");
    }
    let mut changed: Vec<String> = changed.into_iter().map(str::to_owned).collect();
    changed.extend(ids.settings_diff(&s.settings, &g.fields));
    if !changed.is_empty() {
        actions.push(Action::UpdateServer { changed });
    }

    // Webhooks and bot messages.
    for (_, ch) in spec.channels() {
        let live_id = ids.channels.get(&(ch.kind.discord_type(), ch.name.clone()));
        if let Some(name) = &ch.webhook {
            let exists = live_id
                .and_then(|id| state.webhooks.get(id))
                .is_some_and(|hooks| hooks.iter().any(|h| &h.name == name));
            if !exists {
                actions.push(Action::CreateWebhook { channel: ch.name.clone(), name: name.clone() });
            }
        }
        if let Some(want) = desired_message(spec, ch, &ids) {
            let mine = live_id
                .and_then(|id| state.messages.get(id))
                .and_then(|ms| ms.iter().find(|m| m.kind == 0 && m.author_id == state.bot.id));
            match mine {
                None => actions.push(Action::PostMessage { channel: ch.name.clone() }),
                Some(m) if m.body != want => {
                    actions.push(Action::EditMessage { channel: ch.name.clone(), message_id: m.id.clone() });
                }
                Some(_) => {}
            }
            if let (Some(m), Some(pinned)) = (mine, ch.pinned)
                && m.pinned != pinned
            {
                actions.push(Action::SetMessagePinned { channel: ch.name.clone(), message_id: m.id.clone(), pinned });
            }
        }
    }

    if spec.server.icon.is_some() && state.guild.icon.is_none() {
        actions.push(Action::SetServerIcon);
    }
    if spec.server.bot_avatar.is_some() && state.bot.avatar.is_none() {
        actions.push(Action::SetBotAvatar);
    }

    // Members. Discord refuses to grant a role that isn't below the bot's,
    // and new roles only get ordered on the pass after they're created, so
    // wait until no role is being created or reordered.
    let roles_settled = !actions.iter().any(|a| matches!(a, Action::CreateRole(_) | Action::ReorderRoles(_)));
    let mut pending = Vec::new();
    for (username, want) in &spec.members {
        let Some(live) = state.members.iter().find(|m| &m.username == username) else {
            pending.push(username.clone());
            continue;
        };
        if !roles_settled {
            continue;
        }
        let holds = |role: &str| ids.roles.get(role).is_some_and(|id| live.roles.contains(id));
        for r in &spec.roles {
            let (username, user_id, role) = (username.clone(), live.user_id.clone(), r.name.clone());
            match (want.contains(&r.name), holds(&r.name)) {
                (true, false) => actions.push(Action::AddMemberRole { username, user_id, role }),
                (false, true) => actions.push(Action::RemoveMemberRole { username, user_id, role }),
                _ => {}
            }
        }
    }

    if spec.server.remove_starter_channels {
        for c in &state.channels {
            let starter_category = c.kind == CATEGORY_TYPE && STARTER_CATEGORIES.contains(&c.name.as_str());
            let empty = !state.channels.iter().any(|x| x.parent_id.as_ref() == Some(&c.id));
            let starter_voice = c.kind == 2 && c.name == STARTER_VOICE && find_spec_voice(spec, &c.name).is_none();
            if (starter_category && empty) || starter_voice {
                actions.push(Action::DeleteChannel { id: c.id.clone(), name: c.name.clone() });
            }
        }
    }

    let spec_roles: BTreeSet<&str> = spec.roles.iter().map(|r| r.name.as_str()).collect();
    let spec_channels: BTreeSet<(u8, &str)> =
        desired.iter().flat_map(|(c, chs)| std::iter::once(c).chain(chs)).map(|c| (c.kind, c.name.as_str())).collect();
    let mut unmanaged: Vec<String> = state
        .roles
        .iter()
        .filter(|r| r.id != g.id && !r.managed && !spec_roles.contains(r.name.as_str()))
        .map(|r| format!("role {:?}", r.name))
        .collect();
    unmanaged.extend(
        state
            .channels
            .iter()
            .filter(|c| !spec_channels.contains(&(c.kind, c.name.as_str())))
            .map(|c| format!("channel {:?}", c.name)),
    );
    let role_names: BTreeMap<&str, &str> = state.roles.iter().map(|r| (r.id.as_str(), r.name.as_str())).collect();
    for m in state.members.iter().filter(|m| !m.bot && !spec.members.contains_key(&m.username)) {
        let held: Vec<&str> = spec
            .roles
            .iter()
            .map(|r| r.name.as_str())
            .filter(|n| m.roles.iter().any(|id| role_names.get(id.as_str()) == Some(n)))
            .collect();
        let held = if held.is_empty() { "no role yet".to_owned() } else { held.join(", ") };
        unmanaged.push(format!("member @{} ({held})", m.username));
    }
    Plan { actions, unmanaged, pending }
}

// ── Apply ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct Report {
    pub executed: Vec<String>,
    pub unmanaged: Vec<String>,
    /// (channel name, webhook URL) for every managed webhook.
    pub webhooks: Vec<(String, String)>,
    /// Listed members who haven't joined the server yet.
    pub pending: Vec<String>,
}

/// Enough passes for every forward reference to settle; if the plan is
/// still non-empty after this, something is fighting us (or a bug).
const MAX_ROUNDS: usize = 4;

/// Image files named in the spec, as data URIs, keyed by spec path.
pub type Images = BTreeMap<String, String>;

/// The post the spec wants in `ch`, with `<#links>` resolved.
fn desired_message(spec: &Spec, ch: &Channel, ids: &Ids) -> Option<MessageBody> {
    let text = ids.render(ch.message.as_deref()?.trim());
    Some(match &ch.title {
        Some(title) => MessageBody {
            content: String::new(),
            embed: Some(Embed {
                title: title.clone(),
                description: text,
                color: spec.server.accent_rgb().unwrap_or(0),
            }),
        },
        None => MessageBody { content: text, embed: None },
    })
}

pub async fn apply(api: &dyn Api, spec: &Spec, images: &Images) -> ApiResult<Report> {
    let mut report = Report::default();
    for _ in 0..MAX_ROUNDS {
        let state = fetch(api, spec).await?;
        let p = plan(spec, &state);
        report.unmanaged = p.unmanaged.clone();
        report.pending = p.pending.clone();
        if p.actions.is_empty() {
            for (_, ch) in spec.channels() {
                let (Some(hook), Some(id)) = (&ch.webhook, Ids::from(&state).text(&ch.name).cloned()) else { continue };
                if let Some(url) = state
                    .webhooks
                    .get(&id)
                    .and_then(|hs| hs.iter().find(|h| &h.name == hook))
                    .and_then(WebhookState::url)
                {
                    report.webhooks.push((ch.name.clone(), url));
                }
            }
            return Ok(report);
        }
        let mut ids = Ids::from(&state);
        for action in p.actions {
            execute(api, spec, images, &state, &mut ids, &action).await.map_err(|e| format!("{action}: {e}"))?;
            report.executed.push(action.to_string());
        }
    }
    let leftover = plan(spec, &fetch(api, spec).await?).actions;
    Err(format!(
        "did not converge after {MAX_ROUNDS} passes; still pending:\n{}",
        leftover.iter().map(|a| format!("  {a}")).collect::<Vec<_>>().join("\n")
    ))
}

async fn execute(
    api: &dyn Api,
    spec: &Spec,
    images: &Images,
    state: &GuildState,
    ids: &mut Ids,
    action: &Action,
) -> ApiResult<()> {
    let image = |path: &Option<String>| -> ApiResult<String> {
        let p = path.as_deref().unwrap_or_default();
        images.get(p).cloned().ok_or_else(|| format!("image {p:?} was not loaded"))
    };
    let unresolved = || "refers to something that does not exist yet".to_owned();
    match action {
        Action::CreateRole(body) => {
            let r = api.create_role(body).await?;
            ids.roles.insert(r.name, r.id);
        }
        Action::UpdateRole { id, body, .. } => api.modify_role(id, body).await?,
        Action::SetEveryonePermissions(p) => api.set_everyone_permissions(*p).await?,
        Action::ReorderRoles(order) => {
            // Discord creates roles tied at position 1, the bot's own role
            // included, and refuses to place a role at or above the bot.
            // So ours take positions n..1 and, if needed, the bot lifts its
            // own role to n+1 in the same request (which Discord allows).
            let n = i64::try_from(order.len()).unwrap_or(0);
            let mut positions: Vec<(String, i64)> = order
                .iter()
                .enumerate()
                .filter_map(|(i, name)| Some((ids.roles.get(name)?.clone(), n - i64::try_from(i).unwrap_or(0))))
                .collect();
            let bot_role = state
                .roles
                .iter()
                .filter(|r| r.bot_id.as_deref() == Some(state.bot.id.as_str()))
                .max_by_key(|r| r.position);
            if let Some(b) = bot_role
                && b.position <= n
            {
                positions.push((b.id.clone(), n + 1));
            }
            api.reorder_roles(&positions).await?;
        }
        Action::CreateChannel(want) => {
            let body = ids.body(want).ok_or_else(unresolved)?;
            let c = api.create_channel(&body).await?;
            ids.channels.insert((c.kind, c.name), c.id);
        }
        Action::UpdateChannel { id, want, .. } => {
            let body = ids.body(want).ok_or_else(unresolved)?;
            api.modify_channel(id, &body).await?;
        }
        Action::ReorderChannels => {
            let mut positions = Vec::new();
            for (i, (cat, children)) in desired_order(spec).into_iter().enumerate() {
                let id = ids.channels.get(&(CATEGORY_TYPE, cat.name)).ok_or_else(unresolved)?;
                positions.push((id.clone(), i64::try_from(i).unwrap_or(0)));
                for (j, ch) in children.into_iter().enumerate() {
                    let id = ids.channels.get(&(ch.kind, ch.name)).ok_or_else(unresolved)?;
                    positions.push((id.clone(), i64::try_from(j).unwrap_or(0)));
                }
            }
            api.reorder_channels(&positions).await?;
        }
        Action::UpdateServer { .. } => {
            let s = &spec.server;
            let system_channel_id = match &s.system_channel {
                Some(n) => Some(ids.text(n).ok_or_else(unresolved)?.clone()),
                None => None,
            };
            api.modify_guild(&GuildBody {
                name: s.name.clone(),
                verification_level: s.verification_level,
                default_message_notifications: s.default_message_notifications,
                explicit_content_filter: s.explicit_content_filter,
                system_channel_id,
                settings: ids.settings(&s.settings).ok_or_else(unresolved)?,
            })
            .await?;
        }
        Action::CreateWebhook { channel, name } => {
            let id = ids.text(channel).ok_or_else(unresolved)?.clone();
            api.create_webhook(&id, name).await?;
        }
        Action::DeleteChannel { id, .. } => api.delete_channel(id).await?,
        Action::PostMessage { channel } | Action::EditMessage { channel, .. } => {
            let id = ids.text(channel).ok_or_else(unresolved)?.clone();
            let ch = spec.channels().map(|(_, c)| c).find(|c| &c.name == channel).ok_or_else(unresolved)?;
            let body = desired_message(spec, ch, ids).ok_or_else(unresolved)?;
            match action {
                Action::EditMessage { message_id, .. } => api.edit_message(&id, message_id, &body).await?,
                _ => {
                    api.create_message(&id, &body).await?;
                }
            }
        }
        Action::SetMessagePinned { channel, message_id, pinned } => {
            let id = ids.text(channel).ok_or_else(unresolved)?.clone();
            api.set_message_pinned(&id, message_id, *pinned).await?;
        }
        Action::SetServerIcon => api.set_guild_icon(&image(&spec.server.icon)?).await?,
        Action::SetBotAvatar => api.set_bot_avatar(&image(&spec.server.bot_avatar)?).await?,
        Action::AddMemberRole { user_id, role, .. } => {
            api.add_member_role(user_id, ids.roles.get(role).ok_or_else(unresolved)?).await?;
        }
        Action::RemoveMemberRole { user_id, role, .. } => {
            api.remove_member_role(user_id, ids.roles.get(role).ok_or_else(unresolved)?).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod fake;

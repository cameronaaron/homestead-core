//! In-memory Discord guild for provisioning tests, modelling the behaviours
//! that matter: a new server's starter channels, new roles inserting at
//! position 1, the role hierarchy (for ordering roles and for granting
//! them to members), and Discord's permission computation.

use std::sync::Mutex;

use homestead_bus::BoxFuture;

use super::spec::{Kind, bit};
use super::*;

const GUILD: &str = "100";
const BOT_USER: &str = "900";
const BOT_ROLE: &str = "901";
const ADMIN: u64 = 1 << 3;

/// Fields a real GET returns (values as observed on our server), which is
/// also every field this fake will change: like Discord, it silently
/// ignores any other key it's sent.
fn default_fields(kind: Option<u8>) -> Settings {
    let pairs: &[(&str, serde_json::Value)] = match kind {
        None => &[
            ("afk_timeout", serde_json::json!(300)),
            ("afk_channel_id", serde_json::Value::Null),
            ("description", serde_json::Value::Null),
            ("preferred_locale", serde_json::json!("en-US")),
        ],
        Some(0) => &[("rate_limit_per_user", serde_json::json!(0)), ("nsfw", serde_json::json!(false))],
        Some(2) => &[("bitrate", serde_json::json!(64000)), ("user_limit", serde_json::json!(0))],
        Some(_) => &[],
    };
    pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
}

/// Applies the settings Discord knows about; drops the rest.
fn absorb(fields: &mut Settings, settings: &Settings) {
    for (k, v) in settings {
        if let Some(slot) = fields.get_mut(k) {
            *slot = v.clone();
        }
    }
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    guild: Option<GuildInfo>,
    roles: Vec<RoleState>,
    channels: Vec<ChannelState>,
    messages: BTreeMap<String, Vec<MessageState>>,
    webhooks: BTreeMap<String, Vec<WebhookState>>,
    bot_avatar: Option<String>,
    members: Vec<MemberState>,
    mutations: usize,
}

pub struct Fake(Mutex<Inner>);

impl Fake {
    /// A server as Discord creates it, with the bot (Administrator) invited.
    pub fn new_server() -> Self {
        let ch = |id: &str, name: &str, kind: u8, parent: Option<&str>, position: i64| ChannelState {
            id: id.into(),
            name: name.into(),
            kind,
            parent_id: parent.map(Into::into),
            topic: None,
            position,
            overwrites: vec![],
            fields: default_fields(Some(kind)),
        };
        let role = |id: &str, name: &str, permissions: u64, position: i64, managed: bool| RoleState {
            id: id.into(),
            name: name.into(),
            permissions,
            color: 0,
            hoist: false,
            mentionable: false,
            position,
            managed,
            bot_id: managed.then(|| BOT_USER.to_owned()),
        };
        Fake(Mutex::new(Inner {
            next_id: 1000,
            guild: Some(GuildInfo {
                id: GUILD.into(),
                name: "cameron's server".into(),
                verification_level: 0,
                default_message_notifications: 0,
                explicit_content_filter: 0,
                system_channel_id: Some("201".into()),
                icon: None,
                fields: default_fields(None),
            }),
            roles: vec![role(GUILD, "@everyone", 0x6_3FFF_FFFF, 0, false), role(BOT_ROLE, "Homestead", ADMIN, 1, true)],
            channels: vec![
                ch("200", "Text Channels", CATEGORY_TYPE, None, 0),
                ch("201", "general", 0, Some("200"), 0),
                ch("202", "Voice Channels", CATEGORY_TYPE, None, 1),
                ch("203", "General", 2, Some("202"), 0),
            ],
            ..Inner::default()
        }))
    }

    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        f(&mut self.0.lock().expect("fake lock"))
    }

    pub fn mutations(&self) -> usize {
        self.with(|i| i.mutations)
    }

    /// Someone joins the server (with no roles, as Discord does).
    pub fn join(&self, username: &str) -> String {
        self.with(|i| {
            i.next_id += 1;
            let user_id = i.next_id.to_string();
            i.members.push(MemberState {
                user_id: user_id.clone(),
                username: username.into(),
                roles: vec![],
                bot: false,
            });
            user_id
        })
    }

    /// Role names a member holds, sorted.
    pub fn roles_of(&self, username: &str) -> Vec<String> {
        self.with(|i| {
            let m = i.members.iter().find(|m| m.username == username).expect(username);
            let mut names: Vec<String> =
                m.roles.iter().map(|id| i.roles.iter().find(|r| &r.id == id).expect("role").name.clone()).collect();
            names.sort();
            names
        })
    }

    /// A steward hands out a role in Discord by hand.
    pub fn grant_by_hand(&self, username: &str, role: &str) {
        self.with(|i| {
            let id = i.roles.iter().find(|r| r.name == role).expect(role).id.clone();
            i.members.iter_mut().find(|m| m.username == username).expect(username).roles.push(id);
        });
    }

    /// Discord refuses to grant or revoke a role at or above the bot's.
    fn member_role(&self, user_id: &str, role_id: &str, add: bool) -> ApiResult<()> {
        self.with(|i| {
            i.mutations += 1;
            let bot = i.roles.iter().find(|r| r.id == BOT_ROLE).expect("bot").position;
            let role = i.roles.iter().find(|r| r.id == role_id).ok_or("404 Unknown Role")?;
            if role.position >= bot {
                return Err(format!("403 Missing Permissions (50013): role {role_id} is not below the bot"));
            }
            let m = i.members.iter_mut().find(|m| m.user_id == user_id).ok_or("404 Unknown Member")?;
            m.roles.retain(|r| r != role_id);
            if add {
                m.roles.push(role_id.to_owned());
            }
            Ok(())
        })
    }

    pub fn tamper(&self, f: impl FnOnce(&mut Vec<RoleState>, &mut Vec<ChannelState>)) {
        self.with(|i| f(&mut i.roles, &mut i.channels));
    }

    /// Discord's documented algorithm: base = @everyone | member roles;
    /// Administrator short-circuits; then @everyone overwrite, then the
    /// union of the member's role overwrites (deny before allow).
    pub fn permissions(&self, member_roles: &[&str], channel: &str) -> u64 {
        self.with(|i| {
            let id_of = |n: &str| i.roles.iter().find(|r| r.name == n).map(|r| r.id.clone()).expect(n);
            let ids: Vec<String> = member_roles.iter().map(|n| id_of(n)).collect();
            let mut perms =
                i.roles.iter().filter(|r| r.id == GUILD || ids.contains(&r.id)).fold(0, |a, r| a | r.permissions);
            if perms & ADMIN != 0 {
                return u64::MAX;
            }
            let ch = i.channels.iter().find(|c| c.name == channel).expect(channel);
            if let Some(o) = ch.overwrites.iter().find(|o| o.id == GUILD) {
                perms = (perms & !o.deny) | o.allow;
            }
            let (allow, deny) = ch
                .overwrites
                .iter()
                .filter(|o| ids.contains(&o.id))
                .fold((0, 0), |(a, d), o| (a | o.allow, d | o.deny));
            (perms & !deny) | allow
        })
    }
}

fn ok<T: Send + 'static>(v: T) -> BoxFuture<'static, ApiResult<T>> {
    Box::pin(async move { Ok(v) })
}

impl Api for Fake {
    fn bot_user(&self) -> BoxFuture<'_, ApiResult<BotUser>> {
        ok(self.with(|i| BotUser { id: BOT_USER.to_owned(), avatar: i.bot_avatar.clone() }))
    }
    fn guild(&self) -> BoxFuture<'_, ApiResult<GuildInfo>> {
        ok(self.with(|i| i.guild.clone().expect("guild")))
    }
    fn roles(&self) -> BoxFuture<'_, ApiResult<Vec<RoleState>>> {
        ok(self.with(|i| i.roles.clone()))
    }
    fn channels(&self) -> BoxFuture<'_, ApiResult<Vec<ChannelState>>> {
        ok(self.with(|i| i.channels.clone()))
    }
    fn messages<'a>(&'a self, channel_id: &'a str) -> BoxFuture<'a, ApiResult<Vec<MessageState>>> {
        ok(self.with(|i| i.messages.get(channel_id).cloned().unwrap_or_default()))
    }
    fn webhooks<'a>(&'a self, channel_id: &'a str) -> BoxFuture<'a, ApiResult<Vec<WebhookState>>> {
        ok(self.with(|i| i.webhooks.get(channel_id).cloned().unwrap_or_default()))
    }
    fn create_role<'a>(&'a self, b: &'a RoleBody) -> BoxFuture<'a, ApiResult<RoleState>> {
        ok(self.with(|i| {
            i.mutations += 1;
            i.next_id += 1;
            // Observed on the live server (2026-09-28): new roles land at
            // position 1 *tied* with every other new role, the bot's
            // included. Nothing shifts.
            let r = RoleState {
                id: i.next_id.to_string(),
                name: b.name.clone(),
                permissions: b.permissions,
                color: b.color,
                hoist: b.hoist,
                mentionable: b.mentionable,
                position: 1,
                managed: false,
                bot_id: None,
            };
            i.roles.push(r.clone());
            r
        }))
    }
    fn modify_role<'a>(&'a self, id: &'a str, b: &'a RoleBody) -> BoxFuture<'a, ApiResult<()>> {
        self.with(|i| {
            i.mutations += 1;
            let r = i.roles.iter_mut().find(|r| r.id == id).expect("role");
            (r.name, r.permissions, r.color, r.hoist, r.mentionable) =
                (b.name.clone(), b.permissions, b.color, b.hoist, b.mentionable);
        });
        ok(())
    }
    fn set_everyone_permissions(&self, p: u64) -> BoxFuture<'_, ApiResult<()>> {
        self.with(|i| {
            i.mutations += 1;
            i.roles.iter_mut().find(|r| r.id == GUILD).expect("everyone").permissions = p;
        });
        ok(())
    }
    fn reorder_roles<'a>(&'a self, positions: &'a [(String, i64)]) -> BoxFuture<'a, ApiResult<()>> {
        let res = self.with(|i| {
            i.mutations += 1;
            // Discord's rule as observed: after the move, every role the bot
            // moved (other than its own) must sit below the bot's role. The
            // bot may lift its own role in the same request.
            let bot_after = positions
                .iter()
                .find(|(id, _)| id == BOT_ROLE)
                .map(|(_, p)| *p)
                .unwrap_or_else(|| i.roles.iter().find(|r| r.id == BOT_ROLE).expect("bot").position);
            if let Some((id, p)) = positions.iter().find(|(id, p)| id != BOT_ROLE && *p >= bot_after) {
                return Err(format!(
                    "400 Missing Permissions (50013): role {id} to position {p} is not below the bot ({bot_after})"
                ));
            }
            for (id, p) in positions {
                i.roles.iter_mut().find(|r| &r.id == id).expect("role").position = *p;
            }
            Ok(())
        });
        Box::pin(async move { res })
    }
    fn create_channel<'a>(&'a self, b: &'a ChannelBody) -> BoxFuture<'a, ApiResult<ChannelState>> {
        ok(self.with(|i| {
            i.mutations += 1;
            i.next_id += 1;
            let position = i
                .channels
                .iter()
                .filter(|c| c.parent_id == b.parent_id && (c.kind == CATEGORY_TYPE) == (b.kind == CATEGORY_TYPE))
                .map(|c| c.position + 1)
                .max()
                .unwrap_or(0);
            let mut fields = default_fields(Some(b.kind));
            absorb(&mut fields, &b.settings);
            let c = ChannelState {
                id: i.next_id.to_string(),
                name: b.name.clone(),
                kind: b.kind,
                parent_id: b.parent_id.clone(),
                topic: b.topic.clone(),
                position,
                overwrites: b.overwrites.clone(),
                fields,
            };
            i.channels.push(c.clone());
            c
        }))
    }
    fn modify_channel<'a>(&'a self, id: &'a str, b: &'a ChannelBody) -> BoxFuture<'a, ApiResult<()>> {
        self.with(|i| {
            i.mutations += 1;
            let c = i.channels.iter_mut().find(|c| c.id == id).expect("channel");
            c.name.clone_from(&b.name);
            c.parent_id.clone_from(&b.parent_id);
            if b.topic.is_some() {
                c.topic.clone_from(&b.topic);
            }
            c.overwrites.clone_from(&b.overwrites);
            absorb(&mut c.fields, &b.settings);
        });
        ok(())
    }
    fn reorder_channels<'a>(&'a self, positions: &'a [(String, i64)]) -> BoxFuture<'a, ApiResult<()>> {
        self.with(|i| {
            i.mutations += 1;
            for (id, p) in positions {
                i.channels.iter_mut().find(|c| &c.id == id).expect("channel").position = *p;
            }
        });
        ok(())
    }
    fn delete_channel<'a>(&'a self, id: &'a str) -> BoxFuture<'a, ApiResult<()>> {
        self.with(|i| {
            i.mutations += 1;
            i.channels.retain(|c| c.id != id);
        });
        ok(())
    }
    fn modify_guild<'a>(&'a self, b: &'a GuildBody) -> BoxFuture<'a, ApiResult<()>> {
        self.with(|i| {
            i.mutations += 1;
            let g = i.guild.as_mut().expect("guild");
            g.name.clone_from(&b.name);
            g.verification_level = b.verification_level;
            g.default_message_notifications = b.default_message_notifications;
            g.explicit_content_filter = b.explicit_content_filter;
            g.system_channel_id.clone_from(&b.system_channel_id);
            absorb(&mut g.fields, &b.settings);
        });
        ok(())
    }
    fn create_webhook<'a>(&'a self, channel_id: &'a str, name: &'a str) -> BoxFuture<'a, ApiResult<WebhookState>> {
        ok(self.with(|i| {
            i.mutations += 1;
            i.next_id += 1;
            let w = WebhookState { id: i.next_id.to_string(), name: name.into(), token: Some("tok".into()) };
            i.webhooks.entry(channel_id.into()).or_default().push(w.clone());
            w
        }))
    }
    fn create_message<'a>(
        &'a self,
        channel_id: &'a str,
        body: &'a MessageBody,
    ) -> BoxFuture<'a, ApiResult<MessageState>> {
        ok(self.with(|i| {
            i.mutations += 1;
            i.next_id += 1;
            let m = MessageState {
                id: i.next_id.to_string(),
                author_id: BOT_USER.into(),
                kind: 0,
                body: body.clone(),
                pinned: false,
            };
            i.messages.entry(channel_id.into()).or_default().push(m.clone());
            m
        }))
    }
    fn edit_message<'a>(
        &'a self,
        channel_id: &'a str,
        id: &'a str,
        body: &'a MessageBody,
    ) -> BoxFuture<'a, ApiResult<()>> {
        self.with(|i| {
            i.mutations += 1;
            let m = i.messages.get_mut(channel_id).and_then(|ms| ms.iter_mut().find(|m| m.id == id)).expect("message");
            m.body = body.clone();
        });
        ok(())
    }
    fn set_message_pinned<'a>(
        &'a self,
        channel_id: &'a str,
        message_id: &'a str,
        pinned: bool,
    ) -> BoxFuture<'a, ApiResult<()>> {
        self.with(|i| {
            i.mutations += 1;
            let notice_id = pinned.then(|| {
                i.next_id += 1;
                i.next_id.to_string()
            });
            let messages = i.messages.get_mut(channel_id).expect("messages");
            messages.iter_mut().find(|m| m.id == message_id).expect("message").pinned = pinned;
            if let Some(id) = notice_id {
                // Discord adds a type-6 pin notice, attributed to the bot.
                // GET /messages returns it before the post it refers to.
                messages.insert(
                    0,
                    MessageState {
                        id,
                        author_id: BOT_USER.into(),
                        kind: 6,
                        body: MessageBody::default(),
                        pinned: false,
                    },
                );
            }
        });
        ok(())
    }
    fn set_guild_icon<'a>(&'a self, image: &'a str) -> BoxFuture<'a, ApiResult<()>> {
        self.with(|i| {
            i.mutations += 1;
            assert!(image.starts_with("data:image/"), "icon must be a data URI");
            i.guild.as_mut().expect("guild").icon = Some("hash".into());
        });
        ok(())
    }
    fn set_bot_avatar<'a>(&'a self, image: &'a str) -> BoxFuture<'a, ApiResult<()>> {
        self.with(|i| {
            i.mutations += 1;
            assert!(image.starts_with("data:image/"), "avatar must be a data URI");
            i.bot_avatar = Some("hash".into());
        });
        ok(())
    }
    fn members(&self) -> BoxFuture<'_, ApiResult<Vec<MemberState>>> {
        ok(self.with(|i| {
            let mut all = i.members.clone();
            all.push(MemberState {
                user_id: BOT_USER.into(),
                username: "homestead".into(),
                roles: vec![BOT_ROLE.into()],
                bot: true,
            });
            all
        }))
    }
    fn add_member_role<'a>(&'a self, user_id: &'a str, role_id: &'a str) -> BoxFuture<'a, ApiResult<()>> {
        let res = self.member_role(user_id, role_id, true);
        Box::pin(async move { res })
    }
    fn remove_member_role<'a>(&'a self, user_id: &'a str, role_id: &'a str) -> BoxFuture<'a, ApiResult<()>> {
        let res = self.member_role(user_id, role_id, false);
        Box::pin(async move { res })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

fn spec() -> Spec {
    Spec::parse(include_str!("../../../../../discord/server.toml")).expect("valid spec")
}

fn images() -> Images {
    let s = spec();
    [s.server.icon, s.server.bot_avatar]
        .into_iter()
        .flatten()
        .map(|p| (p, "data:image/png;base64,AAAA".to_owned()))
        .collect()
}

async fn provisioned() -> (Fake, Report) {
    let fake = Fake::new_server();
    let report = apply(&fake, &spec(), &images()).await.expect("apply converges");
    (fake, report)
}

#[tokio::test]
async fn converges_from_a_new_server_and_second_run_changes_nothing() {
    let (fake, report) = provisioned().await;
    assert!(!report.executed.is_empty());
    let after_first = fake.mutations();
    let again = apply(&fake, &spec(), &images()).await.expect("second apply");
    assert!(again.executed.is_empty(), "second run should be a no-op, did: {:?}", again.executed);
    assert_eq!(fake.mutations(), after_first, "no API writes on an already-converged server");
    assert!(plan(&spec(), &fetch(&fake, &spec()).await.expect("state")).actions.is_empty());
}

#[tokio::test]
async fn adopts_starter_general_and_removes_empty_starter_scaffolding() {
    let (fake, report) = provisioned().await;
    let state = fetch(&fake, &spec()).await.expect("state");
    let general = state.channels.iter().find(|c| c.name == "general").expect("general kept");
    assert_eq!(general.id, "201", "Discord's own #general is adopted, not duplicated");
    for gone in ["Text Channels", "Voice Channels", "General"] {
        assert!(!state.channels.iter().any(|c| c.name == gone), "{gone} should be removed");
    }
    assert!(report.unmanaged.is_empty(), "unexpected leftovers: {:?}", report.unmanaged);
}

#[tokio::test]
async fn roles_are_ordered_below_the_bot_as_listed() {
    let (fake, _) = provisioned().await;
    let mut roles = fake.with(|i| i.roles.clone());
    roles.sort_by_key(|r| std::cmp::Reverse(r.position));
    let names: Vec<&str> = roles.iter().map(|r| r.name.as_str()).collect();
    let spec = spec();
    let mut want = vec!["Homestead"];
    want.extend(spec.roles.iter().map(|r| r.name.as_str()));
    want.push("@everyone");
    assert_eq!(names, want);
}

#[tokio::test]
async fn access_matrix_matches_the_intended_model() {
    let (fake, _) = provisioned().await;
    let view = |roles: &[&str], ch: &str| fake.permissions(roles, ch) & VIEW != 0;
    let post = |roles: &[&str], ch: &str| {
        let p = fake.permissions(roles, ch);
        p & VIEW != 0 && p & bit("send_messages").expect("bit") != 0
    };
    // A newcomer with no role sees only Start here, and can't post there.
    assert!(view(&[], "welcome") && view(&[], "rules") && view(&[], "announcements"));
    assert!(!post(&[], "welcome"));
    for hidden in ["general", "volunteer-chat", "daily-log", "farm-feed", "leadership", "Farmhouse"] {
        assert!(!view(&[], hidden), "newcomer can see {hidden}");
    }
    // Community: community only.
    assert!(post(&["Community"], "general") && view(&["Community"], "Farmhouse"));
    assert!(!view(&["Community"], "volunteer-chat") && !view(&["Community"], "daily-log"));
    // Volunteers: volunteer channels; the guide is read-only.
    assert!(post(&["Volunteer"], "volunteer-chat") && post(&["Volunteer"], "general"));
    assert!(view(&["Volunteer"], "volunteer-guide") && !post(&["Volunteer"], "volunteer-guide"));
    assert!(!view(&["Volunteer"], "daily-log"));
    // Crew: ops, but not leadership; nobody chats in the automated feed.
    assert!(post(&["Crew"], "daily-log") && post(&["Crew"], "ops-bot") && view(&["Crew"], "Crew Huddle"));
    assert!(view(&["Crew"], "farm-feed") && !post(&["Crew"], "farm-feed"));
    assert!(!view(&["Crew"], "leadership") && !view(&["Crew"], "Leadership Room"));
    // Board: leadership, and read-only oversight of operations.
    assert!(post(&["Board"], "finance") && view(&["Board"], "daily-log") && !post(&["Board"], "daily-log"));
    assert!(!view(&["Board"], "volunteer-chat") && !view(&["Board"], "Crew Huddle"));
    // Farm Lead: announces, runs ops and volunteers, sits in leadership.
    assert!(post(&["Farm Lead"], "announcements") && post(&["Farm Lead"], "volunteer-guide"));
    assert!(post(&["Farm Lead"], "leadership") && !post(&["Farm Lead"], "farm-feed"));
    // Stewards administer everything.
    assert!(post(&["Steward"], "farm-feed") && post(&["Steward"], "finance"));
    // Holding several roles unions their access.
    assert!(post(&["Board", "Crew"], "daily-log"));
    // Working groups open exactly their one channel, and nothing beside it.
    assert!(post(&["Grants & Business"], "fundraising"));
    assert!(!view(&["Grants & Business"], "leadership") && !view(&["Grants & Business"], "finance"));
    assert!(post(&["Social Media"], "social-media") && !view(&["Social Media"], "daily-log"));
    assert!(post(&["Farm & Labor"], "tasks") && !view(&["Farm & Labor"], "daily-log"));
    assert!(!view(&["Farm & Labor"], "general"), "a working group alone grants no access");
    // Adding a group to a channel doesn't take it from the roles that had it.
    assert!(post(&["Crew"], "social-media") && view(&["Board"], "social-media") && !post(&["Board"], "social-media"));
    assert!(post(&["Board"], "fundraising") && post(&["Farm Lead"], "fundraising"));
    assert!(post(&["Crew"], "tasks") && view(&["Board"], "tasks") && !post(&["Board"], "tasks"));
}

#[tokio::test]
async fn links_resolve_and_messages_and_webhook_are_in_place() {
    let (fake, report) = provisioned().await;
    let state = fetch(&fake, &spec()).await.expect("state");
    let id = |n: &str| state.channels.iter().find(|c| c.name == n).expect(n).id.clone();
    let welcome = &state.messages[&id("welcome")][0].body;
    let card = welcome.embed.as_ref().expect("welcome is an embed card");
    assert_eq!(card.color, spec().server.accent_rgb().expect("accent"));
    let welcome_msg = &card.description;
    assert!(welcome_msg.contains(&format!("<#{}>", id("rules"))), "channel link not resolved: {welcome_msg}");
    assert!(!welcome_msg.contains("<#rules>"));
    let topic = state.channels.iter().find(|c| c.name == "welcome").and_then(|c| c.topic.clone()).expect("topic");
    assert!(topic.contains(&format!("<#{}>", id("rules"))));
    assert_eq!(report.webhooks.len(), 1);
    assert_eq!(report.webhooks[0].0, "farm-feed");
    assert!(report.webhooks[0].1.starts_with("https://discord.com/api/webhooks/"));
    assert_eq!(state.guild.system_channel_id, Some(id("welcome")));
    assert_eq!(state.guild.name, "Sun Brewed Homestead");
    assert!(state.guild.icon.is_some() && state.bot.avatar.is_some(), "icon and avatar set from brand/");
}

#[tokio::test]
async fn drift_is_detected_precisely_and_repaired() {
    let (fake, _) = provisioned().await;
    fake.tamper(|roles, channels| {
        roles.iter_mut().find(|r| r.name == "Crew").expect("crew").permissions |= ADMIN;
        let feed = channels.iter_mut().find(|c| c.name == "farm-feed").expect("feed");
        feed.overwrites.clear();
        feed.topic = Some("hacked".into());
    });
    let p = plan(&spec(), &fetch(&fake, &spec()).await.expect("state"));
    let shown: Vec<String> = p.actions.iter().map(ToString::to_string).collect();
    assert_eq!(shown, [r#"~ role "Crew" (permissions)"#, r#"~ channel "farm-feed" (topic, permissions)"#]);
    apply(&fake, &spec(), &images()).await.expect("repair");
    assert!(fake.permissions(&["Crew"], "farm-feed") & bit("send_messages").expect("b") == 0);
    assert_eq!(fake.permissions(&["Crew"], "farm-feed") & ADMIN, 0);
}

#[tokio::test]
async fn things_outside_the_spec_are_reported_not_deleted() {
    let (fake, _) = provisioned().await;
    fake.tamper(|roles, channels| {
        roles.push(RoleState {
            id: "5000".into(),
            name: "Goat Fan Club".into(),
            permissions: 0,
            color: 0,
            hoist: false,
            mentionable: false,
            position: 2,
            managed: false,
            bot_id: None,
        });
        channels.push(ChannelState {
            id: "5001".into(),
            name: "goat-memes".into(),
            kind: 0,
            parent_id: None,
            topic: None,
            position: 99,
            overwrites: vec![],
            fields: Settings::new(),
        });
    });
    let report = apply(&fake, &spec(), &images()).await.expect("apply");
    assert!(report.unmanaged.contains(&r#"role "Goat Fan Club""#.to_owned()));
    assert!(report.unmanaged.contains(&r#"channel "goat-memes""#.to_owned()));
    let state = fetch(&fake, &spec()).await.expect("state");
    assert!(state.channels.iter().any(|c| c.name == "goat-memes"), "unmanaged channel must survive");
}

#[tokio::test]
async fn edited_spec_message_updates_the_existing_post_in_place() {
    let (fake, _) = provisioned().await;
    let mut edited = spec();
    let rules = edited.categories[0].channels.iter_mut().find(|c| c.name == "rules").expect("rules");
    rules.message = Some("New rules.".into());
    let report = apply(&fake, &edited, &images()).await.expect("apply");
    assert_eq!(report.executed, ["~ bot message in #rules"]);
    let state = fetch(&fake, &edited).await.expect("state");
    let rules_id = state.channels.iter().find(|c| c.name == "rules").expect("rules").id.clone();
    assert_eq!(state.messages[&rules_id].len(), 1, "edited, not re-posted");
    let body = &state.messages[&rules_id][0].body;
    assert_eq!(body.embed.as_ref().map(|e| e.description.as_str()), Some("New rules."));
}

#[tokio::test]
async fn managed_message_pin_state_is_created_and_repaired() {
    let fake = Fake::new_server();
    let mut pinned = spec();
    let guide = pinned
        .categories
        .iter_mut()
        .flat_map(|category| &mut category.channels)
        .find(|channel| channel.name == "volunteer-guide")
        .expect("guide");
    guide.message = Some("Call a permitted wildlife rehabilitator.".into());
    guide.title = Some("When wildlife turns up".into());
    guide.pinned = Some(true);

    let report = apply(&fake, &pinned, &images()).await.expect("apply");
    assert!(report.executed.contains(&"+ bot message in #volunteer-guide".to_owned()));
    assert!(report.executed.contains(&"+ pin bot message in #volunteer-guide".to_owned()));

    let state = fetch(&fake, &pinned).await.expect("state");
    let guide_id = state.channels.iter().find(|c| c.name == "volunteer-guide").expect("guide").id.clone();
    let managed = state.messages[&guide_id].iter().find(|message| message.kind == 0).expect("managed post");
    assert!(managed.pinned);

    fake.with(|inner| {
        inner
            .messages
            .get_mut(&guide_id)
            .expect("messages")
            .iter_mut()
            .find(|message| message.kind == 0)
            .expect("post")
            .pinned = false;
    });
    let plan = plan(&pinned, &fetch(&fake, &pinned).await.expect("state"));
    assert_eq!(
        plan.actions.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["+ pin bot message in #volunteer-guide"]
    );
}

fn with_members(members: &[(&str, &[&str])]) -> Spec {
    let mut s = spec();
    s.members = members.iter().map(|(u, rs)| ((*u).to_owned(), rs.iter().map(|r| (*r).to_owned()).collect())).collect();
    s.validate().expect("valid members");
    s
}

#[tokio::test]
async fn listed_members_get_their_roles_on_a_brand_new_server() {
    // They joined before the roles even existed: granting must wait until
    // the roles are created and ordered below the bot, in the same apply.
    let fake = Fake::new_server();
    fake.join("kiki");
    fake.join("bystander");
    let spec = with_members(&[("kiki", &["Crew", "Volunteer"]), ("not_here_yet", &["Community"])]);
    let report = apply(&fake, &spec, &images()).await.expect("apply converges");
    assert_eq!(fake.roles_of("kiki"), ["Crew", "Volunteer"]);
    assert!(fake.roles_of("bystander").is_empty(), "unlisted members are left alone");
    assert!(report.unmanaged.contains(&"member @bystander (no role yet)".to_owned()), "{:?}", report.unmanaged);
    assert!(!report.unmanaged.iter().any(|u| u.contains("homestead")), "bots aren't people");
    assert_eq!(report.pending, ["not_here_yet"]);
    assert!(report.executed.contains(&r#"+ @kiki gets role "Crew""#.to_owned()), "{:?}", report.executed);

    let before = fake.mutations();
    let again = apply(&fake, &spec, &images()).await.expect("second apply");
    assert!(again.executed.is_empty(), "second run should be a no-op, did: {:?}", again.executed);
    assert_eq!(fake.mutations(), before);
    assert_eq!(again.pending, ["not_here_yet"], "still pending, and pending is not drift");
}

#[tokio::test]
async fn a_member_who_joins_later_is_picked_up_on_the_next_run() {
    let (fake, _) = provisioned().await;
    let spec = with_members(&[("kiki", &["Community"])]);
    let p = plan(&spec, &fetch(&fake, &spec).await.expect("state"));
    assert!(p.actions.is_empty() && p.pending == ["kiki"]);
    fake.join("kiki");
    let report = apply(&fake, &spec, &images()).await.expect("apply");
    assert_eq!(report.executed, [r#"+ @kiki gets role "Community""#]);
    assert!(report.pending.is_empty());
    assert!(fake.permissions(&["Community"], "general") & VIEW != 0);
}

#[tokio::test]
async fn the_list_is_authoritative_for_spec_roles_only() {
    let (fake, _) = provisioned().await;
    fake.join("kiki");
    fake.tamper(|roles, _| {
        roles.push(RoleState {
            id: "5000".into(),
            name: "Goat Fan Club".into(),
            permissions: 0,
            color: 0,
            hoist: false,
            mentionable: false,
            position: 2,
            managed: false,
            bot_id: None,
        });
    });
    fake.grant_by_hand("kiki", "Steward");
    fake.grant_by_hand("kiki", "Goat Fan Club");
    fake.join("stray");
    fake.grant_by_hand("stray", "Steward");
    let spec = with_members(&[("kiki", &["Crew"])]);
    let p = plan(&spec, &fetch(&fake, &spec).await.expect("state"));
    assert!(p.unmanaged.contains(&"member @stray (Steward)".to_owned()), "an unlisted admin must be visible");
    let shown: Vec<String> = p.actions.iter().map(ToString::to_string).collect();
    assert_eq!(shown, [r#"- @kiki loses role "Steward""#, r#"+ @kiki gets role "Crew""#]);
    apply(&fake, &spec, &images()).await.expect("apply");
    assert_eq!(fake.roles_of("kiki"), ["Crew", "Goat Fan Club"], "a role outside the spec is not ours to remove");
    assert_eq!(fake.roles_of("stray"), ["Steward"], "unlisted people are reported, not changed");

    // An empty list takes every spec role away.
    let spec = with_members(&[("kiki", &[])]);
    apply(&fake, &spec, &images()).await.expect("apply");
    assert_eq!(fake.roles_of("kiki"), ["Goat Fan Club"]);
}

fn with_settings(server: &[(&str, serde_json::Value)], channel: &str, ch: &[(&str, serde_json::Value)]) -> Spec {
    let mut s = spec();
    s.server.settings = server.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect();
    let c = s.categories.iter_mut().flat_map(|c| &mut c.channels).find(|c| c.name == channel).expect(channel);
    c.settings = ch.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect();
    s.validate().expect("valid settings");
    s
}

#[tokio::test]
async fn settings_pass_through_converge_and_name_channels() {
    use serde_json::json;
    let (fake, _) = provisioned().await;
    let spec = with_settings(
        &[("afk_timeout", json!(900)), ("afk_channel_id", json!("Farmhouse"))],
        "general",
        &[("rate_limit_per_user", json!(10))],
    );
    let p = plan(&spec, &fetch(&fake, &spec).await.expect("state"));
    let shown: Vec<String> = p.actions.iter().map(ToString::to_string).collect();
    assert_eq!(
        shown,
        [r#"~ channel "general" (rate_limit_per_user)"#, "~ server settings (afk_channel_id, afk_timeout)"]
    );
    apply(&fake, &spec, &images()).await.expect("apply");
    let state = fetch(&fake, &spec).await.expect("state");
    let farmhouse = state.channels.iter().find(|c| c.name == "Farmhouse").expect("voice").id.clone();
    assert_eq!(state.guild.fields["afk_channel_id"], json!(farmhouse), "a channel name becomes its id");
    assert_eq!(state.guild.fields["afk_timeout"], json!(900));
    let general = state.channels.iter().find(|c| c.name == "general").expect("general");
    assert_eq!(general.fields["rate_limit_per_user"], json!(10));
    assert!(plan(&spec, &state).actions.is_empty(), "converged settings are not drift");
}

#[tokio::test]
async fn a_setting_discord_ignores_fails_loudly_instead_of_passing() {
    let (fake, _) = provisioned().await;
    let spec = with_settings(&[("afk_timout", serde_json::json!(900))], "general", &[]);
    let err = apply(&fake, &spec, &images()).await.expect_err("typo can never converge");
    assert!(err.contains("afk_timout"), "the error names the field: {err}");
}

#[test]
fn render_handles_unknown_and_numeric_links() {
    let mut ids = Ids::default();
    ids.channels.insert((0, "rules".into()), "7".into());
    assert_eq!(ids.render("see <#rules> and <#nope>"), "see <#7> and #nope");
    assert_eq!(ids.render("already <#123> ok"), "already <#123> ok", "real ids pass through");
    assert_eq!(ids.render("dangling <#"), "dangling <#");
    let _ = Kind::Text;
}

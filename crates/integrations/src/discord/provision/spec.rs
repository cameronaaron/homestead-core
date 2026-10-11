//! The declarative server spec (`discord/server.toml`) and its validation.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;

pub const EVERYONE: &str = "@everyone";

/// Discord permission bits by lowercase name. Values from the Discord
/// developer docs ("Bitwise Permission Flags").
pub const PERMISSIONS: &[(&str, u64)] = &[
    ("create_instant_invite", 1 << 0),
    ("kick_members", 1 << 1),
    ("ban_members", 1 << 2),
    ("administrator", 1 << 3),
    ("manage_channels", 1 << 4),
    ("manage_guild", 1 << 5),
    ("add_reactions", 1 << 6),
    ("view_audit_log", 1 << 7),
    ("priority_speaker", 1 << 8),
    ("stream", 1 << 9),
    ("view_channel", 1 << 10),
    ("send_messages", 1 << 11),
    ("send_tts_messages", 1 << 12),
    ("manage_messages", 1 << 13),
    ("embed_links", 1 << 14),
    ("attach_files", 1 << 15),
    ("read_message_history", 1 << 16),
    ("mention_everyone", 1 << 17),
    ("use_external_emojis", 1 << 18),
    ("view_guild_insights", 1 << 19),
    ("connect", 1 << 20),
    ("speak", 1 << 21),
    ("mute_members", 1 << 22),
    ("deafen_members", 1 << 23),
    ("move_members", 1 << 24),
    ("use_vad", 1 << 25),
    ("change_nickname", 1 << 26),
    ("manage_nicknames", 1 << 27),
    ("manage_roles", 1 << 28),
    ("manage_webhooks", 1 << 29),
    ("manage_guild_expressions", 1 << 30),
    ("use_application_commands", 1 << 31),
    ("request_to_speak", 1 << 32),
    ("manage_events", 1 << 33),
    ("manage_threads", 1 << 34),
    ("create_public_threads", 1 << 35),
    ("create_private_threads", 1 << 36),
    ("use_external_stickers", 1 << 37),
    ("send_messages_in_threads", 1 << 38),
    ("use_embedded_activities", 1 << 39),
    ("moderate_members", 1 << 40),
    ("use_soundboard", 1 << 42),
    ("create_guild_expressions", 1 << 43),
    ("create_events", 1 << 44),
    ("use_external_sounds", 1 << 45),
    ("send_voice_messages", 1 << 46),
    ("send_polls", 1 << 49),
    ("pin_messages", 1 << 51),
];

pub fn bit(name: &str) -> Option<u64> {
    PERMISSIONS.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
}

pub const VIEW: u64 = 1 << 10;
/// Everything that lets someone post. Denied to readers who aren't writers.
pub const POST: u64 = (1 << 11) | (1 << 38) | (1 << 35) | (1 << 36) | (1 << 46) | (1 << 49);

/// Discord's limit on message content.
pub const MAX_MESSAGE: usize = 2000;
/// Discord's limit on an embed description.
pub const MAX_EMBED: usize = 4096;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub server: Server,
    pub roles: Vec<Role>,
    pub categories: Vec<Category>,
    /// Discord username → the spec roles that person holds. Their spec
    /// roles are made to match exactly; roles outside the spec are left
    /// alone, and so is anyone not listed.
    #[serde(default)]
    pub members: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub name: String,
    pub verification_level: u8,
    pub default_message_notifications: u8,
    pub explicit_content_filter: u8,
    pub system_channel: Option<String>,
    pub everyone_permissions: Vec<String>,
    /// Remove Discord's starter "Text Channels"/"Voice Channels" categories
    /// (once empty) and its "General" voice channel. Never deletes anything
    /// else, and never a text channel (those hold history).
    #[serde(default)]
    pub remove_starter_channels: bool,
    /// Brand colour (#RRGGBB) for the bot's embed cards.
    pub accent: Option<String>,
    /// Image files for the server icon and the bot's avatar. Applied only
    /// while none is set, so a hand-uploaded image is never replaced.
    pub icon: Option<String>,
    pub bot_avatar: Option<String>,
    /// Any other Modify Guild field, sent as-is (e.g. `afk_timeout`). A
    /// `*_channel_id` may name a channel instead of giving its id.
    #[serde(default)]
    pub settings: Settings,
}

/// Raw API fields, compared with what Discord reports and sent when they
/// differ.
pub type Settings = BTreeMap<String, Value>;

/// Settings keys the typed spec fields own; setting them twice would fight.
pub const SERVER_OWNED: &[&str] = &[
    "id",
    "name",
    "verification_level",
    "default_message_notifications",
    "explicit_content_filter",
    "system_channel_id",
    "icon",
];
pub const CHANNEL_OWNED: &[&str] = &["id", "name", "type", "topic", "parent_id", "permission_overwrites", "position"];

/// A settings value that names a channel (`afk_channel_id = "Farmhouse"`)
/// rather than giving its numeric id.
pub fn channel_ref<'a>(key: &str, v: &'a Value) -> Option<&'a str> {
    v.as_str().filter(|s| key.ends_with("_channel_id") && !s.chars().all(|c| c.is_ascii_digit()))
}

impl Server {
    pub fn accent_rgb(&self) -> Result<u32, String> {
        let hex = self.accent.as_deref().unwrap_or("#000000").trim_start_matches('#');
        u32::from_str_radix(hex, 16)
            .ok()
            .filter(|_| hex.len() == 6)
            .ok_or_else(|| "server.accent must be #RRGGBB".to_owned())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Role {
    pub name: String,
    pub color: String,
    #[serde(default)]
    pub hoist: bool,
    #[serde(default)]
    pub mentionable: bool,
    #[serde(default)]
    pub about: String,
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Category {
    pub name: String,
    pub readers: Vec<String>,
    pub writers: Option<Vec<String>>,
    pub channels: Vec<Channel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Text,
    Voice,
}

impl Kind {
    pub fn discord_type(self) -> u8 {
        match self {
            Kind::Text => 0,
            Kind::Voice => 2,
        }
    }
}

pub const CATEGORY_TYPE: u8 = 4;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    pub name: String,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default)]
    pub topic: String,
    pub readers: Option<Vec<String>>,
    pub writers: Option<Vec<String>>,
    /// Posted once by the bot and kept in sync (edited) on later runs.
    pub message: Option<String>,
    /// With `message`: post it as an embed card with this title.
    pub title: Option<String>,
    /// Keep the managed bot message pinned (`true`) or unpinned (`false`).
    /// When absent, pin state is left to people in Discord.
    pub pinned: Option<bool>,
    /// Name of an incoming webhook to ensure exists in this channel.
    pub webhook: Option<String>,
    /// Any other Modify Channel field, sent as-is (e.g. `rate_limit_per_user`,
    /// `nsfw`, `user_limit`, `bitrate`).
    #[serde(default)]
    pub settings: Settings,
}

/// Who may see / post, resolved from channel overrides then category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Access {
    pub readers: BTreeSet<String>,
    pub writers: BTreeSet<String>,
}

impl Access {
    /// `@everyone` as a reader includes every role.
    pub fn can_read(&self, who: &str) -> bool {
        self.readers.contains(EVERYONE) || self.readers.contains(who)
    }
}

impl Category {
    pub fn access(&self) -> Access {
        let readers: BTreeSet<String> = self.readers.iter().cloned().collect();
        let writers = self.writers.as_ref().map_or_else(|| readers.clone(), |w| w.iter().cloned().collect());
        Access { readers, writers }
    }
}

impl Channel {
    pub fn access(&self, cat: &Category) -> Access {
        let base = cat.access();
        let readers: BTreeSet<String> =
            self.readers.as_ref().map_or(base.readers.clone(), |r| r.iter().cloned().collect());
        let writers = match (&self.writers, &self.readers) {
            (Some(w), _) => w.iter().cloned().collect(),
            // Own readers but no own writers: everyone who can read may post,
            // unless the category restricted writers.
            (None, Some(_)) if cat.writers.is_none() => readers.clone(),
            (None, _) => {
                let narrowed = Access { readers: readers.clone(), writers: BTreeSet::new() };
                base.writers.iter().filter(|w| narrowed.can_read(w)).cloned().collect()
            }
        };
        Access { readers, writers }
    }
}

/// Why `name` isn't a Discord username (the unique handle, not the display
/// name): 2–32 of `a-z 0-9 _ .`, no `..`. `None` if it is one.
pub fn username_problem(name: &str) -> Option<String> {
    let valid = (2..=32).contains(&name.len())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
        && !name.contains("..");
    let hint = if name.contains('#') {
        "use their current username, not the old name#1234 form"
    } else if name.chars().any(|c| c.is_ascii_uppercase()) {
        "usernames are lowercase"
    } else {
        "use the username from their Discord profile, not their display name"
    };
    (!valid).then(|| format!("member {name:?} is not a Discord username ({hint})"))
}

/// Channel names referenced as `<#name>` (numeric ids are not names).
pub fn links(text: &str) -> impl Iterator<Item = &str> {
    text.split("<#").skip(1).filter_map(|rest| {
        let name = &rest[..rest.find('>')?];
        (!name.is_empty() && !name.chars().all(|c| c.is_ascii_digit())).then_some(name)
    })
}

impl Spec {
    pub fn parse(toml_text: &str) -> Result<Self, String> {
        let spec: Spec = toml::from_str(toml_text).map_err(|e| e.to_string())?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn permission_bits(names: &[String]) -> Result<u64, String> {
        names.iter().try_fold(0u64, |acc, n| bit(n).map(|b| acc | b).ok_or_else(|| format!("unknown permission {n:?}")))
    }

    pub fn role_color(role: &Role) -> Result<u32, String> {
        let hex = role.color.trim_start_matches('#');
        u32::from_str_radix(hex, 16)
            .ok()
            .filter(|_| hex.len() == 6)
            .ok_or_else(|| format!("role {:?}: color must be #RRGGBB", role.name))
    }

    /// Every channel with its category, in spec order.
    pub fn channels(&self) -> impl Iterator<Item = (&Category, &Channel)> {
        self.categories.iter().flat_map(|c| c.channels.iter().map(move |ch| (c, ch)))
    }

    pub fn validate(&self) -> Result<(), String> {
        let mut errors = Vec::new();
        Self::permission_bits(&self.server.everyone_permissions).map_err(|e| errors.push(e)).ok();
        self.server.accent_rgb().map_err(|e| errors.push(e)).ok();

        let mut roles = BTreeSet::new();
        for r in &self.roles {
            if r.name == EVERYONE || r.name.trim().is_empty() {
                errors.push(format!("invalid role name {:?}", r.name));
            }
            if !roles.insert(r.name.as_str()) {
                errors.push(format!("duplicate role {:?}", r.name));
            }
            Self::permission_bits(&r.permissions).map_err(|e| errors.push(format!("role {:?}: {e}", r.name))).ok();
            Self::role_color(r).map_err(|e| errors.push(e)).ok();
        }
        let known = |who: &str| who == EVERYONE || roles.contains(who);

        let mut names: BTreeMap<(Kind, &str), &str> = BTreeMap::new();
        let mut categories = BTreeSet::new();
        for cat in &self.categories {
            if !categories.insert(cat.name.as_str()) {
                errors.push(format!("duplicate category {:?}", cat.name));
            }
            // Checked here too: channel inheritance narrows writers to
            // readers, which would otherwise hide a category-level mistake.
            let cat_access = cat.access();
            for who in cat_access.readers.iter().chain(&cat_access.writers) {
                if !known(who) {
                    errors.push(format!("category {:?}: unknown role {who:?}", cat.name));
                }
            }
            if !cat_access.writers.iter().all(|w| cat_access.can_read(w)) {
                errors.push(format!("category {:?}: every writer must also be a reader", cat.name));
            }
            for (ch_cat, ch) in cat.channels.iter().map(|ch| (cat, ch)) {
                let where_ = format!("channel {:?}", ch.name);
                if let Some(prev) = names.insert((ch.kind, ch.name.as_str()), &cat.name) {
                    errors.push(format!("{where_} appears in both {prev:?} and {:?}", cat.name));
                }
                if ch.kind == Kind::Text
                    && !ch.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
                {
                    errors.push(format!(
                        "{where_}: text channel names must be lowercase-with-dashes (Discord rewrites others)"
                    ));
                }
                let access = ch.access(ch_cat);
                for who in access.readers.iter().chain(&access.writers) {
                    if !known(who) {
                        errors.push(format!("{where_}: unknown role {who:?}"));
                    }
                }
                if !access.writers.iter().all(|w| access.can_read(w)) {
                    errors.push(format!("{where_}: every writer must also be a reader"));
                }
                if let Some(m) = &ch.message {
                    if ch.kind != Kind::Text {
                        errors.push(format!("{where_}: messages only go in text channels"));
                    }
                    let limit = if ch.title.is_some() { MAX_EMBED } else { MAX_MESSAGE };
                    if m.trim().chars().count() > limit {
                        errors.push(format!("{where_}: message exceeds {limit} characters"));
                    }
                }
                if ch.title.is_some() && ch.message.is_none() {
                    errors.push(format!("{where_}: title needs a message"));
                }
                if ch.pinned.is_some() && ch.message.is_none() {
                    errors.push(format!("{where_}: pinned needs a message"));
                }
                if ch.webhook.is_some() && ch.kind != Kind::Text {
                    errors.push(format!("{where_}: webhooks only go in text channels"));
                }
            }
        }
        for (_, ch) in self.channels() {
            for text in [Some(ch.topic.as_str()), ch.message.as_deref()].into_iter().flatten() {
                for link in links(text) {
                    if !names.contains_key(&(Kind::Text, link)) {
                        errors.push(format!(
                            "channel {:?} links <#{link}>, which is not a text channel in this spec",
                            ch.name
                        ));
                    }
                }
            }
        }
        for (name, held) in &self.members {
            errors.extend(username_problem(name));
            let mut seen = BTreeSet::new();
            for role in held {
                if !roles.contains(role.as_str()) {
                    errors.push(format!("member {name:?}: unknown role {role:?}"));
                }
                if !seen.insert(role) {
                    errors.push(format!("member {name:?}: role {role:?} is listed twice"));
                }
            }
        }
        let any_channel = |n: &str| names.keys().any(|(_, name)| *name == n);
        let mut check_settings = |whose: &str, settings: &Settings, owned: &[&str]| {
            for (k, v) in settings {
                if owned.contains(&k.as_str()) {
                    errors.push(format!("{whose} settings: {k:?} is set by its own field in this file, not settings"));
                }
                if k.is_empty() || !k.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
                    errors.push(format!("{whose} settings: {k:?} is not a Discord API field name (snake_case)"));
                }
                if let Some(name) = channel_ref(k, v)
                    && !any_channel(name)
                {
                    errors.push(format!("{whose} settings: {k} names {name:?}, which is not a channel in this spec"));
                }
            }
        };
        check_settings("server", &self.server.settings, SERVER_OWNED);
        for (_, ch) in self.channels() {
            check_settings(&format!("channel {:?}", ch.name), &ch.settings, CHANNEL_OWNED);
        }
        // A role that grants nothing and opens nothing does nothing: whoever
        // is given it would find no new channel. (Farm & Labor was one.)
        let opened: BTreeSet<&str> = self
            .categories
            .iter()
            .flat_map(|c| c.readers.iter().chain(c.channels.iter().flat_map(|ch| ch.readers.iter().flatten())))
            .map(String::as_str)
            .collect();
        for r in &self.roles {
            if r.permissions.is_empty() && !opened.contains(r.name.as_str()) {
                errors.push(format!("role {:?} grants no permission and opens no channel", r.name));
            }
        }
        if let Some(sys) = &self.server.system_channel
            && !names.contains_key(&(Kind::Text, sys.as_str()))
        {
            errors.push(format!("system_channel {sys:?} is not a text channel in this spec"));
        }
        if errors.is_empty() { Ok(()) } else { Err(errors.join("\n")) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = include_str!("../../../../../discord/server.toml");

    #[test]
    fn shipped_spec_is_valid() {
        let spec = Spec::parse(SPEC).unwrap_or_else(|e| panic!("discord/server.toml is invalid:\n{e}"));
        assert!(spec.roles.iter().any(|r| r.permissions == ["administrator"]), "someone must be able to administer");
        let webhooks: Vec<_> = spec.channels().filter(|(_, c)| c.webhook.is_some()).collect();
        assert_eq!(webhooks.len(), 1, "the core posts to exactly one feed channel");
    }

    #[test]
    fn permission_table_has_no_duplicate_bits_or_names() {
        let mut bits: Vec<u64> = PERMISSIONS.iter().map(|(_, b)| *b).collect();
        let mut names: Vec<&str> = PERMISSIONS.iter().map(|(n, _)| *n).collect();
        bits.sort_unstable();
        bits.dedup();
        names.sort_unstable();
        names.dedup();
        assert_eq!(bits.len(), PERMISSIONS.len());
        assert_eq!(names.len(), PERMISSIONS.len());
        assert!(bits.iter().all(|b| b.is_power_of_two()));
        // Spot-check against the documented values.
        assert_eq!(bit("view_channel"), Some(0x400));
        assert_eq!(bit("moderate_members"), Some(0x100_0000_0000));
        assert_eq!(bit("create_events"), Some(0x1000_0000_0000));
        assert_eq!(bit("pin_messages"), Some(0x8_0000_0000_0000));
        assert_eq!(POST & VIEW, 0);
    }

    fn with(extra: &str) -> Result<Spec, String> {
        Spec::parse(&format!(
            r##"
            [server]
            name = "t"
            verification_level = 1
            default_message_notifications = 1
            explicit_content_filter = 2
            everyone_permissions = ["view_channel"]
            [[roles]]
            name = "Crew"
            color = "#112233"
            permissions = ["create_instant_invite"]
            {extra}
            "##
        ))
    }

    #[test]
    fn rejects_mistakes_before_touching_discord() {
        let cat = |body: &str| with(&format!("[[categories]]\nname = \"C\"\n{body}"));
        let err = |r: Result<Spec, String>| r.expect_err("should be rejected");
        assert!(err(cat("readers = [\"Crwe\"]\n[[categories.channels]]\nname = \"x\"")).contains("unknown role"));
        assert!(
            err(cat("readers = [\"Crew\"]\nwriters = [\"@everyone\"]\n[[categories.channels]]\nname = \"x\""))
                .contains("writer must also be a reader")
        );
        assert!(err(cat("readers = [\"Crew\"]\n[[categories.channels]]\nname = \"Big Room\"")).contains("lowercase"));
        assert!(err(with("[[roles]]\nname = \"Crew\"\ncolor = \"#000000\"\npermissions = []\n[[categories]]\nname = \"C\"\nreaders = []\nchannels = []")).contains("duplicate role"));
        assert!(err(with("[[roles]]\nname = \"X\"\ncolor = \"red\"\npermissions = [\"fly\"]\n[[categories]]\nname = \"C\"\nreaders = []\nchannels = []")).contains("unknown permission"));
        assert!(
            err(cat("readers = [\"Crew\"]\n[[categories.channels]]\nname = \"x\"\ntopic = \"see <#nowhere>\""))
                .contains("links <#nowhere>")
        );
        let member =
            |line: &str| with(&format!("[members]\n{line}\n[[categories]]\nname = \"C\"\nreaders = []\nchannels = []"));
        assert!(member("kiki = [\"Crew\"]").is_ok());
        assert!(member("\"kiki.b_2\" = []").is_ok(), "an empty list strips every spec role");
        assert!(err(member("kiki = [\"Crwe\"]")).contains("unknown role"));
        assert!(err(member("kiki = [\"@everyone\"]")).contains("unknown role"));
        assert!(err(member("kiki = [\"Crew\", \"Crew\"]")).contains("listed twice"));
        assert!(err(member("Kiki = [\"Crew\"]")).contains("lowercase"));
        assert!(err(member("\"kiki#1234\" = []")).contains("name#1234"));
        assert!(err(member("\"Kiki Sun\" = []")).contains("lowercase"));
        assert!(err(member("\"kiki sun\" = []")).contains("display name"));
        assert!(err(member("\"a..b\" = []")).contains("not a Discord username"));
        assert!(err(member("k = []")).contains("not a Discord username"));
        let server = |settings: &str| {
            with(&format!(
                "[server.settings]\n{settings}\n[[categories]]\nname = \"C\"\nreaders = [\"Crew\"]\n\
                 [[categories.channels]]\nname = \"Den\"\nkind = \"voice\""
            ))
        };
        assert!(server("afk_timeout = 900\nafk_channel_id = \"Den\"\ndescription = \"hi\"").is_ok());
        assert!(err(server("afk_channel_id = \"Nowhere\"")).contains("not a channel"));
        assert!(err(server("name = \"x\"")).contains("its own field"));
        assert!(err(server("afkTimeout = 1")).contains("snake_case"));
        let idle = "[[roles]]\nname = \"Idle\"\ncolor = \"#000000\"\npermissions = []\n[[categories]]\nname = \"C\"\nreaders = []\nchannels = []";
        assert!(err(with(idle)).contains("role \"Idle\" grants no permission and opens no channel"));
        let long = "a".repeat(MAX_MESSAGE + 1);
        assert!(
            err(cat(&format!("readers = [\"Crew\"]\n[[categories.channels]]\nname = \"x\"\nmessage = \"{long}\"")))
                .contains("exceeds")
        );
        assert!(
            err(cat("readers = [\"Crew\"]\n[[categories.channels]]\nname = \"x\"\npinned = true"))
                .contains("pinned needs a message")
        );
    }

    #[test]
    fn channel_access_inherits_and_narrows() {
        let spec = Spec::parse(SPEC).expect("valid");
        let find = |n: &str| spec.channels().find(|(_, c)| c.name == n).expect(n);
        let (cat, ch) = find("volunteer-guide");
        let a = ch.access(cat);
        assert!(a.readers.contains("Volunteer") && !a.writers.contains("Volunteer"));
        let (cat, ch) = find("farm-feed");
        assert!(ch.access(cat).writers.is_empty(), "only the webhook posts to the feed");
        let (cat, ch) = find("Crew Huddle");
        let a = ch.access(cat);
        assert_eq!(a.readers, a.writers);
        assert!(!a.readers.contains("Community"));
        // Regression: "@everyone" readers include every role, so a named
        // writer must survive inheritance (this once computed to nobody).
        let (cat, ch) = find("announcements");
        let a = ch.access(cat);
        assert_eq!(a.writers.iter().map(String::as_str).collect::<Vec<_>>(), ["Farm Lead"]);
        let (cat, ch) = find("daily-log");
        let a = ch.access(cat);
        assert!(a.readers.contains("Board") && !a.writers.contains("Board"), "board reads ops, doesn't post");
    }
}

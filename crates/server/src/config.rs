//! Configuration: `homestead.toml` for everything, the environment for
//! secrets only.
//!
//! Each enabled integration names the secrets it needs; loading fails with
//! the full list of what's missing, so a misconfigured deploy stops at boot
//! instead of dead-lettering deliveries later.

use std::time::Duration;

use anyhow::{Context, bail};
use homestead_bus::{DispatcherConfig, RetryPolicy};
use homestead_domain::Event;
use serde::Deserialize;

pub const DEFAULT_PATH: &str = "homestead.toml";

/// Anything shorter is guessable enough to matter for an internet-facing
/// API credential.
pub const MIN_TOKEN_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub server: Server,
    pub dispatcher: Dispatcher,
    pub discord: Discord,
    pub linear: Linear,
    pub buffer: Buffer,
    pub documenso: Documenso,
    pub grants: Grants,
    pub zeffy: SpecOnly,
    pub github: SpecOnly,
    #[serde(default)]
    pub attachments: Attachments,
    #[serde(default)]
    pub stand: Stand,
}

/// The farm stand's printed sign.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stand {
    /// Scan-to-pay links shown as QR codes on the sign (Venmo, Zeffy…).
    #[serde(default)]
    pub pay_links: Vec<PayLink>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayLink {
    /// What the sign says under the code: "Venmo @sunbrewed".
    pub label: String,
    pub url: String,
}

/// Where attachment bytes are kept.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attachments {
    /// `disabled`, `filesystem` (local development) or `s3` (R2/MinIO).
    pub backend: String,
    /// `filesystem`: the directory.
    #[serde(default)]
    pub path: String,
    /// `s3`: `https://<account>.r2.cloudflarestorage.com`.
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub bucket: String,
    /// `auto` for R2.
    #[serde(default = "default_region")]
    pub region: String,
    #[serde(default = "default_max_bytes")]
    pub max_bytes: usize,
    /// How long a download link works.
    #[serde(default = "default_link_secs")]
    pub link_secs: u32,
    /// Days a deleted file's bytes are kept (restorable) before purging.
    /// At least the bucket lock's age, or purges fail until it passes.
    #[serde(default = "default_retain_deleted_days")]
    pub retain_deleted_days: i32,
    /// `s3`: the second bucket `attachments-backup` copies into (same
    /// endpoint, its own credentials). Empty: no backup configured.
    #[serde(default)]
    pub backup_bucket: String,
}

fn default_region() -> String {
    "auto".into()
}
fn default_max_bytes() -> usize {
    homestead_domain::attachments::DEFAULT_MAX_BYTES
}
fn default_link_secs() -> u32 {
    300
}
fn default_retain_deleted_days() -> i32 {
    30
}

impl Default for Attachments {
    fn default() -> Self {
        Self {
            backend: "disabled".into(),
            path: String::new(),
            endpoint: String::new(),
            bucket: String::new(),
            region: default_region(),
            max_bytes: default_max_bytes(),
            link_secs: default_link_secs(),
            retain_deleted_days: default_retain_deleted_days(),
            backup_bucket: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub bind: String,
    pub secure_cookies: bool,
    /// Where people reach the app; used to print full setup links.
    #[serde(default)]
    pub public_url: Option<String>,
    /// The farm's IANA time zone: what "today" means.
    #[serde(default = "default_timezone")]
    pub timezone: String,
}

fn default_timezone() -> String {
    "America/Los_Angeles".into()
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dispatcher {
    pub batch: i64,
    pub lease_secs: u64,
    pub delivery_timeout_secs: u64,
    pub poll_secs: u64,
    pub max_attempts: i32,
    pub backoff_base_secs: u64,
    pub backoff_cap_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Discord {
    pub enabled: bool,
    pub spec: String,
    pub application_id: String,
    pub public_key: Option<String>,
    pub events: Vec<String>,
    /// Channels whose latest posts the Community page shows. Each must be
    /// readable by the Community role in the spec (checked at startup).
    #[serde(default)]
    pub feed_channels: Vec<String>,
    /// Linear project → the channel its deadline reminders go to, posted
    /// by the bot. Other reminders go to #farm-feed with everything else.
    #[serde(default)]
    pub deadline_channels: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Linear {
    pub enabled: bool,
    pub spec: String,
    pub team: String,
    pub events: Vec<String>,
    /// How often the team's issues are pulled in as tasks while the app is
    /// awake (pages also pull when it's been longer). 0 turns pulling off.
    #[serde(default = "default_pull_secs")]
    pub pull_secs: u64,
}

fn default_pull_secs() -> u64 {
    300
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Buffer {
    pub enabled: bool,
    pub spec: String,
    pub events: Vec<String>,
    pub organization_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Documenso {
    pub enabled: bool,
    pub spec: String,
    pub base_url: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grants {
    pub enabled: bool,
    pub endpoint: String,
    pub from: String,
    pub reply_to: String,
    pub max_batch: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecOnly {
    pub spec: String,
}

/// Which event kinds each outbound integration can act on at all.
pub fn capabilities(system: &str) -> &'static [&'static str] {
    match system {
        "discord" => Event::KINDS,
        "linear" => &["task.created", "task.completed", "inventory.low_stock"],
        "buffer" => &["harvest.logged", "planting.created"],
        _ => &[],
    }
}

impl File {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let f: File = toml::from_str(text).context("homestead.toml")?;
        let mut e = Vec::new();
        for (system, events) in
            [("discord", &f.discord.events), ("linear", &f.linear.events), ("buffer", &f.buffer.events)]
        {
            for ev in events {
                if !Event::KINDS.contains(&ev.as_str()) {
                    e.push(format!("[{system}] unknown event {ev:?}; known: {}", Event::KINDS.join(", ")));
                } else if !capabilities(system).contains(&ev.as_str()) {
                    e.push(format!("[{system}] can't act on {ev:?}; it handles {}", capabilities(system).join(", ")));
                }
            }
        }
        let d = &f.dispatcher;
        if d.delivery_timeout_secs >= d.lease_secs {
            e.push("[dispatcher] delivery_timeout_secs must be shorter than lease_secs".to_owned());
        }
        if d.batch < 1 || d.max_attempts < 1 {
            e.push("[dispatcher] batch and max_attempts must be at least 1".to_owned());
        }
        if f.linear.pull_secs != 0 && !(30..=86_400).contains(&f.linear.pull_secs) {
            e.push("[linear] pull_secs must be 0 (off) or between 30 and 86400".to_owned());
        }
        if f.documenso.enabled && !f.documenso.base_url.starts_with("http") {
            e.push("[documenso] base_url must be an http(s) URL".to_owned());
        }
        if f.grants.enabled && !f.grants.endpoint.starts_with("https://") {
            e.push("[grants] endpoint must be an https URL".to_owned());
        }
        if !(1..=100).contains(&f.grants.max_batch) {
            e.push("[grants] max_batch must be between 1 and 100".to_owned());
        }
        if !f.grants.from.contains('@') || !f.grants.reply_to.contains('@') {
            e.push("[grants] from and reply_to must be email addresses".to_owned());
        }
        for l in &f.stand.pay_links {
            if !l.url.starts_with("https://") || l.label.trim().is_empty() {
                e.push(format!("[stand] pay link {:?} needs a label and an https URL", l.label));
            }
        }
        let a = &f.attachments;
        match a.backend.as_str() {
            "disabled" => {}
            "filesystem" if a.path.trim().is_empty() => e.push("[attachments] filesystem needs a path".to_owned()),
            "filesystem" => {}
            "s3" => {
                let local = a.endpoint.starts_with("http://localhost") || a.endpoint.starts_with("http://127.0.0.1");
                if !(a.endpoint.starts_with("https://") || local) {
                    e.push("[attachments] endpoint must be an https URL".to_owned());
                }
                if a.bucket.trim().is_empty() {
                    e.push("[attachments] s3 needs a bucket".to_owned());
                }
            }
            other => e.push(format!("[attachments] unknown backend {other:?}; use disabled, filesystem or s3")),
        }
        if !(1..=50 * 1024 * 1024).contains(&a.max_bytes) {
            e.push("[attachments] max_bytes must be between 1 and 50 MiB".to_owned());
        }
        if !(30..=3600).contains(&a.link_secs) {
            e.push("[attachments] link_secs must be between 30 and 3600".to_owned());
        }
        if !(0..=3650).contains(&a.retain_deleted_days) {
            e.push("[attachments] retain_deleted_days must be between 0 and 3650".to_owned());
        }
        if !a.backup_bucket.is_empty() && a.backup_bucket == a.bucket {
            e.push("[attachments] backup_bucket must be a different bucket".to_owned());
        }
        if !e.is_empty() {
            bail!("homestead.toml:\n  {}", e.join("\n  "));
        }
        Ok(f)
    }

    pub fn dispatcher(&self) -> DispatcherConfig {
        let d = &self.dispatcher;
        DispatcherConfig {
            batch: d.batch,
            lease: Duration::from_secs(d.lease_secs),
            delivery_timeout: Duration::from_secs(d.delivery_timeout_secs),
            poll_interval: Duration::from_secs(d.poll_secs),
            retry: RetryPolicy {
                max_attempts: d.max_attempts,
                base: Duration::from_secs(d.backoff_base_secs),
                cap: Duration::from_secs(d.backoff_cap_secs),
            },
        }
    }
}

/// Secrets, read from the environment.
#[derive(Debug, Clone, Default)]
pub struct Secrets {
    pub database_url: String,
    /// Legacy shared API credential; optional now that people and services
    /// have their own.
    pub admin_token: Option<String>,
    pub discord_webhook_url: Option<String>,
    /// Bot token and server id, for reading the feed channels and events
    /// and posting deadline reminders to their channels.
    pub discord_reader: Option<(String, String)>,
    /// OAuth client secret used by the public join flow to identify the
    /// signer and add that exact Discord account with its earned role.
    pub discord_client_secret: Option<String>,
    pub linear_api_key: Option<String>,
    pub buffer_api_key: Option<String>,
    pub documenso_api_token: Option<String>,
    pub documenso_webhook_secret: Option<String>,
    pub zeffy_webhook_secret: Option<String>,
    pub resend_api_key: Option<String>,
    /// Object storage credentials, when `[attachments] backend = "s3"`.
    pub attachments_key: Option<(String, String)>,
}

pub struct Config {
    pub file: File,
    pub secrets: Secrets,
    /// `BIND_ADDR` from the environment, else `[server].bind`.
    pub bind: String,
}

impl Config {
    /// Loads `HOMESTEAD_CONFIG` (default `homestead.toml`) and the secrets
    /// its enabled integrations require.
    pub fn load() -> anyhow::Result<Self> {
        let path = std::env::var("HOMESTEAD_CONFIG").unwrap_or_else(|_| DEFAULT_PATH.to_owned());
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
        Self::from_parts(&text, |k| std::env::var(k).ok())
    }

    pub fn from_parts(text: &str, env: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let file = File::parse(text)?;
        let var = |k: &str| env(k).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        let mut missing = Vec::new();
        let mut need = |k: &'static str, why: &str| {
            let v = var(k);
            if v.is_none() {
                missing.push(format!("{k} ({why})"));
            }
            v
        };
        let database_url = need("DATABASE_URL", "Postgres").unwrap_or_default();
        let admin_token = var("ADMIN_TOKEN");
        let discord_webhook_url = if file.discord.enabled {
            need("DISCORD_WEBHOOK_URL", "[discord] is enabled; run just discord-provision")
        } else {
            None
        };
        // The bot reads the feed, posts reminders, and grants the role a
        // signed welcome agreement earns (so Documenso needs it too).
        let bot_needed = !file.discord.feed_channels.is_empty()
            || !file.discord.deadline_channels.is_empty()
            || file.documenso.enabled;
        let discord_reader = if file.discord.enabled && bot_needed {
            let why = "[discord] feed_channels or deadline_channels is set, or [documenso] is (the bot grants \
                       the role a signed welcome agreement earns)";
            let token = need("DISCORD_BOT_TOKEN", why);
            let guild = need("DISCORD_GUILD_ID", why);
            token.zip(guild)
        } else {
            None
        };
        let discord_client_secret = if file.discord.enabled && file.documenso.enabled {
            need("DISCORD_CLIENT_SECRET", "[documenso] is enabled; /join identifies the signer with Discord OAuth")
        } else {
            None
        };
        let linear_api_key = if file.linear.enabled { need("LINEAR_API_KEY", "[linear] is enabled") } else { None };
        let buffer_api_key = if file.buffer.enabled { need("BUFFER_API_KEY", "[buffer] is enabled") } else { None };
        let (documenso_api_token, documenso_webhook_secret) = if file.documenso.enabled {
            (
                need("DOCUMENSO_API_TOKEN", "[documenso] is enabled"),
                need("DOCUMENSO_WEBHOOK_SECRET", "[documenso] is enabled; set the same value on Documenso's webhook"),
            )
        } else {
            (None, None)
        };
        let zeffy_webhook_secret = var("ZEFFY_WEBHOOK_SECRET");
        let resend_api_key = if file.grants.enabled { need("RESEND_API_KEY", "[grants] is enabled") } else { None };
        let attachments_key = if file.attachments.backend == "s3" {
            let id = need("ATTACHMENTS_ACCESS_KEY_ID", "[attachments] backend is s3 (an R2 API token's key id)");
            let secret = need("ATTACHMENTS_SECRET_ACCESS_KEY", "[attachments] backend is s3");
            id.zip(secret)
        } else {
            None
        };
        if !missing.is_empty() {
            bail!("missing environment variables:\n  {}", missing.join("\n  "));
        }
        if admin_token.as_ref().is_some_and(|t| t.len() < MIN_TOKEN_LEN) {
            bail!("ADMIN_TOKEN must be at least {MIN_TOKEN_LEN} characters (or unset)");
        }
        let bind = var("BIND_ADDR").unwrap_or_else(|| file.server.bind.clone());
        Ok(Self {
            file,
            secrets: Secrets {
                database_url,
                admin_token,
                discord_webhook_url,
                discord_reader,
                discord_client_secret,
                linear_api_key,
                buffer_api_key,
                documenso_api_token,
                documenso_webhook_secret,
                zeffy_webhook_secret,
                resend_api_key,
                attachments_key,
            },
            bind,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIPPED: &str = include_str!("../../../homestead.toml");

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: std::collections::BTreeMap<String, String> =
            pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        move |k| map.get(k).cloned()
    }

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    /// `Config` has no `Debug` (it holds secrets), so no `expect_err`.
    fn load_err(text: &str, e: impl Fn(&str) -> Option<String>) -> String {
        let Err(err) = Config::from_parts(text, e) else { panic!("expected a configuration error") };
        err.to_string()
    }

    fn full_env() -> Vec<(&'static str, &'static str)> {
        vec![
            ("DATABASE_URL", "postgres://x"),
            ("ADMIN_TOKEN", TOKEN),
            ("DISCORD_WEBHOOK_URL", "https://discord.test/hook"),
            ("DISCORD_BOT_TOKEN", "bot"),
            ("DISCORD_CLIENT_SECRET", "oauth-secret"),
            ("DISCORD_GUILD_ID", "1554027507932598345"),
            ("LINEAR_API_KEY", "lin"),
            ("BUFFER_API_KEY", "buf"),
            ("DOCUMENSO_API_TOKEN", "api_x"),
            ("DOCUMENSO_WEBHOOK_SECRET", "whsec"),
            ("ATTACHMENTS_ACCESS_KEY_ID", "id"),
            ("ATTACHMENTS_SECRET_ACCESS_KEY", "secret"),
        ]
    }

    #[test]
    fn shipped_config_is_valid_and_every_spec_file_exists() {
        let f = File::parse(SHIPPED).expect("homestead.toml");
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../");
        for spec in [&f.discord.spec, &f.linear.spec, &f.buffer.spec, &f.documenso.spec, &f.zeffy.spec, &f.github.spec]
        {
            assert!(std::path::Path::new(&format!("{root}{spec}")).is_file(), "{spec} is referenced but missing");
        }
    }

    /// Grant email must come from the org's own domain (the one its website
    /// and GitHub org list); Resend refuses unverified domains, and a typo'd
    /// one (`sunbrewed.org` once shipped) has no DNS at all.
    #[test]
    fn grant_mail_is_sent_from_the_org_domain() {
        let f = File::parse(SHIPPED).expect("homestead.toml");
        let org = include_str!("../../../github/org.toml");
        let site = org
            .lines()
            .find_map(|l| l.strip_prefix("blog = \"https://"))
            .and_then(|l| l.strip_suffix('"'))
            .expect("github/org.toml names the website");
        let domain = f.grants.from.rsplit_once('@').map(|(_, d)| d.trim_end_matches('>')).expect("from has a domain");
        assert_eq!(domain, site, "[grants] from");
    }

    #[test]
    fn stand_pay_links_must_be_https() {
        let with = |link: &str| SHIPPED.replace("[stand]\npay_links = []", &format!("[stand]\npay_links = [{link}]"));
        assert!(SHIPPED.contains("[stand]\npay_links = []"), "the shipped file has an empty [stand] section");
        let ok = File::parse(&with(r#"{ label = "Venmo", url = "https://venmo.com/u/example" }"#)).expect("ok");
        assert_eq!(ok.stand.pay_links.len(), 1);
        assert!(File::parse(&with(r#"{ label = "Venmo", url = "http://venmo.com/u/example" }"#)).is_err());
        assert!(File::parse(&with(r#"{ label = " ", url = "https://venmo.com/u/example" }"#)).is_err());
    }

    #[test]
    fn missing_secrets_are_all_named_at_once() {
        // Every integration on, whatever the shipped file currently enables.
        let all_on = SHIPPED.replace("enabled = false", "enabled = true");
        let err = load_err(&all_on, env(&[("DATABASE_URL", "postgres://x")]));
        for k in [
            "DISCORD_WEBHOOK_URL",
            "DISCORD_BOT_TOKEN",
            "DISCORD_GUILD_ID",
            "LINEAR_API_KEY",
            "BUFFER_API_KEY",
            "DOCUMENSO_API_TOKEN",
            "DOCUMENSO_WEBHOOK_SECRET",
            "RESEND_API_KEY",
        ] {
            assert!(err.contains(k), "{k} not reported:\n{err}");
        }
    }

    /// Without the bot, a signed welcome agreement would grant nothing,
    /// silently: the role sink is only built when the bot is.
    #[test]
    fn onboarding_needs_the_bot_even_without_feeds_or_reminders() {
        let quiet = SHIPPED
            .replace(r#"feed_channels = ["announcements", "events", "general"]"#, "feed_channels = []")
            .replace(r#"deadline_channels = { "Grant writing" = "grant-deadlines" }"#, "deadline_channels = {}");
        assert_ne!(quiet, SHIPPED, "the shipped file sets both");
        let mut e = full_env();
        e.retain(|(k, _)| *k != "DISCORD_BOT_TOKEN");
        assert!(load_err(&quiet, env(&e)).contains("DISCORD_BOT_TOKEN"));
        let no_releases = quiet.replace("[documenso]\nenabled = true", "[documenso]\nenabled = false");
        assert_ne!(no_releases, quiet);
        assert!(Config::from_parts(&no_releases, env(&e)).is_ok(), "no releases, no bot needed");
    }

    #[test]
    fn disabling_an_integration_drops_its_secret_requirement() {
        let text = SHIPPED.replace("[buffer]\nenabled = true", "[buffer]\nenabled = false");
        let mut e = full_env();
        e.retain(|(k, _)| *k != "BUFFER_API_KEY");
        let c = Config::from_parts(&text, env(&e)).expect("buffer off needs no key");
        assert!(c.secrets.buffer_api_key.is_none());
    }

    #[test]
    fn blank_secrets_count_as_missing_and_bind_can_be_overridden() {
        let mut e = full_env();
        e.push(("LINEAR_API_KEY", "   "));
        e.retain(|(k, v)| *k != "LINEAR_API_KEY" || v.trim().is_empty());
        assert!(load_err(SHIPPED, env(&e)).contains("LINEAR_API_KEY"));
        let mut e = full_env();
        e.push(("BIND_ADDR", "0.0.0.0:8080"));
        assert_eq!(Config::from_parts(SHIPPED, env(&e)).expect("ok").bind, "0.0.0.0:8080");
        assert_eq!(Config::from_parts(SHIPPED, env(&full_env())).expect("ok").bind, "127.0.0.1:8080");
    }

    #[test]
    fn events_are_checked_against_what_each_integration_can_do() {
        let typo =
            SHIPPED.replace("events = [\"harvest.logged\", \"planting.created\"]", "events = [\"harvest.loged\"]");
        assert!(File::parse(&typo).expect_err("typo").to_string().contains("unknown event"));
        let wrong =
            SHIPPED.replace("events = [\"harvest.logged\", \"planting.created\"]", "events = [\"task.created\"]");
        assert!(File::parse(&wrong).expect_err("capability").to_string().contains("can't act on"));
    }

    #[test]
    fn dispatcher_settings_are_sanity_checked() {
        let bad = SHIPPED.replace("delivery_timeout_secs = 30", "delivery_timeout_secs = 500");
        assert!(File::parse(&bad).expect_err("timeout").to_string().contains("shorter than lease"));
        let f = File::parse(SHIPPED).expect("ok").dispatcher();
        assert_eq!((f.retry.max_attempts, f.lease.as_secs()), (12, 120));
    }

    #[test]
    fn retention_is_bounded_and_the_backup_is_a_different_bucket() {
        let err = |text: &str| File::parse(text).expect_err("invalid").to_string();
        let forever = SHIPPED.replace("retain_deleted_days = 30", "retain_deleted_days = 99999");
        assert!(err(&forever).contains("retain_deleted_days"));
        let same = SHIPPED
            .replace("backup_bucket = \"homestead-attachments-backup\"", "backup_bucket = \"homestead-attachments\"");
        assert!(err(&same).contains("backup_bucket must be a different bucket"), "{}", err(&same));
    }

    #[test]
    fn attachment_storage_is_checked_and_needs_its_keys() {
        let without_keys: Vec<_> = full_env().into_iter().filter(|(k, _)| !k.starts_with("ATTACHMENTS_")).collect();
        assert!(Config::from_parts(SHIPPED, env(&without_keys)).is_ok(), "disabled storage needs no keys");
        let s3 = SHIPPED.replace("backend = \"disabled\"", "backend = \"s3\"");
        let err = load_err(&s3, env(&without_keys));
        assert!(err.contains("ATTACHMENTS_ACCESS_KEY_ID") && err.contains("ATTACHMENTS_SECRET_ACCESS_KEY"), "{err}");
        assert!(Config::from_parts(&s3, env(&full_env())).expect("ok").secrets.attachments_key.is_some());
        let plain = s3.replace("https://905346f3b99c907683ddf08ee4886f56", "http://acct");
        assert!(File::parse(&plain).expect_err("http").to_string().contains("https"));
        let typo = SHIPPED.replace("backend = \"disabled\"", "backend = \"r2\"");
        assert!(File::parse(&typo).expect_err("backend").to_string().contains("unknown backend"));
    }

    /// The Community page shows these to everyone who can open it, so a
    /// private channel listed here would leak.
    #[test]
    fn shipped_discord_feed_is_readable_by_the_whole_community() {
        use homestead_integrations::discord::{feed, provision::Spec};
        let f = File::parse(SHIPPED).expect("ok");
        assert!(!f.discord.feed_channels.is_empty());
        let spec = Spec::parse(include_str!("../../../discord/server.toml")).expect("spec");
        feed::check_channels(&spec, &f.discord.feed_channels).expect("feed channels");
    }

    /// Reminders carry internal task titles, so their channels must be
    /// ones the whole community can't read.
    #[test]
    fn shipped_deadline_channels_are_for_the_team() {
        use homestead_integrations::discord::{check_deadline_channels, provision::Spec};
        let f = File::parse(SHIPPED).expect("ok");
        assert_eq!(f.discord.deadline_channels.get("Grant writing").map(String::as_str), Some("grant-deadlines"));
        assert!(f.discord.events.iter().any(|e| e == "task.deadlines"), "the rest go to #farm-feed");
        let spec = Spec::parse(include_str!("../../../discord/server.toml")).expect("spec");
        check_deadline_channels(&spec, f.discord.deadline_channels.values()).expect("deadline channels");
        let public = check_deadline_channels(&spec, &["general".to_owned()]).expect_err("public");
        assert!(public.contains("readable by Community"), "{public}");
    }

    /// `/onboard` hands out roles with no steward involved, so each must
    /// exist, open something a newcomer can't already see, and open no
    /// team channel (ops, leadership, the core's feed).
    #[test]
    fn self_serve_roles_open_community_channels_but_not_the_teams() {
        use homestead_domain::onboarding::OnboardingRole;
        use homestead_integrations::discord::provision::Spec;
        let spec = Spec::parse(include_str!("../../../discord/server.toml")).expect("spec");
        let reads = |role: &str| -> Vec<&str> {
            spec.channels().filter(|(cat, ch)| ch.access(cat).can_read(role)).map(|(_, ch)| ch.name.as_str()).collect()
        };
        let newcomer = reads("nobody holds this role");
        for role in OnboardingRole::ALL {
            assert!(spec.roles.iter().any(|r| r.name == role.as_str()), "{role} isn't in discord/server.toml");
            let can = reads(role.as_str());
            assert!(can.len() > newcomer.len(), "{role} opens nothing new");
            for team in ["daily-log", "tasks", "farm-feed", "leadership", "finance", "fundraising"] {
                assert!(!can.contains(&team), "{role} may read #{team}");
            }
        }
    }

    #[test]
    fn linear_pull_interval_is_bounded_and_can_be_off() {
        assert_eq!(File::parse(SHIPPED).expect("ok").linear.pull_secs, 300);
        let with = |v: &str| SHIPPED.replace("pull_secs = 300", &format!("pull_secs = {v}"));
        assert_eq!(File::parse(&with("0")).expect("off").linear.pull_secs, 0);
        assert!(File::parse(&with("5")).expect_err("hammering").to_string().contains("pull_secs"));
    }

    #[test]
    fn short_admin_token_is_refused() {
        let mut e = full_env();
        e.retain(|(k, _)| *k != "ADMIN_TOKEN");
        e.push(("ADMIN_TOKEN", "short"));
        assert!(load_err(SHIPPED, env(&e)).contains("at least"));
    }
}

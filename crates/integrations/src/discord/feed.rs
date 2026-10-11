//! Reading the Discord server back: recent posts in a few channels and the
//! scheduled events, for the Community page.
//!
//! Anyone who can open that page sees these, so only channels the spec lets
//! the base `Community` role read may be listed ([`check_channels`]). Private
//! channels (leadership, finance) can't leak into the app by a config edit.

use std::collections::HashMap;

use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::provision::Spec;

pub const API: &str = "https://discord.com/api/v10";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthUser {
    pub id: String,
    pub username: String,
}

/// An identified joiner and the OAuth token that lets the bot add them.
/// No `Debug`, so the token can't end up in a log line.
pub struct OAuthGrant {
    pub user: OAuthUser,
    access_token: String,
}

/// The role every member of the community holds.
pub const BASELINE_ROLE: &str = "Community";

/// How much of a post the page shows.
const EXCERPT: usize = 280;

/// Refuses feed channels that aren't in the spec, or that the baseline role
/// can't read there.
pub fn check_channels(spec: &Spec, names: &[String]) -> Result<(), String> {
    let mut bad = Vec::new();
    for name in names {
        match spec.channels().find(|(_, ch)| &ch.name == name) {
            None => bad.push(format!("#{name} is not in the Discord spec")),
            Some((cat, ch)) if !ch.access(cat).can_read(BASELINE_ROLE) => {
                bad.push(format!("#{name} is not readable by {BASELINE_ROLE}; the app would show it to everyone"))
            }
            Some(_) => {}
        }
    }
    if bad.is_empty() { Ok(()) } else { Err(bad.join("; ")) }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    pub channel: String,
    pub author: String,
    pub excerpt: String,
    pub at: OffsetDateTime,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledEvent {
    pub name: String,
    pub description: String,
    pub starts_at: OffsetDateTime,
    /// Where: a place for an external event, or the voice/stage channel.
    pub place: String,
    pub interested: Option<u64>,
    pub url: String,
}

/// What the Community page shows from Discord.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    /// Soonest first.
    pub events: Vec<ScheduledEvent>,
    /// Newest first, across the feed channels.
    pub posts: Vec<Post>,
    /// Feed channels that exist but have nothing to show.
    pub quiet: Vec<String>,
}

pub struct Reader {
    http: reqwest::Client,
    base: String,
    token: String,
    guild_id: String,
}

impl Reader {
    /// Exchange a signed joiner's OAuth code and identify that exact
    /// Discord account. Nothing changes in the guild yet: the caller claims
    /// the account for the join first, then calls [`Reader::add_joiner`].
    pub async fn oauth_identify(
        &self,
        application_id: &str,
        client_secret: &str,
        redirect_uri: &str,
        code: &str,
    ) -> Result<OAuthGrant, String> {
        let token_body = form_urlencoded::Serializer::new(String::new())
            .append_pair("client_id", application_id)
            .append_pair("client_secret", client_secret)
            .append_pair("grant_type", "authorization_code")
            .append_pair("code", code)
            .append_pair("redirect_uri", redirect_uri)
            .finish();
        let token = self
            .http
            .post(format!("{}/oauth2/token", self.base))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(token_body)
            .send()
            .await
            .map_err(|e| format!("Discord OAuth: {e}"))?;
        let status = token.status();
        let body = token.text().await.map_err(|e| format!("Discord OAuth: {e}"))?;
        if !status.is_success() {
            return Err(format!("Discord OAuth HTTP {status}: {}", body.chars().take(200).collect::<String>()));
        }
        let token: Value = serde_json::from_str(&body).map_err(|e| format!("Discord OAuth: unreadable token ({e})"))?;
        let access =
            token["access_token"].as_str().ok_or_else(|| "Discord OAuth returned no access token".to_owned())?;
        let me = self
            .http
            .get(format!("{}/users/@me", self.base))
            .bearer_auth(access)
            .send()
            .await
            .map_err(|e| format!("Discord OAuth: {e}"))?;
        let status = me.status();
        let body = me.text().await.map_err(|e| format!("Discord OAuth: {e}"))?;
        if !status.is_success() {
            return Err(format!("Discord OAuth user HTTP {status}: {}", body.chars().take(200).collect::<String>()));
        }
        let me: Value = serde_json::from_str(&body).map_err(|e| format!("Discord OAuth: unreadable user ({e})"))?;
        let user = OAuthUser {
            id: me["id"].as_str().ok_or_else(|| "Discord returned no user id".to_owned())?.to_owned(),
            username: me["username"].as_str().ok_or_else(|| "Discord returned no username".to_owned())?.to_owned(),
        };
        Ok(OAuthGrant { user, access_token: access.to_owned() })
    }

    /// Add an identified joiner to this guild and grant the named role. The
    /// explicit role call also covers someone who was already in the guild.
    pub async fn add_joiner(&self, grant: &OAuthGrant, role: &str) -> Result<(), String> {
        let user = &grant.user;
        let roles = self.get(&format!("/guilds/{}/roles", self.guild_id)).await?;
        let role_id = roles
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| r["name"] == role)
            .and_then(|r| r["id"].as_str())
            .ok_or_else(|| format!("Discord has no {role:?} role"))?
            .to_owned();
        let add = self
            .http
            .put(format!("{}/guilds/{}/members/{}", self.base, self.guild_id, user.id))
            .header("Authorization", format!("Bot {}", self.token))
            .json(&serde_json::json!({ "access_token": grant.access_token, "roles": [&role_id] }))
            .send()
            .await
            .map_err(|e| format!("Discord: {e}"))?;
        if !add.status().is_success() {
            return Err(format!("Discord add member HTTP {}: {}", add.status(), add.text().await.unwrap_or_default()));
        }
        let grant = self
            .http
            .put(format!("{}/guilds/{}/members/{}/roles/{role_id}", self.base, self.guild_id, user.id))
            .header("Authorization", format!("Bot {}", self.token))
            .send()
            .await
            .map_err(|e| format!("Discord: {e}"))?;
        if !grant.status().is_success() {
            return Err(format!(
                "Discord role grant HTTP {}: {}",
                grant.status(),
                grant.text().await.unwrap_or_default()
            ));
        }
        Ok(())
    }

    pub fn new(http: reqwest::Client, base: Option<String>, token: String, guild_id: String) -> Self {
        Self { http, base: base.unwrap_or_else(|| API.to_owned()), token, guild_id }
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        let resp = self
            .http
            .get(format!("{}{path}", self.base))
            .header("Authorization", format!("Bot {}", self.token))
            .send()
            .await
            .map_err(|e| format!("Discord: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| format!("Discord: {e}"))?;
        if !status.is_success() {
            return Err(format!("Discord HTTP {status}: {}", text.chars().take(200).collect::<String>()));
        }
        serde_json::from_str(&text).map_err(|e| format!("Discord: unreadable response ({e})"))
    }

    /// The latest `per_channel` posts in each named channel, and the
    /// upcoming scheduled events.
    pub async fn snapshot(&self, channels: &[String], per_channel: u8) -> Result<Snapshot, String> {
        let g = &self.guild_id;
        let listed = self.get(&format!("/guilds/{g}/channels")).await?;
        let names: HashMap<String, String> = listed
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| Some((c["id"].as_str()?.to_owned(), c["name"].as_str()?.to_owned())))
            .collect();
        let mut snap = Snapshot::default();
        for want in channels {
            let Some(id) = names.iter().find(|(_, n)| *n == want).map(|(id, _)| id.clone()) else {
                return Err(format!("Discord has no #{want} channel"));
            };
            let messages = self.get(&format!("/channels/{id}/messages?limit={per_channel}")).await?;
            let before = snap.posts.len();
            for m in messages.as_array().into_iter().flatten() {
                if let Some(p) = post(m, want, g, &id, &names) {
                    snap.posts.push(p);
                }
            }
            if snap.posts.len() == before {
                snap.quiet.push(want.clone());
            }
        }
        snap.posts.sort_by_key(|p| std::cmp::Reverse(p.at));
        let events = self.get(&format!("/guilds/{g}/scheduled-events?with_user_count=true")).await?;
        snap.events = events.as_array().into_iter().flatten().filter_map(|e| event(e, g, &names)).collect();
        snap.events.sort_by_key(|e| e.starts_at);
        Ok(snap)
    }
}

/// A person in the server and the roles they hold, by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleHolder {
    pub user_id: String,
    pub username: String,
    pub roles: Vec<String>,
}

impl Reader {
    /// Everyone (bots aside) who holds at least one role. Needs the Server
    /// Members intent, which the provisioner already relies on.
    pub async fn role_holders(&self) -> Result<Vec<RoleHolder>, String> {
        let g = &self.guild_id;
        let roles = self.get(&format!("/guilds/{g}/roles")).await?;
        let names: HashMap<&str, &str> = roles
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| Some((r["id"].as_str()?, r["name"].as_str()?)))
            .collect();
        let mut out = Vec::new();
        let mut after = "0".to_owned();
        loop {
            let page = self.get(&format!("/guilds/{g}/members?limit=1000&after={after}")).await?;
            let page = page.as_array().cloned().unwrap_or_default();
            for m in &page {
                let user = &m["user"];
                let held: Vec<String> = m["roles"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|id| names.get(id.as_str()?).map(|n| (*n).to_owned()))
                    .collect();
                if user["bot"].as_bool() != Some(true) && !held.is_empty() {
                    out.push(RoleHolder {
                        user_id: user["id"].as_str().unwrap_or_default().to_owned(),
                        username: user["username"].as_str().unwrap_or_default().to_owned(),
                        roles: held,
                    });
                }
            }
            match page.last().and_then(|m| m["user"]["id"].as_str()) {
                Some(last) if page.len() == 1000 => after = last.to_owned(),
                _ => return Ok(out),
            }
        }
    }
}

/// A message worth showing: something a person wrote (not a join notice or
/// a pin), with text or an attachment.
fn post(m: &Value, channel: &str, guild: &str, channel_id: &str, names: &HashMap<String, String>) -> Option<Post> {
    if !matches!(m["type"].as_u64(), Some(0 | 19)) {
        return None;
    }
    let at = OffsetDateTime::parse(m["timestamp"].as_str()?, &Rfc3339).ok()?;
    let mut text = readable(m["content"].as_str().unwrap_or_default(), m, names);
    if text.is_empty() {
        // An embed (the provisioner's welcome posts) or a photo.
        text = m["embeds"][0]["title"].as_str().map(str::to_owned).unwrap_or_default();
    }
    let files = m["attachments"].as_array().map_or(0, Vec::len);
    if text.is_empty() && files == 0 {
        return None;
    }
    if files > 0 {
        let label = if files == 1 { "📎 1 attachment".to_owned() } else { format!("📎 {files} attachments") };
        text = if text.is_empty() { label } else { format!("{text} · {label}") };
    }
    let author = &m["author"];
    let name = ["global_name", "username"].into_iter().find_map(|k| author[k].as_str().filter(|s| !s.is_empty()));
    Some(Post {
        channel: channel.to_owned(),
        author: name.unwrap_or("someone").to_owned(),
        excerpt: excerpt(&text),
        at,
        url: format!("https://discord.com/channels/{guild}/{channel_id}/{}", m["id"].as_str()?),
    })
}

/// Discord's `<@id>`, `<#id>` and `<:emoji:id>` markup, as people read it.
fn readable(content: &str, m: &Value, channels: &HashMap<String, String>) -> String {
    let users: HashMap<&str, &str> = m["mentions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|u| {
            let name = u["global_name"].as_str().filter(|s| !s.is_empty()).or(u["username"].as_str())?;
            Some((u["id"].as_str()?, name))
        })
        .collect();
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(end) = tail.find('>') else {
            out.push_str(tail);
            return out.trim().to_owned();
        };
        let inner = &tail[1..end];
        let id = |p: &str| inner.strip_prefix(p).filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()));
        if let Some(id) = id("@!").or_else(|| id("@")) {
            out.push('@');
            out.push_str(users.get(id).copied().unwrap_or("someone"));
        } else if let Some(id) = id("#") {
            out.push('#');
            out.push_str(channels.get(id).map_or("channel", String::as_str));
        } else if let Some(name) =
            inner.strip_prefix(':').or_else(|| inner.strip_prefix("a:")).and_then(|e| e.split(':').next())
        {
            out.push(':');
            out.push_str(name);
            out.push(':');
        } else {
            out.push_str(&tail[..=end]);
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out.trim().to_owned()
}

fn excerpt(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= EXCERPT {
        one_line
    } else {
        one_line.chars().take(EXCERPT - 1).chain(std::iter::once('…')).collect()
    }
}

/// A scheduled or running event (status 1 or 2).
fn event(e: &Value, guild: &str, channels: &HashMap<String, String>) -> Option<ScheduledEvent> {
    if !matches!(e["status"].as_u64(), Some(1 | 2)) {
        return None;
    }
    let place = match e["entity_metadata"]["location"].as_str() {
        Some(loc) => loc.to_owned(),
        None => e["channel_id"].as_str().and_then(|id| channels.get(id)).map(|n| format!("🔊 {n}")).unwrap_or_default(),
    };
    Some(ScheduledEvent {
        name: e["name"].as_str()?.to_owned(),
        description: excerpt(e["description"].as_str().unwrap_or_default()),
        starts_at: OffsetDateTime::parse(e["scheduled_start_time"].as_str()?, &Rfc3339).ok()?,
        place,
        interested: e["user_count"].as_u64(),
        url: format!("https://discord.com/events/{guild}/{}", e["id"].as_str()?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SPEC: &str = include_str!("../../../../discord/server.toml");

    fn names() -> HashMap<String, String> {
        HashMap::from([("42".to_owned(), "farm-stand".to_owned()), ("7".to_owned(), "Farmhouse".to_owned())])
    }

    #[test]
    fn only_channels_the_whole_community_reads_can_be_shown() {
        let spec = Spec::parse(SPEC).expect("spec");
        let ok = ["announcements", "events", "general"].map(String::from);
        assert_eq!(check_channels(&spec, &ok), Ok(()));
        for private in ["leadership", "finance", "volunteer-chat"] {
            let err = check_channels(&spec, &[private.to_owned()]).expect_err(private);
            assert!(err.contains("not readable by Community"), "{err}");
        }
        assert!(check_channels(&spec, &["nope".to_owned()]).expect_err("typo").contains("not in the Discord spec"));
    }

    #[test]
    fn join_notices_are_skipped_and_people_are_named() {
        // A member-join system message, verbatim shape from #general (2026-09-28).
        let join = json!({ "id": "1", "type": 7, "content": "", "timestamp": "2026-09-28T07:24:38.365000+00:00",
                           "author": { "username": "Sunbrewed Homestead", "global_name": null, "bot": true },
                           "mentions": [], "attachments": [], "embeds": [] });
        assert_eq!(post(&join, "general", "g", "c", &names()), None);

        let said = json!({ "id": "99", "type": 0, "timestamp": "2026-09-28T18:00:00.000000+00:00",
            "content": "Eggs are in <#42>! Thanks <@5> <:sun:123>",
            "author": { "username": "photomancer", "global_name": "Photo" },
            "mentions": [{ "id": "5", "username": "ava", "global_name": "" }], "attachments": [{}], "embeds": [] });
        let p = post(&said, "general", "g", "c", &names()).expect("a post");
        assert_eq!(p.author, "Photo");
        assert_eq!(p.excerpt, "Eggs are in #farm-stand! Thanks @ava :sun: · 📎 1 attachment");
        assert_eq!(p.url, "https://discord.com/channels/g/c/99");
    }

    #[test]
    fn unknown_markup_survives_and_long_posts_are_cut() {
        let m = json!({ "mentions": [] });
        assert_eq!(readable("a <b> <@1> <#9> <", &m, &names()), "a <b> @someone #channel <");
        let long = "word ".repeat(100);
        let cut = excerpt(&long);
        assert_eq!(cut.chars().count(), EXCERPT);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn only_upcoming_events_are_listed_with_their_place() {
        let at = "2026-10-04T20:00:00+00:00";
        let e = |status: u64, meta: Value, channel: Value| {
            json!({ "id": "e1", "name": "Work day", "description": "Coop build", "status": status,
                    "scheduled_start_time": at, "entity_metadata": meta, "channel_id": channel, "user_count": 4 })
        };
        let outside = event(&e(1, json!({ "location": "The homestead" }), Value::Null), "g", &names()).expect("event");
        assert_eq!((outside.place.as_str(), outside.interested), ("The homestead", Some(4)));
        assert_eq!(outside.url, "https://discord.com/events/g/e1");
        let voice = event(&e(2, Value::Null, json!("7")), "g", &names()).expect("live");
        assert_eq!(voice.place, "🔊 Farmhouse");
        assert_eq!(event(&e(3, Value::Null, Value::Null), "g", &names()), None, "finished");
    }
}

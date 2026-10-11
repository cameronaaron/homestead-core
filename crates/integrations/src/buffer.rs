//! Buffer: spec verification (`buffer/account.toml`), the drafts sink, and
//! a read of what's queued and posted ([`Client::social`]) for the app.
//!
//! Buffer's GraphQL API can create posts and ideas and read accounts and
//! channels, but it can't connect a social account or rename the
//! organization. So the spec is verify-only, and the integration only ever
//! creates *Ideas* (drafts a person finishes and approves), never posts.

use homestead_bus::{BoxFuture, DeliveryError, Sink};
use homestead_domain::Event;
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::Finding;

pub const ENDPOINT: &str = "https://api.buffer.com";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub account: AccountSpec,
    #[serde(default)]
    pub channels: Vec<ChannelSpec>,
    pub drafts: DraftSpec,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountSpec {
    pub organization: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelSpec {
    pub service: String,
    pub handle: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftSpec {
    pub hashtags: Vec<String>,
}

/// Channel services Buffer supports (from its docs).
const SERVICES: &[&str] = &[
    "instagram",
    "facebook",
    "linkedin",
    "tiktok",
    "twitter",
    "threads",
    "bluesky",
    "pinterest",
    "youtube",
    "googlebusiness",
    "mastodon",
];

impl Spec {
    pub fn parse(text: &str) -> Result<Self, String> {
        let spec: Spec = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut e = Vec::new();
        for c in &spec.channels {
            if !SERVICES.contains(&c.service.as_str()) {
                e.push(format!("unknown Buffer service {:?} (one of {SERVICES:?})", c.service));
            }
        }
        if spec.channels.len() > 3 {
            e.push("Buffer's free plan connects at most 3 channels".to_owned());
        }
        for h in &spec.drafts.hashtags {
            if !h.starts_with('#') || h.contains(' ') {
                e.push(format!("hashtag {h:?} must start with # and contain no spaces"));
            }
        }
        if e.is_empty() { Ok(spec) } else { Err(e.join("\n")) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveOrg {
    pub id: String,
    pub name: String,
    pub channels: Vec<LiveChannel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveChannel {
    pub service: String,
    pub name: String,
    pub paused: bool,
}

pub fn verify(spec: &Spec, org: &LiveOrg) -> Vec<Finding> {
    let mut out = Vec::new();
    if org.name == spec.account.organization {
        out.push(Finding::Note(format!("organization is {:?}", org.name)));
    } else {
        out.push(Finding::Pending(format!(
            "rename the Buffer organization from {:?} to {:?} (Buffer → Settings → Organization)",
            org.name, spec.account.organization
        )));
    }
    for want in &spec.channels {
        match org.channels.iter().find(|c| c.service.eq_ignore_ascii_case(&want.service)) {
            None => out.push(Finding::Pending(format!(
                "connect {} (@{}) in Buffer → Channels → Connect",
                want.service, want.handle
            ))),
            Some(c) if c.paused => {
                out.push(Finding::Pending(format!("{} ({}) is connected but its queue is paused", c.service, c.name)))
            }
            Some(c) => out.push(Finding::Note(format!("{} connected as {}", c.service, c.name))),
        }
    }
    out
}

pub struct Client {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl Client {
    pub fn new(http: reqwest::Client, endpoint: Option<String>, api_key: String) -> Self {
        Self { http, endpoint: endpoint.unwrap_or_else(|| ENDPOINT.to_owned()), api_key }
    }

    /// Returns `data`, or `Err((retryable, message))`.
    async fn gql(&self, query: &str, variables: Value) -> Result<Value, (bool, String)> {
        let resp = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await
            .map_err(|e| (true, format!("Buffer: {e}")))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if status.as_u16() == 429 || status.is_server_error() {
            return Err((true, format!("Buffer HTTP {status}")));
        }
        let v: Value = serde_json::from_str(&text)
            .map_err(|_| (false, format!("Buffer HTTP {status}: {}", text.chars().take(200).collect::<String>())))?;
        if let Some(errors) = v["errors"].as_array() {
            let msg: Vec<&str> = errors.iter().filter_map(|e| e["message"].as_str()).collect();
            return Err((false, format!("Buffer: {}", msg.join("; "))));
        }
        Ok(v["data"].clone())
    }

    /// The account's first organization with its channels.
    pub async fn organization(&self) -> Result<LiveOrg, String> {
        let d = self.gql("{ account { organizations { id name } } }", json!({})).await.map_err(|e| e.1)?;
        let org = d["account"]["organizations"]
            .as_array()
            .and_then(|a| a.first())
            .ok_or("this Buffer account has no organization")?;
        let id = org["id"].as_str().unwrap_or_default().to_owned();
        let c = self
            .gql(
                "query($org: OrganizationId!) { channels(input: { organizationId: $org }) { name displayName service isQueuePaused } }",
                json!({ "org": id }),
            )
            .await
            .map_err(|e| e.1)?;
        let channels = c["channels"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|ch| LiveChannel {
                service: ch["service"].as_str().unwrap_or_default().to_owned(),
                name: ch["displayName"].as_str().or(ch["name"].as_str()).unwrap_or_default().to_owned(),
                paused: ch["isQueuePaused"].as_bool().unwrap_or(false),
            })
            .collect();
        Ok(LiveOrg { id, name: org["name"].as_str().unwrap_or_default().to_owned(), channels })
    }

    /// Connected channels, recent and queued posts, and the drafts
    /// Homestead made that still wait for a person.
    pub async fn social(&self, organization_id: &str) -> Result<Social, String> {
        let org = self.organization().await?;
        let d = self
            .gql(
                "query($org: OrganizationId!) {
                   posts(first: 20, input: { organizationId: $org,
                         filter: { status: [scheduled, sending, sent, error, needs_approval] } }) {
                     edges { node { id text status dueAt sentAt externalLink channelService
                                    channel { displayName name } error { message } } } }
                   ideas(first: 50, input: { organizationId: $org }) {
                     edges { node { id content { title } createdAt } } }
                 }",
                json!({ "org": organization_id }),
            )
            .await
            .map_err(|e| e.1)?;
        Ok(social_from(org.channels, &d))
    }

    pub async fn create_idea(&self, organization_id: &str, title: &str, text: &str) -> Result<String, DeliveryError> {
        let d = self
            .gql(
                "mutation($input: CreateIdeaInput!) { createIdea(input: $input) {
                   __typename
                   ... on Idea { id }
                   ... on InvalidInputError { message }
                   ... on UnauthorizedError { message }
                   ... on UnexpectedError { message }
                   ... on LimitReachedError { message }
                 } }",
                json!({ "input": { "organizationId": organization_id, "content": { "title": title, "text": text } } }),
            )
            .await
            .map_err(|(retry, m)| if retry { DeliveryError::Retryable(m) } else { DeliveryError::Permanent(m) })?;
        interpret_idea(&d["createIdea"])
    }
}

/// What Buffer shows of the org's social media.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Social {
    pub channels: Vec<LiveChannel>,
    /// Queued ones first (soonest first), then sent (newest first).
    pub posts: Vec<SocialPost>,
    /// Ideas Homestead drafted ([`DRAFT_MARK`]), newest first.
    pub drafts: Vec<DraftIdea>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocialPost {
    pub service: String,
    pub channel: String,
    pub excerpt: String,
    /// `scheduled`, `sending`, `sent`, `error` or `needs_approval`.
    pub status: String,
    /// When it went out, or is due to.
    pub at: Option<OffsetDateTime>,
    pub link: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftIdea {
    pub title: String,
    pub created_at: Option<OffsetDateTime>,
}

/// How every idea Homestead drafts begins (see [`draft`]).
pub const DRAFT_MARK: &str = "📸 Needs photo";

/// Buffer's `DateTime` as an ISO string, and `createdAt` on ideas as Unix
/// seconds (observed 2026-09-29); either is accepted.
fn when(v: &Value) -> Option<OffsetDateTime> {
    match v {
        Value::String(s) => OffsetDateTime::parse(s, &Rfc3339).ok(),
        Value::Number(n) => {
            let secs = n.as_i64()?;
            // Milliseconds, if it's too big to be seconds.
            let secs = if secs > 100_000_000_000 { secs / 1000 } else { secs };
            OffsetDateTime::from_unix_timestamp(secs).ok()
        }
        _ => None,
    }
}

pub(crate) fn social_from(channels: Vec<LiveChannel>, d: &Value) -> Social {
    let mut posts: Vec<SocialPost> = d["posts"]["edges"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| &e["node"])
        .map(|n| {
            let status = n["status"].as_str().unwrap_or_default().to_owned();
            let at = if status == "sent" { when(&n["sentAt"]) } else { when(&n["dueAt"]) };
            let text = n["text"].as_str().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
            let excerpt =
                if text.chars().count() > 200 { text.chars().take(199).chain(['…']).collect() } else { text };
            SocialPost {
                service: n["channelService"].as_str().unwrap_or_default().to_owned(),
                channel: n["channel"]["displayName"]
                    .as_str()
                    .or(n["channel"]["name"].as_str())
                    .unwrap_or_default()
                    .to_owned(),
                excerpt,
                status,
                at,
                link: n["externalLink"].as_str().filter(|l| l.starts_with("https://")).map(str::to_owned),
                error: n["error"]["message"].as_str().map(str::to_owned),
            }
        })
        .collect();
    // Queued first, soonest first; then what went out, newest first.
    posts.sort_by(|a, b| {
        let sent = |p: &SocialPost| p.status == "sent";
        sent(a).cmp(&sent(b)).then_with(|| if sent(a) { b.at.cmp(&a.at) } else { a.at.cmp(&b.at) })
    });
    let mut drafts: Vec<DraftIdea> = d["ideas"]["edges"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| &e["node"])
        .filter_map(|n| {
            let title = n["content"]["title"].as_str()?;
            title
                .starts_with(DRAFT_MARK)
                .then(|| DraftIdea { title: title.to_owned(), created_at: when(&n["createdAt"]) })
        })
        .collect();
    drafts.sort_by_key(|d| std::cmp::Reverse(d.created_at));
    Social { channels, posts, drafts }
}

/// Classifies the `createIdea` result union.
pub(crate) fn interpret_idea(r: &Value) -> Result<String, DeliveryError> {
    let kind = r["__typename"].as_str().unwrap_or_default();
    let msg = format!("Buffer {kind}: {}", r["message"].as_str().unwrap_or(""));
    match kind {
        "Idea" => Ok(r["id"].as_str().unwrap_or_default().to_owned()),
        // Buffer also uses UnexpectedError for a wrong organization id
        // ("Organization not found", observed 2026-09-28): that's config,
        // not a hiccup, so don't spend the retry budget on it.
        "UnexpectedError" if msg.to_lowercase().contains("not found") => Err(DeliveryError::Permanent(msg)),
        // Otherwise Buffer's own hiccup: worth another try.
        "UnexpectedError" => Err(DeliveryError::Retryable(msg)),
        // Bad input, bad key, or the plan's idea limit: a person must act.
        _ => Err(DeliveryError::Permanent(msg)),
    }
}

/// Title and text of the draft idea for an event, if it deserves one.
pub fn draft(event: &Event, hashtags: &[String]) -> Option<(String, String)> {
    let tags = hashtags.join(" ");
    match event {
        Event::HarvestLogged { harvest, crop, .. } => Some((
            format!("{DRAFT_MARK} · Fresh harvest: {crop}"),
            format!(
                "Just picked: {} {} of {crop}, grown right here in Van Nuys. 🌞\n\n\
                 Every harvest is proof you don't need acres to grow real food. Want to learn how? \
                 Our workshops are free. Link in bio.\n\n{tags}",
                harvest.quantity.normalize(),
                harvest.unit
            ),
        )),
        // Only something in the ground is "just planted".
        Event::PlantingCreated { planting, .. } if planting.status == homestead_domain::PlantingStatus::Planned => None,
        Event::PlantingCreated { planting, .. } => {
            let variety = if planting.variety.is_empty() { String::new() } else { format!(" ({})", planting.variety) };
            Some((
                format!("{DRAFT_MARK} · Just planted: {}", planting.crop),
                format!(
                    "In the ground today: {}{variety}. 🌱 Follow along as it grows, and come see it at the farm stand \
                     when it's ready.\n\n{tags}",
                    planting.crop
                ),
            ))
        }
        _ => None,
    }
}

/// Saves a draft Buffer idea for harvests and new plantings.
///
/// Redelivery can create a duplicate idea (Buffer has no idempotency key);
/// ideas are drafts, so a duplicate costs one click to delete.
pub struct DraftsSink {
    client: Client,
    organization_id: String,
    hashtags: Vec<String>,
}

impl DraftsSink {
    pub fn new(client: Client, organization_id: String, hashtags: Vec<String>) -> Self {
        Self { client, organization_id, hashtags }
    }
}

impl Sink for DraftsSink {
    fn name(&self) -> &'static str {
        "buffer"
    }

    fn accepts(&self, event: &Event) -> bool {
        draft(event, &[]).is_some()
    }

    fn deliver<'a>(&'a self, _event_id: i64, event: &'a Event) -> BoxFuture<'a, Result<(), DeliveryError>> {
        Box::pin(async move {
            let Some((title, text)) = draft(event, &self.hashtags) else { return Ok(()) };
            self.client.create_idea(&self.organization_id, &title, &text).await.map(drop)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homestead_domain::{Harvest, HarvestUnit, Task, TaskPriority, TaskStatus};
    use rust_decimal::Decimal;
    use uuid::Uuid;

    const SPEC: &str = include_str!("../../../buffer/account.toml");

    fn harvest() -> Event {
        Event::HarvestLogged {
            harvest: Harvest {
                id: Uuid::nil(),
                planting_id: Uuid::nil(),
                harvested_on: time::macros::date!(2026 - 07 - 04),
                quantity: Decimal::new(4250, 3),
                unit: HarvestUnit::Lb,
                notes: String::new(),
            },
            crop: "Sungold tomatoes".into(),
            location_name: "Bed 1".into(),
        }
    }

    #[test]
    fn shipped_spec_is_valid() {
        let spec = Spec::parse(SPEC).unwrap_or_else(|e| panic!("buffer/account.toml is invalid:\n{e}"));
        assert!(spec.channels.iter().any(|c| c.service == "instagram"));
    }

    #[test]
    fn empty_account_reports_pending_steps_not_violations() {
        let spec = Spec::parse(SPEC).expect("spec");
        // The live account as of 2026-09-28: default name, nothing connected.
        let live = LiveOrg { id: "o".into(), name: "My organization".into(), channels: vec![] };
        let f = verify(&spec, &live);
        assert_eq!(f.len(), 3, "{f:?}");
        assert!(f.iter().all(|x| matches!(x, Finding::Pending(_))));
    }

    #[test]
    fn connected_account_verifies_clean() {
        let spec = Spec::parse(SPEC).expect("spec");
        let ch = |s: &str| LiveChannel { service: s.into(), name: "sunbrewedhomestead".into(), paused: false };
        let live = LiveOrg {
            id: "o".into(),
            name: "Sun Brewed Homestead".into(),
            channels: vec![ch("instagram"), ch("facebook")],
        };
        assert!(verify(&spec, &live).iter().all(|x| matches!(x, Finding::Note(_))));
    }

    #[test]
    fn drafts_flag_the_photo_and_never_mention_people() {
        let (title, text) = draft(&harvest(), &["#sunbrewedhomestead".into()]).expect("harvest drafts");
        assert!(title.starts_with("📸 Needs photo"), "the title reminds a person to add the photo");
        assert!(text.contains("4.25 lb of Sungold tomatoes"));
        assert!(text.ends_with("#sunbrewedhomestead"));
        assert!(!text.contains("Bed 1"), "internal bed names stay off social media");
    }

    #[test]
    fn only_shareable_events_become_ideas() {
        let task = Task {
            id: Uuid::nil(),
            title: "Fix fence".into(),
            details: String::new(),
            location_id: None,
            due_on: None,
            priority: TaskPriority::Normal,
            status: TaskStatus::Open,
            mirror_to_linear: false,
            linear_issue_id: None,
            completed_at: None,
            template_id: None,
            assigned_to: None,
        };
        assert!(draft(&Event::TaskCreated { task }, &[]).is_none(), "chores aren't social posts");
        assert!(draft(&harvest(), &[]).is_some());
        let mut planting = homestead_domain::Planting {
            id: Uuid::nil(),
            location_id: Uuid::nil(),
            crop: "Lettuce".into(),
            variety: String::new(),
            sown_on: time::macros::date!(2026 - 03 - 01),
            expected_harvest_on: None,
            status: homestead_domain::PlantingStatus::Planned,
            notes: String::new(),
            seed_lot_id: None,
            crop_plan_id: None,
            family: String::new(),
            method: homestead_domain::SowMethod::Direct,
            expected_yield: None,
            expected_yield_unit: None,
            ended_on: None,
            transplant_on: None,
            expected_harvest_until: None,
        };
        let event =
            |p: &homestead_domain::Planting| Event::PlantingCreated { planting: p.clone(), location_name: "A3".into() };
        assert!(draft(&event(&planting), &[]).is_none(), "a plan is not \"just planted\"");
        planting.status = homestead_domain::PlantingStatus::Growing;
        assert!(draft(&event(&planting), &[]).is_some());
    }

    #[test]
    fn idea_result_union_is_classified() {
        assert_eq!(interpret_idea(&json!({"__typename": "Idea", "id": "i1"})), Ok("i1".to_owned()));
        assert!(matches!(
            interpret_idea(&json!({"__typename": "UnexpectedError", "message": "oops"})),
            Err(DeliveryError::Retryable(_))
        ));
        // Captured from the live API with a wrong organization id.
        assert!(matches!(
            interpret_idea(&json!({"__typename": "UnexpectedError", "message": "Organization not found"})),
            Err(DeliveryError::Permanent(_))
        ));
        for k in ["InvalidInputError", "UnauthorizedError", "LimitReachedError"] {
            assert!(
                matches!(interpret_idea(&json!({"__typename": k, "message": "x"})), Err(DeliveryError::Permanent(_))),
                "{k}"
            );
        }
    }

    #[test]
    fn social_keeps_our_drafts_and_orders_queued_before_sent() {
        let d = json!({
            "posts": { "edges": [
                { "node": { "text": "Old post", "status": "sent", "sentAt": "2026-09-01T10:00:00.000Z",
                            "channelService": "instagram", "channel": { "displayName": "sunbrewedhomestead" },
                            "externalLink": "https://www.instagram.com/p/abc/", "error": null } },
                { "node": { "text": "Newer post", "status": "sent", "sentAt": "2026-09-20T10:00:00.000Z",
                            "channelService": "facebook", "channel": { "name": "Sun Brewed" }, "error": null } },
                { "node": { "text": "Work day   Saturday", "status": "scheduled", "dueAt": "2026-10-03T16:00:00.000Z",
                            "channelService": "instagram", "channel": { "displayName": "sunbrewedhomestead" } } },
                { "node": { "text": "Broke", "status": "error", "dueAt": "2026-09-30T16:00:00.000Z",
                            "channelService": "instagram", "channel": {}, "error": { "message": "Token expired" } } }
            ] },
            // Buffer's own onboarding ideas (verbatim titles) are not ours.
            "ideas": { "edges": [
                { "node": { "content": { "title": "This is a place to plan ✍️ your content" }, "createdAt": 1790666520 } },
                { "node": { "content": { "title": "📸 Needs photo · Fresh harvest: Kale" }, "createdAt": 1790700000 } }
            ] }
        });
        let s = social_from(vec![], &d);
        let order: Vec<&str> = s.posts.iter().map(|p| p.excerpt.as_str()).collect();
        assert_eq!(order, ["Broke", "Work day Saturday", "Newer post", "Old post"]);
        assert_eq!(s.posts[0].error.as_deref(), Some("Token expired"));
        assert_eq!(s.posts[2].channel, "Sun Brewed");
        assert_eq!(s.posts[3].link.as_deref(), Some("https://www.instagram.com/p/abc/"));
        assert_eq!(s.drafts.len(), 1);
        assert_eq!(s.drafts[0].created_at, OffsetDateTime::from_unix_timestamp(1_790_700_000).ok());
        let (title, _) = draft(&harvest(), &[]).expect("harvest");
        assert!(title.starts_with(DRAFT_MARK), "the drafts sink and this filter agree on the mark");
    }

    #[test]
    fn rejects_unknown_services_and_bad_hashtags() {
        assert!(
            Spec::parse(&SPEC.replace("\"facebook\"", "\"nextdoor\""))
                .expect_err("svc")
                .contains("unknown Buffer service")
        );
        assert!(Spec::parse(&SPEC.replace("\"#vannuys\"", "\"van nuys\"")).expect_err("tag").contains("hashtag"));
    }
}

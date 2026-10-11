//! Zeffy: campaign spec (`zeffy/campaign.toml`) and read-only verification.
//!
//! Zeffy's public API cannot create or edit campaigns, so there is no
//! `apply`: a person builds the form in the dashboard from the spec, and
//! [`verify`] checks the live campaign against it. Verification separates
//! *violations* (CI fails, e.g. a campaign meant to be private is live) from
//! *pending* manual steps (CI warns, e.g. not created yet).

use serde::Deserialize;
use serde_json::Value;

pub const API_BASE: &str = "https://api.zeffy.com/api/v1";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub campaign: Campaign,
    pub appearance: Appearance,
    pub description: Description,
    pub budget: Vec<BudgetLine>,
    pub giving: Giving,
    pub receipts: Receipts,
    pub questions: Vec<Question>,
    pub thank_you_email: Email,
    pub advanced: Advanced,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Campaign {
    pub title: String,
    pub status: Status,
    pub goal_usd: u64,
    pub currency: String,
    pub language: String,
    #[serde(default)]
    pub translations: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Unpublished: only visible inside the Zeffy dashboard.
    Draft,
    Active,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Appearance {
    pub theme: String,
    pub shapes: bool,
    pub banner: String,
    pub logo: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Description {
    pub headline: String,
    pub body: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetLine {
    pub fund: String,
    pub usd: u64,
    pub detail: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Giving {
    pub frequencies: Vec<String>,
    pub default_frequency: String,
    pub default_amount_usd: u64,
    #[serde(default)]
    pub one_time: Vec<Amount>,
    #[serde(default)]
    pub monthly: Vec<Amount>,
    #[serde(default)]
    pub quarterly: Vec<Amount>,
    #[serde(default)]
    pub yearly: Vec<Amount>,
}

impl Giving {
    pub fn amounts(&self, frequency: &str) -> Option<&[Amount]> {
        match frequency {
            "one_time" => Some(&self.one_time),
            "monthly" => Some(&self.monthly),
            "quarterly" => Some(&self.quarterly),
            "yearly" => Some(&self.yearly),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Amount {
    pub usd: u64,
    pub impact: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipts {
    pub tax_receipts: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub kind: String,
    pub label: String,
    pub options: Vec<String>,
    pub default: Option<String>,
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Email {
    pub subject: String,
    pub body: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Advanced {
    pub honorary_donations: bool,
    pub public_donor_list: bool,
    pub thermometer: bool,
    pub check_donations: bool,
}

/// The fund-designation option that isn't a budget line.
pub const GENERAL_FUND: &str = "Where it's needed most";

impl Spec {
    pub fn parse(text: &str) -> Result<Self, String> {
        let spec: Spec = toml::from_str(text).map_err(|e| e.to_string())?;
        spec.validate()?;
        Ok(spec)
    }

    /// Internal consistency: the numbers a donor sees must add up.
    pub fn validate(&self) -> Result<(), String> {
        let mut e = Vec::new();
        let c = &self.campaign;
        if c.title.trim().is_empty() || c.title.len() > 80 {
            e.push("campaign.title must be 1–80 characters".to_owned());
        }
        if c.currency != "usd" {
            e.push("campaign.currency: only usd is supported".to_owned());
        }
        let budget: u64 = self.budget.iter().map(|b| b.usd).sum();
        if budget != c.goal_usd {
            e.push(format!("budget lines add up to ${budget}, but goal_usd is ${}", c.goal_usd));
        }
        let g = &self.giving;
        for f in &g.frequencies {
            match g.amounts(f) {
                None => e.push(format!("giving: unknown frequency {f:?}")),
                Some([]) => e.push(format!("giving: {f} is enabled but has no amounts")),
                Some(amounts) => {
                    let mut sorted: Vec<u64> = amounts.iter().map(|a| a.usd).collect();
                    let n = sorted.len();
                    sorted.dedup();
                    if !sorted.is_sorted() || sorted.len() != n || sorted.first() == Some(&0) {
                        e.push(format!("giving.{f}: amounts must be distinct, ascending and non-zero"));
                    }
                    if amounts.iter().any(|a| a.impact.trim().is_empty()) {
                        e.push(format!("giving.{f}: every amount needs an impact line"));
                    }
                }
            }
        }
        match g.amounts(&g.default_frequency) {
            Some(a) if g.frequencies.contains(&g.default_frequency) => {
                if !a.iter().any(|x| x.usd == g.default_amount_usd) {
                    e.push(format!(
                        "default_amount_usd ${} is not offered for {}",
                        g.default_amount_usd, g.default_frequency
                    ));
                }
            }
            _ => e.push(format!("default_frequency {:?} is not enabled", g.default_frequency)),
        }
        let funds: Vec<&str> = self.budget.iter().map(|b| b.fund.as_str()).collect();
        for q in &self.questions {
            if q.options.is_empty() {
                e.push(format!("question {:?} has no options", q.label));
            }
            if let Some(d) = &q.default
                && !q.options.contains(d)
            {
                e.push(format!("question {:?}: default {d:?} is not an option", q.label));
            }
            if q.kind == "fund_designation" {
                let mut want: Vec<&str> = std::iter::once(GENERAL_FUND).chain(funds.iter().copied()).collect();
                let mut got: Vec<&str> = q.options.iter().map(String::as_str).collect();
                want.sort_unstable();
                got.sort_unstable();
                if want != got {
                    e.push(format!("fund designation options must be {GENERAL_FUND:?} plus exactly the budget funds"));
                }
            }
        }
        if !self.thank_you_email.body.contains("501(c)(3)") {
            e.push("thank-you email must state 501(c)(3) status (tax receipts)".to_owned());
        }
        if e.is_empty() { Ok(()) } else { Err(e.join("\n")) }
    }
}

// ── Live state ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct LiveCampaign {
    pub id: String,
    pub title: String,
    pub status: String,
    pub url: String,
    pub goal_cents: Option<u64>,
    pub currency: String,
    pub archived: bool,
}

pub fn parse_campaign(v: &Value) -> LiveCampaign {
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_owned();
    LiveCampaign {
        id: s("id"),
        title: s("title"),
        status: s("status"),
        url: s("url"),
        goal_cents: v["goal_amount"].as_u64(),
        currency: s("currency"),
        archived: v["is_archived"].as_bool().unwrap_or(false) || !v["deleted_at"].is_null(),
    }
}

/// All campaigns, following cursor pagination.
pub async fn list_campaigns(client: &reqwest::Client, base: &str, api_key: &str) -> Result<Vec<LiveCampaign>, String> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut url = format!("{base}/campaigns?limit=100");
        if let Some(c) = &cursor {
            url.push_str(&format!("&starting_after={c}"));
        }
        let resp = client.get(&url).bearer_auth(api_key).send().await.map_err(|e| format!("GET campaigns: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let hint = if status.as_u16() == 401 { " (check ZEFFY_API_KEY)" } else { "" };
            return Err(format!("GET campaigns → HTTP {status}{hint}: {}", text.chars().take(300).collect::<String>()));
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        out.extend(v["data"].as_array().into_iter().flatten().map(parse_campaign));
        cursor = v["next_cursor"].as_str().map(str::to_owned).filter(|_| v["has_more"] == true);
        if cursor.is_none() {
            return Ok(out);
        }
    }
}

// ── Verification ──────────────────────────────────────────────────────────────

pub use crate::Finding;

/// The only status Zeffy's API is known to report for a published,
/// publicly reachable form. Anything else is treated as not public.
pub const PUBLIC_STATUS: &str = "active";

pub fn verify(spec: &Spec, live: &[LiveCampaign]) -> Vec<Finding> {
    let c = &spec.campaign;
    let mut out = Vec::new();
    let current: Vec<&LiveCampaign> = live.iter().filter(|l| !l.archived).collect();
    match current.iter().find(|l| l.title.trim().eq_ignore_ascii_case(c.title.trim())) {
        None => out.push(Pending(format!(
            "campaign {:?} does not exist in Zeffy yet: build it from zeffy/campaign.toml (Campaigns → Create → Donation form), and leave it in Draft",
            c.title
        ))),
        Some(l) => {
            let is_public = l.status == PUBLIC_STATUS;
            match (c.status, is_public) {
                (Status::Draft, true) => out.push(Violation(format!(
                    "{:?} is PUBLISHED ({}) but the spec says draft/private. Unpublish it in Zeffy, or set status = \"active\" to launch on purpose",
                    l.title, l.url
                ))),
                (Status::Active, false) => {
                    out.push(Pending(format!("{:?} is still {:?}: publish it in Zeffy to launch", l.title, l.status)));
                }
                _ => out.push(Note(format!("{:?} is {} as intended", l.title, if is_public { "live" } else { "private (draft)" }))),
            }
            let want_cents = c.goal_usd * 100;
            if l.goal_cents != Some(want_cents) {
                out.push(Violation(format!(
                    "{:?} goal is {} but the spec says ${}",
                    l.title,
                    l.goal_cents.map_or("unset".to_owned(), |g| format!("${}", g / 100)),
                    c.goal_usd
                )));
            }
            if !l.currency.eq_ignore_ascii_case(&c.currency) {
                out.push(Violation(format!("{:?} currency is {:?}, spec says {:?}", l.title, l.currency, c.currency)));
            }
        }
    }
    for other in current.iter().filter(|l| !l.title.trim().eq_ignore_ascii_case(c.title.trim())) {
        if other.status == PUBLIC_STATUS {
            out.push(Pending(format!(
                "{:?} is live at {} but isn't in the spec; archive it in Zeffy if it's Zeffy's starter form",
                other.title, other.url
            )));
        }
    }
    out
}

use Finding::{Note, Pending, Violation};

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = include_str!("../../../zeffy/campaign.toml");

    fn live(title: &str, status: &str, goal_usd: u64) -> LiveCampaign {
        LiveCampaign {
            id: "x".into(),
            title: title.into(),
            status: status.into(),
            url: format!("https://www.zeffy.com/donation-form/{}", title.to_lowercase().replace(' ', "-")),
            goal_cents: Some(goal_usd * 100),
            currency: "usd".into(),
            archived: false,
        }
    }

    #[test]
    fn shipped_campaign_is_consistent_and_private() {
        let spec = Spec::parse(SPEC).unwrap_or_else(|e| panic!("zeffy/campaign.toml is invalid:\n{e}"));
        assert_eq!(spec.campaign.status, Status::Draft, "the campaign must stay private until launch is decided");
    }

    #[test]
    fn a_draft_campaign_going_live_is_a_violation() {
        let spec = Spec::parse(SPEC).expect("spec");
        let f = verify(&spec, &[live("Grow the Valley", "active", 15000)]);
        assert!(matches!(&f[0], Violation(m) if m.contains("PUBLISHED")), "{f:?}");
    }

    #[test]
    fn a_draft_campaign_in_draft_passes() {
        let spec = Spec::parse(SPEC).expect("spec");
        let f = verify(&spec, &[live("grow the valley", "draft", 15000)]);
        assert!(f.iter().all(|x| !matches!(x, Violation(_))), "{f:?}");
    }

    #[test]
    fn missing_campaign_and_stray_public_forms_are_pending_not_failures() {
        let spec = Spec::parse(SPEC).expect("spec");
        let f = verify(&spec, &[live("Donate to Change Lives", "active", 1_000_000)]);
        assert_eq!(f.len(), 2);
        assert!(f.iter().all(|x| matches!(x, Pending(_))), "{f:?}");
    }

    #[test]
    fn goal_drift_is_a_violation_and_archived_forms_are_ignored() {
        let spec = Spec::parse(SPEC).expect("spec");
        let f = verify(&spec, &[live("Grow the Valley", "draft", 9000)]);
        assert!(f.iter().any(|x| matches!(x, Violation(m) if m.contains("goal"))));
        let mut old = live("Grow the Valley", "active", 1);
        old.archived = true;
        let f = verify(&spec, &[old]);
        assert!(matches!(&f[0], Pending(_)), "an archived copy doesn't count as the campaign");
    }

    #[test]
    fn inconsistent_numbers_are_rejected() {
        let broken = SPEC.replace("goal_usd = 15000", "goal_usd = 20000");
        assert!(Spec::parse(&broken).expect_err("budget mismatch").contains("add up to"));
        let broken = SPEC.replace("default_amount_usd = 50", "default_amount_usd = 75");
        assert!(Spec::parse(&broken).expect_err("default").contains("not offered"));
        let broken = SPEC.replace("\"Goats & chickens\", \"Kiki's", "\"Goats\", \"Kiki's");
        assert!(Spec::parse(&broken).expect_err("funds").contains("fund designation"));
    }

    #[test]
    fn parses_the_real_api_shape() {
        // Captured from GET /api/v1/campaigns on 2026-09-28.
        let v: Value = serde_json::from_str(
            r#"{"id":"f4de1bd5","object":"campaign","type":"donation_form","status":"active",
                "title":"Donate to Change Lives","url":"https://www.zeffy.com/donation-form/donate-to-change-lives-24993",
                "goal_amount":100000000,"currency":"usd","is_archived":false,"deleted_at":null}"#,
        )
        .expect("json");
        let c = parse_campaign(&v);
        assert_eq!((c.status.as_str(), c.goal_cents, c.archived), ("active", Some(100_000_000), false));
    }
}

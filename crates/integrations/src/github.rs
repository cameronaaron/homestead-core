//! GitHub organization as code (`github/org.toml`): profile, safe defaults,
//! any other org setting, members, teams, team repo access, and repos with
//! their settings.
//!
//! [`plan`] is a pure diff; [`Client::apply`] executes it over the REST API
//! and re-plans to confirm. Nothing outside the spec is removed. Adding a
//! member sends a real invitation, so the plan names it explicitly.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::Finding;

pub const API: &str = "https://api.github.com";

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub org: OrgSpec,
    pub members: Vec<MemberSpec>,
    #[serde(default)]
    pub teams: Vec<TeamSpec>,
    #[serde(default)]
    pub repos: Vec<RepoSpec>,
}

/// Raw REST fields, compared with what GitHub reports (see
/// [`crate::settings::covers`]) and sent when they differ.
pub type Settings = BTreeMap<String, Value>;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoSpec {
    pub name: String,
    /// Any Update-a-repository field (`has_wiki`, `delete_branch_on_merge`,
    /// `security_and_analysis`…). A missing repo is created private, with
    /// these settings.
    #[serde(default)]
    pub settings: Settings,
}

/// Org fields the typed spec owns.
const ORG_OWNED: &[&str] = &[
    "login",
    "name",
    "description",
    "blog",
    "location",
    "default_repository_permission",
    "members_can_create_public_repositories",
    "members_can_create_private_repositories",
    "two_factor_requirement_enabled",
];

fn check_settings(whose: &str, settings: &Settings, owned: &[&str], e: &mut Vec<String>) {
    for k in settings.keys() {
        if owned.contains(&k.as_str()) {
            e.push(format!("{whose} settings: {k:?} is set by its own field in this file, not settings"));
        }
        if k.is_empty() || !k.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            e.push(format!("{whose} settings: {k:?} is not a GitHub API field name (snake_case)"));
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgSpec {
    pub login: String,
    pub name: String,
    pub description: String,
    pub blog: String,
    pub location: String,
    pub default_repository_permission: String,
    pub members_can_create_public_repositories: bool,
    pub members_can_create_private_repositories: bool,
    #[serde(default)]
    pub require_two_factor: bool,
    /// Any other Update-an-organization field.
    #[serde(default)]
    pub settings: Settings,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberSpec {
    pub login: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamSpec {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub maintainers: Vec<String>,
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub repos: BTreeMap<String, String>,
}

/// Repo access levels as GitHub reports them (`role_name`).
const ACCESS: &[&str] = &["read", "triage", "write", "maintain", "admin"];

/// `role_name` → the `permission` value the write endpoint expects.
fn permission_param(access: &str) -> &str {
    match access {
        "read" => "pull",
        "write" => "push",
        other => other,
    }
}

impl Spec {
    pub fn parse(text: &str) -> Result<Self, String> {
        let spec: Spec = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut e = Vec::new();
        if !["none", "read", "write", "admin"].contains(&spec.org.default_repository_permission.as_str()) {
            e.push("org.default_repository_permission must be none, read, write or admin".to_owned());
        }
        let mut logins = BTreeSet::new();
        for m in &spec.members {
            if !["admin", "member"].contains(&m.role.as_str()) {
                e.push(format!("member {:?}: role must be admin or member", m.login));
            }
            if !logins.insert(m.login.to_lowercase()) {
                e.push(format!("member {:?} is listed twice", m.login));
            }
        }
        if !spec.members.iter().any(|m| m.role == "admin") {
            e.push("at least one member must be an org admin".to_owned());
        }
        let mut teams = BTreeSet::new();
        for t in &spec.teams {
            if !teams.insert(t.name.to_lowercase()) {
                e.push(format!("team {:?} is listed twice", t.name));
            }
            for who in t.maintainers.iter().chain(&t.members) {
                if !logins.contains(&who.to_lowercase()) {
                    e.push(format!("team {:?}: {who:?} must also be listed under [[members]]", t.name));
                }
            }
            if let Some(dup) = t.maintainers.iter().find(|m| t.members.contains(m)) {
                e.push(format!("team {:?}: {dup:?} is both maintainer and member", t.name));
            }
            for (repo, access) in &t.repos {
                if !ACCESS.contains(&access.as_str()) {
                    e.push(format!("team {:?}: {repo} access must be one of {ACCESS:?}", t.name));
                }
            }
        }
        check_settings("org", &spec.org.settings, ORG_OWNED, &mut e);
        let mut repos = BTreeSet::new();
        for r in &spec.repos {
            if !repos.insert(r.name.to_lowercase()) {
                e.push(format!("repo {:?} is listed twice", r.name));
            }
            check_settings(&format!("repo {:?}", r.name), &r.settings, &["name"], &mut e);
        }
        if e.is_empty() { Ok(spec) } else { Err(e.join("\n")) }
    }
}

// ── Live state ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub org: Value,
    /// login (lowercase) → "admin" | "member"
    pub members: BTreeMap<String, String>,
    /// Logins (lowercase) with a pending invitation.
    pub invited: BTreeSet<String>,
    pub teams: Vec<LiveTeam>,
    pub repos: Vec<String>,
    /// Full GET response for every repo named under `[[repos]]` that exists.
    pub repo_details: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveTeam {
    pub name: String,
    pub slug: String,
    pub description: String,
    pub privacy: String,
    /// login (lowercase) → "maintainer" | "member"
    pub members: BTreeMap<String, String>,
    /// repo name → access (`role_name`)
    pub repos: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    UpdateOrg { changed: Vec<String> },
    CreateRepo(RepoSpec),
    UpdateRepo { spec: RepoSpec, changed: Vec<String> },
    Invite { login: String, role: String },
    SetOrgRole { login: String, role: String },
    CreateTeam(TeamSpec),
    UpdateTeam { slug: String, spec: TeamSpec, changed: Vec<&'static str> },
    SetTeamRole { team: String, login: String, role: &'static str },
    SetTeamRepo { team: String, repo: String, access: String },
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Action::UpdateOrg { changed } => write!(f, "~ org settings ({})", changed.join(", ")),
            Action::CreateRepo(r) => write!(f, "+ private repo {}", r.name),
            Action::UpdateRepo { spec, changed } => write!(f, "~ repo {} ({})", spec.name, changed.join(", ")),
            Action::Invite { login, role } => write!(f, "+ invite @{login} to the org as {role} (sends an email)"),
            Action::SetOrgRole { login, role } => write!(f, "~ @{login} org role → {role}"),
            Action::CreateTeam(t) => write!(f, "+ team \"{}\"", t.name),
            Action::UpdateTeam { spec, changed, .. } => write!(f, "~ team \"{}\" ({})", spec.name, changed.join(", ")),
            Action::SetTeamRole { team, login, role } => write!(f, "~ team \"{team}\": @{login} as {role}"),
            Action::SetTeamRepo { team, repo, access } => write!(f, "~ team \"{team}\": {repo} → {access}"),
        }
    }
}

/// Settings keys whose live value doesn't match yet.
fn settings_changes(want: &Settings, live: &Value) -> Vec<String> {
    want.iter().filter(|(k, v)| !crate::settings::covers(v, &live[k.as_str()])).map(|(k, _)| k.clone()).collect()
}

fn org_changes(want: &OrgSpec, live: &Value) -> Vec<String> {
    let s = |k: &str| live[k].as_str().unwrap_or_default().to_owned();
    let mut c = Vec::new();
    if s("name") != want.name {
        c.push("name");
    }
    if s("description") != want.description {
        c.push("description");
    }
    if s("blog") != want.blog {
        c.push("website");
    }
    if s("location") != want.location {
        c.push("location");
    }
    if s("default_repository_permission") != want.default_repository_permission {
        c.push("default repo permission");
    }
    if live["members_can_create_public_repositories"].as_bool() != Some(want.members_can_create_public_repositories) {
        c.push("public repo creation");
    }
    if live["members_can_create_private_repositories"].as_bool() != Some(want.members_can_create_private_repositories) {
        c.push("private repo creation");
    }
    let mut c: Vec<String> = c.into_iter().map(str::to_owned).collect();
    c.extend(settings_changes(&want.settings, live));
    c
}

pub fn plan(spec: &Spec, state: &State) -> Vec<Action> {
    let mut out = Vec::new();
    let changed = org_changes(&spec.org, &state.org);
    if !changed.is_empty() {
        out.push(Action::UpdateOrg { changed });
    }
    for m in &spec.members {
        let key = m.login.to_lowercase();
        match state.members.get(&key) {
            None if state.invited.contains(&key) => {}
            None => out.push(Action::Invite { login: m.login.clone(), role: m.role.clone() }),
            Some(r) if r != &m.role => out.push(Action::SetOrgRole { login: m.login.clone(), role: m.role.clone() }),
            Some(_) => {}
        }
    }
    for t in &spec.teams {
        let live = state.teams.iter().find(|l| l.name.eq_ignore_ascii_case(&t.name));
        match live {
            None => out.push(Action::CreateTeam(t.clone())),
            Some(l) => {
                let mut c = Vec::new();
                if l.description != t.description {
                    c.push("description");
                }
                if l.privacy != "closed" {
                    c.push("visibility");
                }
                if !c.is_empty() {
                    out.push(Action::UpdateTeam { slug: l.slug.clone(), spec: t.clone(), changed: c });
                }
            }
        }
        let live_members = live.map(|l| &l.members);
        let roles = t.maintainers.iter().map(|m| (m, "maintainer")).chain(t.members.iter().map(|m| (m, "member")));
        for (login, role) in roles {
            // Invited-but-not-joined people can't be put on teams yet.
            if !state.members.contains_key(&login.to_lowercase()) {
                continue;
            }
            if live_members.and_then(|m| m.get(&login.to_lowercase())).map(String::as_str) != Some(role) {
                out.push(Action::SetTeamRole { team: t.name.clone(), login: login.clone(), role });
            }
        }
        for (repo, access) in &t.repos {
            if live.and_then(|l| l.repos.get(repo)) != Some(access) {
                out.push(Action::SetTeamRepo { team: t.name.clone(), repo: repo.clone(), access: access.clone() });
            }
        }
    }
    // Repos before team access: a team can't be given a repo that doesn't
    // exist yet, so a new repo's grants land on the next pass.
    let mut repo_actions = Vec::new();
    for r in &spec.repos {
        if !state.repos.iter().any(|x| x.eq_ignore_ascii_case(&r.name)) {
            repo_actions.push(Action::CreateRepo(r.clone()));
            continue;
        }
        let live = state.repo_details.get(&r.name.to_lowercase()).unwrap_or(&Value::Null);
        let changed = settings_changes(&r.settings, live);
        if !changed.is_empty() {
            repo_actions.push(Action::UpdateRepo { spec: r.clone(), changed });
        }
    }
    let first_team = out
        .iter()
        .position(|a| !matches!(a, Action::UpdateOrg { .. } | Action::Invite { .. } | Action::SetOrgRole { .. }))
        .unwrap_or(out.len());
    out.splice(first_team..first_team, repo_actions);
    out
}

/// A spec the org's GitHub plan can't hold. GitHub rejects the whole org
/// PATCH with 422 "Private-only repository creation policy is not allowed
/// for this organization" when a free org asks for private-but-not-public
/// member repo creation, so nothing else in that PATCH applies either.
/// (Observed 2026-09-28 on Sunbrewed-Homestead.)
pub fn plan_limit(spec: &Spec, state: &State) -> Option<String> {
    let free = state.org["plan"]["name"].as_str() == Some("free");
    let private_only =
        spec.org.members_can_create_private_repositories && !spec.org.members_can_create_public_repositories;
    (free && private_only).then(|| {
        "GitHub's free plan can't let members create only private repos: set both \
         members_can_create_*_repositories to false (only owners create repos) or both to true"
            .to_owned()
    })
}

/// Things the API can't change, and things present but not in the spec.
pub fn advisories(spec: &Spec, state: &State) -> Vec<Finding> {
    let mut out = Vec::new();
    if let Some(limit) = plan_limit(spec, state) {
        out.push(Finding::Violation(limit));
    }
    if spec.org.require_two_factor && state.org["two_factor_requirement_enabled"].as_bool() != Some(true) {
        out.push(Finding::Pending(
            "require two-factor auth: Settings → Authentication security (GitHub's API can't set this)".to_owned(),
        ));
    }
    let listed: BTreeSet<String> = spec.members.iter().map(|m| m.login.to_lowercase()).collect();
    for login in state.members.keys().filter(|l| !listed.contains(*l)) {
        out.push(Finding::Note(format!("unmanaged member @{login} (left alone)")));
    }
    let teams: BTreeSet<String> = spec.teams.iter().map(|t| t.name.to_lowercase()).collect();
    for t in state.teams.iter().filter(|t| !teams.contains(&t.name.to_lowercase())) {
        out.push(Finding::Note(format!("unmanaged team \"{}\" (left alone)", t.name)));
    }
    let repos: BTreeSet<&str> = spec
        .teams
        .iter()
        .flat_map(|t| t.repos.keys())
        .chain(spec.repos.iter().map(|r| &r.name))
        .map(String::as_str)
        .collect();
    for r in state.repos.iter().filter(|r| !repos.contains(r.as_str())) {
        out.push(Finding::Note(format!("unmanaged repo {r} (left alone)")));
    }
    out
}

// ── REST client ───────────────────────────────────────────────────────────────

pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Client {
    pub fn new(http: reqwest::Client, base: Option<String>, token: String) -> Self {
        Self { http, base: base.unwrap_or_else(|| API.to_owned()), token }
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value, String> {
        let mut req = self
            .http
            .request(method.clone(), format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.map_err(|e| format!("{method} {path}: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let hint = match status.as_u16() {
                401 => " (check the GitHub token)",
                403 | 404 => {
                    " (the token needs org admin rights: admin:org, or a fine-grained token with Administration and Members write)"
                }
                _ => "",
            };
            return Err(format!(
                "{method} {path} → HTTP {status}{hint}: {}",
                text.chars().take(300).collect::<String>()
            ));
        }
        Ok(if text.is_empty() { Value::Null } else { serde_json::from_str(&text).map_err(|e| e.to_string())? })
    }

    async fn list(&self, path: &str) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        for page in 1..=20 {
            let sep = if path.contains('?') { '&' } else { '?' };
            let v = self.call(reqwest::Method::GET, &format!("{path}{sep}per_page=100&page={page}"), None).await?;
            let items = v.as_array().cloned().unwrap_or_default();
            let n = items.len();
            out.extend(items);
            if n < 100 {
                break;
            }
        }
        Ok(out)
    }

    pub async fn fetch(&self, org: &str) -> Result<State, String> {
        let login = |v: &Value| v["login"].as_str().unwrap_or_default().to_lowercase();
        let org_v = self.call(reqwest::Method::GET, &format!("/orgs/{org}"), None).await?;
        let mut members = BTreeMap::new();
        for role in ["admin", "member"] {
            for m in self.list(&format!("/orgs/{org}/members?role={role}")).await? {
                members.insert(login(&m), role.to_owned());
            }
        }
        // Listing invitations needs admin rights; a read-only token sees none.
        let invited =
            self.list(&format!("/orgs/{org}/invitations")).await.unwrap_or_default().iter().map(login).collect();
        let mut teams = Vec::new();
        for t in self.list(&format!("/orgs/{org}/teams")).await? {
            let slug = t["slug"].as_str().unwrap_or_default().to_owned();
            let mut tm = BTreeMap::new();
            for role in ["maintainer", "member"] {
                for m in self.list(&format!("/orgs/{org}/teams/{slug}/members?role={role}")).await? {
                    tm.insert(login(&m), role.to_owned());
                }
            }
            let repos = self
                .list(&format!("/orgs/{org}/teams/{slug}/repos"))
                .await?
                .iter()
                .map(|r| {
                    (
                        r["name"].as_str().unwrap_or_default().to_owned(),
                        r["role_name"].as_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect();
            teams.push(LiveTeam {
                name: t["name"].as_str().unwrap_or_default().to_owned(),
                slug,
                description: t["description"].as_str().unwrap_or_default().to_owned(),
                privacy: t["privacy"].as_str().unwrap_or_default().to_owned(),
                members: tm,
                repos,
            });
        }
        let repos = self
            .list(&format!("/orgs/{org}/repos"))
            .await?
            .iter()
            .filter_map(|r| r["name"].as_str().map(str::to_owned))
            .collect();
        Ok(State { org: org_v, members, invited, teams, repos, repo_details: BTreeMap::new() })
    }

    /// [`Client::fetch`], plus the full settings of every `[[repos]]` entry.
    pub async fn fetch_spec(&self, spec: &Spec) -> Result<State, String> {
        let org = &spec.org.login;
        let mut state = self.fetch(org).await?;
        for r in &spec.repos {
            if state.repos.iter().any(|x| x.eq_ignore_ascii_case(&r.name)) {
                let v = self.call(reqwest::Method::GET, &format!("/repos/{org}/{}", r.name), None).await?;
                state.repo_details.insert(r.name.to_lowercase(), v);
            }
        }
        Ok(state)
    }

    /// Looked up fresh: a team created earlier in this pass has a slug the
    /// plan's snapshot doesn't know.
    async fn slug_of(&self, org: &str, name: &str) -> Result<String, String> {
        self.list(&format!("/orgs/{org}/teams"))
            .await?
            .into_iter()
            .find(|t| t["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name)))
            .and_then(|t| t["slug"].as_str().map(str::to_owned))
            .ok_or_else(|| format!("team {name:?} not found"))
    }

    async fn execute(&self, spec: &Spec, a: &Action) -> Result<(), String> {
        use reqwest::Method;
        let org = &spec.org.login;
        match a {
            Action::UpdateOrg { .. } => {
                let o = &spec.org;
                let mut body: serde_json::Map<String, Value> =
                    o.settings.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                body.extend(
                    json!({
                        "name": o.name, "description": o.description, "blog": o.blog, "location": o.location,
                        "default_repository_permission": o.default_repository_permission,
                        "members_can_create_public_repositories": o.members_can_create_public_repositories,
                        "members_can_create_private_repositories": o.members_can_create_private_repositories,
                    })
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
                );
                self.call(Method::PATCH, &format!("/orgs/{org}"), Some(Value::Object(body))).await?;
            }
            Action::CreateRepo(r) => {
                let mut body: serde_json::Map<String, Value> =
                    r.settings.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                body.insert("name".into(), json!(r.name));
                body.entry("private").or_insert(json!(true));
                self.call(Method::POST, &format!("/orgs/{org}/repos"), Some(Value::Object(body))).await?;
            }
            Action::UpdateRepo { spec: r, .. } => {
                let body: serde_json::Map<String, Value> =
                    r.settings.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                self.call(Method::PATCH, &format!("/repos/{org}/{}", r.name), Some(Value::Object(body))).await?;
            }
            Action::Invite { login, role } | Action::SetOrgRole { login, role } => {
                self.call(Method::PUT, &format!("/orgs/{org}/memberships/{login}"), Some(json!({ "role": role })))
                    .await?;
            }
            Action::CreateTeam(t) => {
                self.call(
                    Method::POST,
                    &format!("/orgs/{org}/teams"),
                    Some(json!({ "name": t.name, "description": t.description, "privacy": "closed" })),
                )
                .await?;
            }
            Action::UpdateTeam { slug, spec: t, .. } => {
                self.call(
                    Method::PATCH,
                    &format!("/orgs/{org}/teams/{slug}"),
                    Some(json!({ "name": t.name, "description": t.description, "privacy": "closed" })),
                )
                .await?;
            }
            Action::SetTeamRole { team, login, role } => {
                let slug = self.slug_of(org, team).await?;
                self.call(
                    Method::PUT,
                    &format!("/orgs/{org}/teams/{slug}/memberships/{login}"),
                    Some(json!({ "role": role })),
                )
                .await?;
            }
            Action::SetTeamRepo { team, repo, access } => {
                let slug = self.slug_of(org, team).await?;
                self.call(
                    Method::PUT,
                    &format!("/orgs/{org}/teams/{slug}/repos/{org}/{repo}"),
                    Some(json!({ "permission": permission_param(access) })),
                )
                .await?;
            }
        }
        Ok(())
    }

    pub async fn apply(&self, spec: &Spec) -> Result<Vec<String>, String> {
        let mut done = Vec::new();
        for _ in 0..3 {
            let state = self.fetch_spec(spec).await?;
            if let Some(limit) = plan_limit(spec, &state) {
                return Err(limit);
            }
            let actions = plan(spec, &state);
            if actions.is_empty() {
                return Ok(done);
            }
            for a in &actions {
                self.execute(spec, a).await.map_err(|e| format!("{a}: {e}"))?;
                done.push(a.to_string());
            }
        }
        let left = plan(spec, &self.fetch_spec(spec).await?);
        if left.is_empty() {
            Ok(done)
        } else {
            Err(format!(
                "GitHub did not converge; still differs:\n{}",
                left.iter().map(|a| format!("  {a}")).collect::<Vec<_>>().join("\n")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = include_str!("../../../github/org.toml");

    /// The org as it was on 2026-09-28, before the spec was applied.
    fn today() -> State {
        State {
            org: json!({
                "name": "Sunbrewed Homestead", "description": null, "blog": null, "location": null,
                "default_repository_permission": "read",
                "members_can_create_public_repositories": true, "members_can_create_private_repositories": true,
                "two_factor_requirement_enabled": false, "web_commit_signoff_required": false
            }),
            members: [("cameronaaron".to_owned(), "admin".to_owned())].into(),
            invited: BTreeSet::new(),
            teams: vec![],
            repos: vec!["demo-repository".into(), "homestead".into()],
            // As `gh api repos/Sunbrewed-Homestead/homestead` reported it.
            repo_details: [(
                "homestead".to_owned(),
                json!({
                    "description": "Sun Brewed Homestead operations core: farm app, and Discord/Linear/Zeffy as code. Proprietary — all rights reserved.",
                    "private": true, "visibility": "private", "has_issues": true, "has_projects": true,
                    "has_wiki": false, "has_discussions": false, "allow_merge_commit": true,
                    "allow_squash_merge": true, "allow_rebase_merge": true, "delete_branch_on_merge": false,
                    "allow_auto_merge": false, "default_branch": "main", "security_and_analysis": null
                }),
            )]
            .into(),
        }
    }

    fn converged(spec: &Spec) -> State {
        let o = &spec.org;
        let mut s = State {
            org: json!({
                "name": o.name, "description": o.description, "blog": o.blog, "location": o.location,
                "default_repository_permission": o.default_repository_permission,
                "members_can_create_public_repositories": o.members_can_create_public_repositories,
                "members_can_create_private_repositories": o.members_can_create_private_repositories,
                "two_factor_requirement_enabled": true
            }),
            members: spec.members.iter().map(|m| (m.login.to_lowercase(), m.role.clone())).collect(),
            invited: BTreeSet::new(),
            teams: spec
                .teams
                .iter()
                .map(|t| LiveTeam {
                    name: t.name.clone(),
                    slug: t.name.to_lowercase(),
                    description: t.description.clone(),
                    privacy: "closed".into(),
                    members: t
                        .maintainers
                        .iter()
                        .map(|m| (m.to_lowercase(), "maintainer".to_owned()))
                        .chain(t.members.iter().map(|m| (m.to_lowercase(), "member".to_owned())))
                        .collect(),
                    repos: t.repos.clone(),
                })
                .collect(),
            repos: spec.repos.iter().map(|r| r.name.clone()).chain(["homestead".into()]).collect(),
            // GitHub adds fields and nests objects; converged means covered.
            repo_details: spec
                .repos
                .iter()
                .map(|r| {
                    let mut v = json!({ "id": 1, "name": r.name, "private": true, "has_issues": true });
                    for (k, want) in &r.settings {
                        v[k.as_str()] = want.clone();
                    }
                    (r.name.to_lowercase(), v)
                })
                .collect(),
        };
        for (k, v) in &o.settings {
            s.org[k.as_str()] = v.clone();
        }
        s
    }

    #[test]
    fn shipped_spec_is_valid_and_locks_down_defaults() {
        let spec = Spec::parse(SPEC).unwrap_or_else(|e| panic!("github/org.toml is invalid:\n{e}"));
        assert_eq!(spec.org.default_repository_permission, "none", "proprietary code: no default read access");
        assert!(!spec.org.members_can_create_public_repositories);
    }

    #[test]
    fn plan_from_today_and_convergence() {
        let spec = Spec::parse(SPEC).expect("spec");
        let shown: Vec<String> = plan(&spec, &today()).iter().map(ToString::to_string).collect();
        assert_eq!(
            shown,
            [
                "~ org settings (name, description, website, location, default repo permission, public repo creation, \
                 private repo creation)",
                "+ team \"Stewards\"",
                "~ team \"Stewards\": @cameronaaron as maintainer",
                "~ team \"Stewards\": homestead → admin",
                "+ team \"Tech\"",
                "~ team \"Tech\": @cameronaaron as maintainer",
                "~ team \"Tech\": homestead → maintain",
            ]
        );
        assert!(plan(&spec, &converged(&spec)).is_empty());
    }

    #[test]
    fn inviting_is_explicit_and_not_repeated() {
        let mut spec = Spec::parse(SPEC).expect("spec");
        spec.members.push(MemberSpec { login: "ava".into(), role: "member".into() });
        spec.teams[0].members.push("ava".into());
        let mut state = converged(&Spec::parse(SPEC).expect("spec"));
        let shown: Vec<String> = plan(&spec, &state).iter().map(ToString::to_string).collect();
        assert_eq!(shown, ["+ invite @ava to the org as member (sends an email)"], "no team add until they accept");
        state.invited.insert("ava".into());
        assert!(plan(&spec, &state).is_empty(), "a pending invitation isn't re-sent");
    }

    #[test]
    fn advisories_cover_what_the_api_cannot_do() {
        let spec = Spec::parse(SPEC).expect("spec");
        let a = advisories(&spec, &today());
        assert!(a.iter().any(|f| matches!(f, Finding::Pending(m) if m.contains("two-factor"))));
        assert!(a.iter().any(|f| matches!(f, Finding::Note(m) if m.contains("demo-repository"))));
    }

    #[test]
    fn a_free_org_cannot_be_asked_for_private_only_repo_creation() {
        let mut state = today();
        state.org["plan"] = json!({ "name": "free" });
        let repo_spec = Spec::parse(SPEC).expect("spec");
        assert_eq!(plan_limit(&repo_spec, &state), None, "github/org.toml must fit the org's free plan");

        let mut private_only = repo_spec;
        private_only.org.members_can_create_public_repositories = false;
        private_only.org.members_can_create_private_repositories = true;
        assert!(plan_limit(&private_only, &state).is_some_and(|m| m.contains("free plan")));
        assert!(
            advisories(&private_only, &state)
                .iter()
                .any(|f| matches!(f, Finding::Violation(m) if m.contains("free plan")))
        );
        state.org["plan"] = json!({ "name": "team" });
        assert_eq!(plan_limit(&private_only, &state), None, "paid plans allow private-only");
    }

    #[test]
    fn rejects_team_members_missing_from_the_org_list() {
        let bad = SPEC.replace(
            "maintainers = [\"cameronaaron\"]\nmembers = []\nrepos = { homestead = \"maintain\" }",
            "maintainers = [\"cameronaaron\"]\nmembers = [\"stranger\"]\nrepos = { homestead = \"maintain\" }",
        );
        assert!(Spec::parse(&bad).expect_err("stranger").contains("must also be listed"));
        assert!(
            Spec::parse(&SPEC.replace("\"admin\" }", "\"owner\" }")).expect_err("access").contains("access must be")
        );
    }

    #[test]
    fn repos_are_created_then_kept_to_their_settings() {
        let mut spec = Spec::parse(SPEC).expect("spec");
        spec.repos
            .push(RepoSpec { name: "farm-photos".into(), settings: [("has_wiki".to_owned(), json!(false))].into() });
        let mut state = converged(&Spec::parse(SPEC).expect("spec"));
        let shown: Vec<String> = plan(&spec, &state).iter().map(ToString::to_string).collect();
        assert_eq!(shown, ["+ private repo farm-photos"]);

        state.repos.push("farm-photos".into());
        state.repo_details.insert(
            "farm-photos".into(),
            json!({ "has_wiki": true, "security_and_analysis": { "secret_scanning": { "status": "enabled" } } }),
        );
        spec.repos
            .last_mut()
            .expect("repo")
            .settings
            .insert("security_and_analysis".into(), json!({ "secret_scanning": { "status": "enabled" } }));
        let shown: Vec<String> = plan(&spec, &state).iter().map(ToString::to_string).collect();
        assert_eq!(shown, ["~ repo farm-photos (has_wiki)"], "a nested object GitHub already matches is not drift");
    }

    #[test]
    fn org_settings_pass_through_and_owned_fields_are_refused() {
        let mut spec = Spec::parse(SPEC).expect("spec");
        spec.org.settings.insert("web_commit_signoff_required".into(), json!(true));
        let shown: Vec<String> =
            plan(&spec, &converged(&Spec::parse(SPEC).expect("spec"))).iter().map(ToString::to_string).collect();
        assert_eq!(shown, ["~ org settings (web_commit_signoff_required)"]);
        let owned = SPEC.replace("web_commit_signoff_required = false", "blog = \"x\"");
        assert!(Spec::parse(&owned).expect_err("owned").contains("its own field"));
    }

    #[test]
    fn read_and_write_map_to_the_write_endpoint_names() {
        assert_eq!(permission_param("read"), "pull");
        assert_eq!(permission_param("write"), "push");
        assert_eq!(permission_param("maintain"), "maintain");
    }
}

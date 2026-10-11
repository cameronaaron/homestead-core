//! Declarative Linear workspace provisioning (`linear/workspace.toml`).
//!
//! [`plan`] diffs the spec against the live team; [`apply`] executes the
//! plan through the GraphQL API and re-plans to confirm convergence. Matching
//! is by name, and nothing outside the spec is deleted or renamed.
//!
//! `[members]` (email → role) invites missing people to the workspace and
//! the team, and keeps listed users' roles and team membership in line.
//! Unlisted users are reported, never removed; owners are never demoted.

use serde::Deserialize;
use serde_json::{Value, json};

use super::DEFAULT_ENDPOINT;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub team: TeamSpec,
    #[serde(default)]
    pub states: Vec<StateSpec>,
    #[serde(default)]
    pub label_groups: Vec<GroupSpec>,
    #[serde(default)]
    pub projects: Vec<ProjectSpec>,
    /// Email → workspace role.
    #[serde(default)]
    pub members: std::collections::BTreeMap<String, Role>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Member,
    Guest,
}

impl Role {
    /// Linear's `UserRoleType`.
    pub fn api(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Member => "user",
            Role::Guest => "guest",
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Role::Admin => "admin",
            Role::Member => "member",
            Role::Guest => "guest",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamSpec {
    pub key: String,
    pub name: String,
    pub description: String,
    pub timezone: String,
    /// Any other `TeamUpdateInput` field, by its API name (e.g.
    /// `cyclesEnabled`). A `…StateId` may name a workflow state.
    #[serde(default)]
    pub settings: std::collections::BTreeMap<String, Value>,
}

/// Team fields the typed spec owns.
const TEAM_OWNED: &[&str] = &["name", "description", "key", "timezone"];

/// A setting that names a workflow state instead of giving its id.
fn state_ref<'a>(key: &str, v: &'a Value) -> Option<&'a str> {
    v.as_str().filter(|s| key.ends_with("StateId") && !is_uuid(s))
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub color: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupSpec {
    pub name: String,
    pub color: String,
    #[serde(default)]
    pub description: String,
    pub labels: Vec<LabelSpec>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelSpec {
    pub name: String,
    pub color: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSpec {
    pub name: String,
    pub status: String,
    pub color: String,
    pub description: String,
    /// Seeds the project document on creation only.
    pub content: Option<String>,
}

const STATE_TYPES: &[&str] = &["triage", "backlog", "unstarted", "started", "completed", "canceled"];

fn is_hex_color(c: &str) -> bool {
    c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|x| x.is_ascii_hexdigit())
}

impl Spec {
    pub fn parse(text: &str) -> Result<Self, String> {
        let spec: Spec = toml::from_str(text).map_err(|e| e.to_string())?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> Result<(), String> {
        let mut e = Vec::new();
        let mut colors: Vec<(&str, &str)> = Vec::new();
        for s in &self.states {
            if !STATE_TYPES.contains(&s.kind.as_str()) {
                e.push(format!("state {:?}: type must be one of {STATE_TYPES:?}", s.name));
            }
            colors.push((&s.name, &s.color));
        }
        let mut names = std::collections::BTreeSet::new();
        for g in &self.label_groups {
            colors.push((&g.name, &g.color));
            if !names.insert(g.name.to_lowercase()) {
                e.push(format!("label name {:?} is used twice", g.name));
            }
            for l in &g.labels {
                colors.push((&l.name, &l.color));
                // Linear label names must be unique within a team.
                if !names.insert(l.name.to_lowercase()) {
                    e.push(format!("label name {:?} is used twice", l.name));
                }
            }
        }
        let mut projects = std::collections::BTreeSet::new();
        for p in &self.projects {
            colors.push((&p.name, &p.color));
            if !projects.insert(p.name.as_str()) {
                e.push(format!("project {:?} is listed twice", p.name));
            }
        }
        for (k, v) in &self.team.settings {
            if TEAM_OWNED.contains(&k.as_str()) {
                e.push(format!("team settings: {k:?} is set by its own field under [team], not settings"));
            }
            let mut chars = k.chars();
            if !chars.next().is_some_and(|c| c.is_ascii_lowercase()) || !chars.all(|c| c.is_ascii_alphanumeric()) {
                e.push(format!("team settings: {k:?} is not a Linear API field name (camelCase)"));
            }
            if v.is_object() || v.is_array() {
                e.push(format!("team settings: {k} must be a plain value (text, number, true/false)"));
            }
        }
        let mut emails = std::collections::BTreeSet::new();
        for email in self.members.keys() {
            let (user, domain) = email.split_once('@').unwrap_or_default();
            if user.is_empty() || !domain.contains('.') || email.contains(char::is_whitespace) {
                e.push(format!("member {email:?} is not an email address"));
            }
            if !emails.insert(email.to_lowercase()) {
                e.push(format!("member {email:?} is listed twice"));
            }
        }
        for (who, c) in colors {
            if !is_hex_color(c) {
                e.push(format!("{who:?}: color must be #RRGGBB"));
            }
        }
        if e.is_empty() { Ok(()) } else { Err(e.join("\n")) }
    }
}

// ── Live state ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub team_id: String,
    pub team_name: String,
    pub team_description: String,
    pub timezone: String,
    pub states: Vec<LiveState>,
    pub labels: Vec<LiveLabel>,
    pub projects: Vec<LiveProject>,
    /// Project status name → id.
    pub project_statuses: Vec<(String, String)>,
    /// People in the workspace (apps and integrations excluded).
    pub users: Vec<LiveUser>,
    /// User ids on the team.
    pub team_members: Vec<String>,
    /// Emails with an invite that is neither accepted nor expired.
    pub invites: Vec<String>,
    /// Live values of the team fields named in `[team.settings]`.
    pub team_settings: std::collections::BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveUser {
    pub id: String,
    pub email: String,
    pub role: Role,
    pub owner: bool,
    /// False once suspended.
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveState {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub color: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveLabel {
    pub id: String,
    pub name: String,
    pub color: String,
    pub description: String,
    pub is_group: bool,
    pub parent: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveProject {
    pub id: String,
    pub name: String,
    pub color: String,
    pub description: String,
    pub status: String,
}

// ── Plan ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    UpdateTeam {
        changed: Vec<&'static str>,
    },
    /// After workflow states, which a setting may name.
    UpdateTeamSettings {
        changed: Vec<String>,
    },
    CreateState(StateSpec),
    UpdateState {
        id: String,
        spec: StateSpec,
        changed: Vec<&'static str>,
    },
    CreateLabel {
        spec: LabelSpec,
        group: Option<String>,
    },
    UpdateLabel {
        id: String,
        spec: LabelSpec,
        group: Option<String>,
        changed: Vec<&'static str>,
    },
    CreateProject(ProjectSpec),
    UpdateProject {
        id: String,
        spec: ProjectSpec,
        changed: Vec<&'static str>,
    },
    Invite {
        email: String,
        role: Role,
    },
    ChangeRole {
        id: String,
        email: String,
        role: Role,
    },
    JoinTeam {
        id: String,
        email: String,
    },
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = |spec: &LabelSpec, group: &Option<String>| match group {
            Some(g) => format!("{g} → {}", spec.name),
            None => spec.name.clone(),
        };
        match self {
            Action::UpdateTeam { changed } => write!(f, "~ team ({})", changed.join(", ")),
            Action::UpdateTeamSettings { changed } => write!(f, "~ team settings ({})", changed.join(", ")),
            Action::CreateState(s) => write!(f, "+ workflow state {:?}", s.name),
            Action::UpdateState { spec, changed, .. } => {
                write!(f, "~ workflow state {:?} ({})", spec.name, changed.join(", "))
            }
            Action::CreateLabel { spec, group } => write!(f, "+ label {:?}", label(spec, group)),
            Action::UpdateLabel { spec, group, changed, .. } => {
                write!(f, "~ label {:?} ({})", label(spec, group), changed.join(", "))
            }
            Action::CreateProject(p) => write!(f, "+ project {:?}", p.name),
            Action::UpdateProject { spec, changed, .. } => {
                write!(f, "~ project {:?} ({})", spec.name, changed.join(", "))
            }
            Action::Invite { email, role } => write!(f, "+ invite {email} as {role}"),
            Action::ChangeRole { email, role, .. } => write!(f, "~ {email} becomes {role}"),
            Action::JoinTeam { email, .. } => write!(f, "+ {email} joins the team"),
        }
    }
}

fn same_color(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn label_changes(live: &LiveLabel, spec: &LabelSpec, is_group: bool, parent: Option<&str>) -> Vec<&'static str> {
    let mut c = Vec::new();
    if !same_color(&live.color, &spec.color) {
        c.push("color");
    }
    if live.description.trim() != spec.description.trim() {
        c.push("description");
    }
    if live.is_group != is_group {
        c.push("group");
    }
    if live.parent.as_deref() != parent {
        c.push("parent");
    }
    c
}

pub fn plan(spec: &Spec, state: &State) -> Vec<Action> {
    let mut out = Vec::new();
    let t = &spec.team;
    let mut changed = Vec::new();
    if state.team_name != t.name {
        changed.push("name");
    }
    if state.team_description.trim() != t.description.trim() {
        changed.push("description");
    }
    if state.timezone != t.timezone {
        changed.push("timezone");
    }
    if !changed.is_empty() {
        out.push(Action::UpdateTeam { changed });
    }

    for s in &spec.states {
        match state.states.iter().find(|l| l.name == s.name) {
            None => out.push(Action::CreateState(s.clone())),
            Some(l) => {
                let mut c = Vec::new();
                if !same_color(&l.color, &s.color) {
                    c.push("color");
                }
                if l.description.trim() != s.description.trim() {
                    c.push("description");
                }
                if l.kind != s.kind {
                    // Linear can't change a state's type; surface it rather than loop.
                    c.push("type (change it by hand in Linear)");
                }
                if !c.is_empty() {
                    out.push(Action::UpdateState { id: l.id.clone(), spec: s.clone(), changed: c });
                }
            }
        }
    }

    let resolved = team_settings(spec, state);
    let changed: Vec<String> = spec
        .team
        .settings
        .keys()
        .filter(|k| {
            let live = state.team_settings.get(*k).unwrap_or(&Value::Null);
            !resolved.as_ref().and_then(|r| r.get(*k)).is_some_and(|want| crate::settings::covers(want, live))
        })
        .cloned()
        .collect();
    if !changed.is_empty() {
        out.push(Action::UpdateTeamSettings { changed });
    }

    let find = |name: &str| state.labels.iter().find(|l| l.name.eq_ignore_ascii_case(name));
    for g in &spec.label_groups {
        let group = LabelSpec { name: g.name.clone(), color: g.color.clone(), description: g.description.clone() };
        match find(&g.name) {
            None => out.push(Action::CreateLabel { spec: group, group: None }),
            Some(l) => {
                let c = label_changes(l, &group, true, None);
                if !c.is_empty() {
                    out.push(Action::UpdateLabel { id: l.id.clone(), spec: group, group: None, changed: c });
                }
            }
        }
        for child in &g.labels {
            match find(&child.name) {
                None => out.push(Action::CreateLabel { spec: child.clone(), group: Some(g.name.clone()) }),
                Some(l) => {
                    let c = label_changes(l, child, false, Some(&g.name));
                    if !c.is_empty() {
                        out.push(Action::UpdateLabel {
                            id: l.id.clone(),
                            spec: child.clone(),
                            group: Some(g.name.clone()),
                            changed: c,
                        });
                    }
                }
            }
        }
    }

    for p in &spec.projects {
        match state.projects.iter().find(|l| l.name == p.name) {
            None => out.push(Action::CreateProject(p.clone())),
            Some(l) => {
                let mut c = Vec::new();
                if !same_color(&l.color, &p.color) {
                    c.push("color");
                }
                if l.description.trim() != p.description.trim() {
                    c.push("description");
                }
                if l.status != p.status {
                    c.push("status");
                }
                if !c.is_empty() {
                    out.push(Action::UpdateProject { id: l.id.clone(), spec: p.clone(), changed: c });
                }
            }
        }
    }

    for (email, &role) in &spec.members {
        let Some(u) = user(state, email) else {
            if !state.invites.iter().any(|i| i.eq_ignore_ascii_case(email)) {
                out.push(Action::Invite { email: email.clone(), role });
            }
            continue;
        };
        if !u.active {
            continue; // reported by `people`; unsuspending is a human call
        }
        if u.role != role && !u.owner {
            out.push(Action::ChangeRole { id: u.id.clone(), email: email.clone(), role });
        }
        if !state.team_members.contains(&u.id) {
            out.push(Action::JoinTeam { id: u.id.clone(), email: email.clone() });
        }
    }
    out
}

/// `[team.settings]` with workflow state names resolved to ids; `None`
/// while a named state doesn't exist yet.
fn team_settings(spec: &Spec, state: &State) -> Option<std::collections::BTreeMap<String, Value>> {
    spec.team
        .settings
        .iter()
        .map(|(k, v)| {
            let v = match state_ref(k, v) {
                Some(name) => json!(state.states.iter().find(|s| s.name == name)?.id),
                None => v.clone(),
            };
            Some((k.clone(), v))
        })
        .collect()
}

fn user<'a>(state: &'a State, email: &str) -> Option<&'a LiveUser> {
    state.users.iter().find(|u| u.email.eq_ignore_ascii_case(email))
}

/// What the plan can't act on: invites waiting to be accepted, suspended
/// or owner accounts that differ from the spec, and users the spec doesn't
/// list (left alone).
pub fn people(spec: &Spec, state: &State) -> Vec<String> {
    let mut out = Vec::new();
    for (email, &role) in &spec.members {
        // An open invite comes first: Linear also lists the invitee as an
        // inactive user, which would otherwise read as suspended.
        if state.invites.iter().any(|i| i.eq_ignore_ascii_case(email)) {
            out.push(format!("… {email} is invited; waiting for them to accept"));
            continue;
        }
        match user(state, email) {
            Some(u) if !u.active => out.push(format!("! {email} is suspended in Linear; unsuspend them by hand")),
            Some(u) if u.owner && role != Role::Admin => {
                out.push(format!("! {email} owns the workspace, so stays owner (listed as {role})"));
            }
            _ => {}
        }
    }
    for u in &state.users {
        if u.active && !spec.members.keys().any(|e| e.eq_ignore_ascii_case(&u.email)) {
            out.push(format!("? unmanaged member {} ({}) (left alone)", u.email, u.role));
        }
    }
    out
}

// ── GraphQL client ────────────────────────────────────────────────────────────

pub struct Client {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl Client {
    pub fn new(http: reqwest::Client, endpoint: Option<String>, api_key: String) -> Self {
        Self { http, endpoint: endpoint.unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned()), api_key }
    }

    async fn gql(&self, query: &str, variables: Value) -> Result<Value, String> {
        for attempt in 0..4 {
            let resp = self
                .http
                .post(&self.endpoint)
                .header("Authorization", &self.api_key)
                .json(&json!({ "query": query, "variables": variables }))
                .send()
                .await
                .map_err(|e| format!("Linear: {e}"))?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let rate_limited =
                v["errors"].as_array().is_some_and(|es| es.iter().any(|e| e["extensions"]["code"] == "RATELIMITED"));
            if rate_limited && attempt < 3 {
                tokio::time::sleep(std::time::Duration::from_secs(2 << attempt)).await;
                continue;
            }
            if let Some(errors) = v["errors"].as_array() {
                let msgs: Vec<&str> = errors.iter().filter_map(|e| e["message"].as_str()).collect();
                return Err(format!("Linear HTTP {status}: {}", msgs.join("; ")));
            }
            if !status.is_success() {
                return Err(format!("Linear HTTP {status}: {}", text.chars().take(300).collect::<String>()));
            }
            return Ok(v["data"].clone());
        }
        Err("Linear: still rate limited".to_owned())
    }

    /// Reads the team in several small queries: Linear rejects a single
    /// query whose combined page sizes exceed its complexity budget.
    pub async fn fetch(&self, team_key: &str) -> Result<State, String> {
        let s = |v: &Value| v.as_str().unwrap_or_default().to_owned();
        let nodes = |v: &Value| v["nodes"].as_array().cloned().unwrap_or_default();
        let d = self
            .gql(
                "query($key: String!) { teams(filter: { key: { eq: $key } }) { nodes {
                   id name description timezone
                   states(first: 50) { nodes { id name type color description } }
                 } } }",
                json!({ "key": team_key }),
            )
            .await?;
        let team = d["teams"]["nodes"]
            .as_array()
            .and_then(|a| a.first())
            .cloned()
            .ok_or_else(|| format!("no Linear team with key {team_key:?} (check LINEAR_API_KEY's workspace)"))?;
        let id = s(&team["id"]);
        let labels = self
            .gql(
                "query($id: String!) { team(id: $id) { labels(first: 100) { nodes { id name color description isGroup parent { name } } } } }",
                json!({ "id": id }),
            )
            .await?;
        let projects = self
            .gql(
                "query($id: String!) { team(id: $id) { projects(first: 50) { nodes { id name color description status { name } } } } }",
                json!({ "id": id }),
            )
            .await?;
        let statuses = self.gql("{ projectStatuses { nodes { id name } } }", json!({})).await?;
        let people = self
            .gql(
                "{ users(first: 100, includeDisabled: true) { nodes { id email active admin guest owner app } } }",
                json!({}),
            )
            .await?;
        let invites =
            self.gql("{ organizationInvites(first: 100) { nodes { email acceptedAt expiresAt } } }", json!({})).await?;
        let members = self
            .gql("query($id: String!) { team(id: $id) { members(first: 100) { nodes { id } } } }", json!({ "id": id }))
            .await?;
        let now = time::OffsetDateTime::now_utc();
        let flag = |v: &Value| v.as_bool().unwrap_or(false);
        Ok(State {
            team_id: id,
            team_name: s(&team["name"]),
            team_description: s(&team["description"]),
            timezone: s(&team["timezone"]),
            states: nodes(&team["states"])
                .iter()
                .map(|n| LiveState {
                    id: s(&n["id"]),
                    name: s(&n["name"]),
                    kind: s(&n["type"]),
                    color: s(&n["color"]),
                    description: s(&n["description"]),
                })
                .collect(),
            labels: nodes(&labels["team"]["labels"])
                .iter()
                .map(|n| LiveLabel {
                    id: s(&n["id"]),
                    name: s(&n["name"]),
                    color: s(&n["color"]),
                    description: s(&n["description"]),
                    is_group: n["isGroup"].as_bool().unwrap_or(false),
                    parent: n["parent"]["name"].as_str().map(str::to_owned),
                })
                .collect(),
            projects: nodes(&projects["team"]["projects"])
                .iter()
                .map(|n| LiveProject {
                    id: s(&n["id"]),
                    name: s(&n["name"]),
                    color: s(&n["color"]),
                    description: s(&n["description"]),
                    status: s(&n["status"]["name"]),
                })
                .collect(),
            project_statuses: nodes(&statuses["projectStatuses"])
                .iter()
                .map(|n| (s(&n["name"]), s(&n["id"])))
                .collect(),
            users: nodes(&people["users"])
                .iter()
                .filter(|n| !flag(&n["app"]))
                .map(|n| LiveUser {
                    id: s(&n["id"]),
                    email: s(&n["email"]),
                    // Owners are admins too.
                    role: if flag(&n["admin"]) || flag(&n["owner"]) {
                        Role::Admin
                    } else if flag(&n["guest"]) {
                        Role::Guest
                    } else {
                        Role::Member
                    },
                    owner: flag(&n["owner"]),
                    active: flag(&n["active"]),
                })
                .collect(),
            team_members: nodes(&members["team"]["members"]).iter().map(|n| s(&n["id"])).collect(),
            invites: nodes(&invites["organizationInvites"])
                .iter()
                .filter(|n| n["acceptedAt"].is_null())
                .filter(|n| {
                    n["expiresAt"].as_str().is_none_or(|t| {
                        time::OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339)
                            .is_ok_and(|t| t > now)
                    })
                })
                .map(|n| s(&n["email"]))
                .collect(),
            team_settings: Default::default(),
        })
    }

    /// [`Client::fetch`], plus the live values of `[team.settings]`. Field
    /// names are validated identifiers, so building the query is safe; an
    /// unknown one fails here, by name.
    pub async fn fetch_spec(&self, spec: &Spec) -> Result<State, String> {
        let mut state = self.fetch(&spec.team.key).await?;
        if spec.team.settings.is_empty() {
            return Ok(state);
        }
        let fields: Vec<&str> = spec.team.settings.keys().map(String::as_str).collect();
        let query = format!("query($id: String!) {{ team(id: $id) {{ {} }} }}", fields.join(" "));
        let d = self.gql(&query, json!({ "id": state.team_id })).await.map_err(|e| format!("[team.settings]: {e}"))?;
        state.team_settings = fields.iter().map(|f| ((*f).to_owned(), d["team"][*f].clone())).collect();
        Ok(state)
    }

    /// Looks a label up fresh: a group created earlier in this pass isn't
    /// in the state the plan was computed from.
    async fn label_id(&self, team_key: &str, name: &str) -> Result<String, String> {
        self.fetch(team_key)
            .await?
            .labels
            .into_iter()
            .find(|l| l.name.eq_ignore_ascii_case(name))
            .map(|l| l.id)
            .ok_or_else(|| format!("label group {name:?} not found"))
    }

    async fn execute(&self, spec: &Spec, state: &State, action: &Action) -> Result<(), String> {
        let status_id = |name: &str| {
            state
                .project_statuses
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, id)| id.clone())
                .ok_or_else(|| format!("no project status named {name:?}"))
        };
        match action {
            Action::UpdateTeam { .. } => {
                let t = &spec.team;
                self.gql(
                    "mutation($id: String!, $input: TeamUpdateInput!) { teamUpdate(id: $id, input: $input) { success } }",
                    json!({ "id": state.team_id, "input": { "name": t.name, "description": t.description, "timezone": t.timezone } }),
                )
                .await?;
            }
            Action::UpdateTeamSettings { .. } => {
                // Fresh: a state this pass created isn't in `state` yet.
                let fresh = self.fetch(&spec.team.key).await?;
                let input =
                    team_settings(spec, &fresh).ok_or("[team.settings] names a workflow state that doesn't exist")?;
                self.gql(
                    "mutation($id: String!, $input: TeamUpdateInput!) { teamUpdate(id: $id, input: $input) { success } }",
                    json!({ "id": state.team_id, "input": input }),
                )
                .await?;
            }
            Action::CreateState(s) => {
                self.gql(
                    "mutation($input: WorkflowStateCreateInput!) { workflowStateCreate(input: $input) { success } }",
                    json!({ "input": { "teamId": state.team_id, "name": s.name, "type": s.kind, "color": s.color, "description": s.description } }),
                )
                .await?;
            }
            Action::UpdateState { id, spec: s, .. } => {
                self.gql(
                    "mutation($id: String!, $input: WorkflowStateUpdateInput!) { workflowStateUpdate(id: $id, input: $input) { success } }",
                    json!({ "id": id, "input": { "color": s.color, "description": s.description } }),
                )
                .await?;
            }
            Action::CreateLabel { spec: l, group } => {
                let mut input =
                    json!({ "teamId": state.team_id, "name": l.name, "color": l.color, "description": l.description });
                match group {
                    Some(g) => input["parentId"] = json!(self.label_id(&spec.team.key, g).await?),
                    None => input["isGroup"] = json!(true),
                }
                self.gql(
                    "mutation($input: IssueLabelCreateInput!) { issueLabelCreate(input: $input) { success } }",
                    json!({ "input": input }),
                )
                .await?;
            }
            Action::UpdateLabel { id, spec: l, group, .. } => {
                let mut input = json!({ "color": l.color, "description": l.description });
                match group {
                    Some(g) => input["parentId"] = json!(self.label_id(&spec.team.key, g).await?),
                    None => input["isGroup"] = json!(true),
                }
                self.gql(
                    "mutation($id: String!, $input: IssueLabelUpdateInput!) { issueLabelUpdate(id: $id, input: $input) { success } }",
                    json!({ "id": id, "input": input }),
                )
                .await?;
            }
            Action::CreateProject(p) => {
                let mut input = json!({
                    "name": p.name, "color": p.color, "description": p.description,
                    "teamIds": [state.team_id], "statusId": status_id(&p.status)?,
                });
                if let Some(c) = &p.content {
                    input["content"] = json!(c.trim());
                }
                self.gql(
                    "mutation($input: ProjectCreateInput!) { projectCreate(input: $input) { success } }",
                    json!({ "input": input }),
                )
                .await?;
            }
            Action::UpdateProject { id, spec: p, .. } => {
                self.gql(
                    "mutation($id: String!, $input: ProjectUpdateInput!) { projectUpdate(id: $id, input: $input) { success } }",
                    json!({ "id": id, "input": { "color": p.color, "description": p.description, "statusId": status_id(&p.status)? } }),
                )
                .await?;
            }
            Action::Invite { email, role } => {
                self.gql(
                    "mutation($input: OrganizationInviteCreateInput!) { organizationInviteCreate(input: $input) { success } }",
                    json!({ "input": { "email": email, "role": role.api(), "teamIds": [state.team_id] } }),
                )
                .await?;
            }
            Action::ChangeRole { id, role, .. } => {
                self.gql(
                    "mutation($id: String!, $role: UserRoleType!) { userChangeRole(id: $id, role: $role) { success } }",
                    json!({ "id": id, "role": role.api() }),
                )
                .await?;
            }
            Action::JoinTeam { id, .. } => {
                self.gql(
                    "mutation($input: TeamMembershipCreateInput!) { teamMembershipCreate(input: $input) { success } }",
                    json!({ "input": { "teamId": state.team_id, "userId": id } }),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Applies the plan, then re-plans. Returns the executed actions, or an
    /// error listing whatever still differs.
    pub async fn apply(&self, spec: &Spec) -> Result<Vec<String>, String> {
        let mut done = Vec::new();
        // Two passes: label children need their group to exist first.
        for _ in 0..3 {
            let state = self.fetch_spec(spec).await?;
            let actions = plan(spec, &state);
            if actions.is_empty() {
                return Ok(done);
            }
            for a in &actions {
                self.execute(spec, &state, a).await.map_err(|e| format!("{a}: {e}"))?;
                done.push(a.to_string());
            }
        }
        let left = plan(spec, &self.fetch_spec(spec).await?);
        if left.is_empty() {
            Ok(done)
        } else {
            Err(format!(
                "Linear did not converge; still differs:\n{}",
                left.iter().map(|a| format!("  {a}")).collect::<Vec<_>>().join("\n")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = include_str!("../../../../linear/workspace.toml");

    /// The state Linear would hold once the spec is applied.
    fn converged(spec: &Spec) -> State {
        let mut labels = Vec::new();
        for g in &spec.label_groups {
            labels.push(LiveLabel {
                id: g.name.clone(),
                name: g.name.clone(),
                color: g.color.to_lowercase(),
                description: g.description.clone(),
                is_group: true,
                parent: None,
            });
            for l in &g.labels {
                labels.push(LiveLabel {
                    id: l.name.clone(),
                    name: l.name.clone(),
                    color: l.color.clone(),
                    description: l.description.clone(),
                    is_group: false,
                    parent: Some(g.name.clone()),
                });
            }
        }
        let mut s = State {
            team_id: "t".into(),
            team_name: spec.team.name.clone(),
            team_description: spec.team.description.clone(),
            timezone: spec.team.timezone.clone(),
            states: spec
                .states
                .iter()
                .map(|s| LiveState {
                    id: s.name.clone(),
                    name: s.name.clone(),
                    kind: s.kind.clone(),
                    color: s.color.clone(),
                    description: s.description.clone(),
                })
                .collect(),
            labels,
            projects: spec
                .projects
                .iter()
                .map(|p| LiveProject {
                    id: p.name.clone(),
                    name: p.name.clone(),
                    color: p.color.clone(),
                    description: p.description.clone(),
                    status: p.status.clone(),
                })
                .collect(),
            project_statuses: vec![],
            users: spec
                .members
                .iter()
                .map(|(email, &role)| LiveUser {
                    id: email.clone(),
                    email: email.to_uppercase(),
                    role,
                    owner: false,
                    active: true,
                })
                .collect(),
            team_members: spec.members.keys().cloned().collect(),
            invites: vec![],
            team_settings: Default::default(),
        };
        // Linear's built-in states, which settings may name.
        for (name, kind) in [("Canceled", "canceled"), ("Done", "completed")] {
            s.states.push(LiveState {
                id: format!("id-{name}"),
                name: name.into(),
                kind: kind.into(),
                color: String::new(),
                description: String::new(),
            });
        }
        s.team_settings = team_settings(spec, &s).expect("settings resolve");
        // Linear reports Floats however it likes; `same` must not care.
        for v in s.team_settings.values_mut() {
            if let Some(n) = v.as_i64() {
                *v = json!(n as f64);
            }
        }
        s
    }

    #[test]
    fn shipped_spec_is_valid() {
        Spec::parse(SPEC).unwrap_or_else(|e| panic!("linear/workspace.toml is invalid:\n{e}"));
    }

    #[test]
    fn empty_team_plans_everything_and_converged_team_plans_nothing() {
        let spec = Spec::parse(SPEC).expect("spec");
        let fresh =
            State { team_name: spec.team.name.clone(), timezone: spec.team.timezone.clone(), ..State::default() };
        let actions = plan(&spec, &fresh);
        let labels: usize = spec.label_groups.iter().map(|g| 1 + g.labels.len()).sum();
        let people = spec.members.len();
        let settings = usize::from(!spec.team.settings.is_empty());
        assert_eq!(
            actions.len(),
            1 + settings + spec.states.len() + labels + spec.projects.len() + people,
            "{actions:#?}"
        );
        assert!(plan(&spec, &converged(&spec)).is_empty(), "colour case, email case and 6 vs 6.0 must not cause drift");
    }

    #[test]
    fn drift_is_named_precisely_and_extras_are_ignored() {
        let spec = Spec::parse(SPEC).expect("spec");
        let mut s = converged(&spec);
        s.labels.iter_mut().find(|l| l.name == "Garden").expect("garden").parent = None;
        s.projects[0].status = "Completed".into();
        s.labels.push(LiveLabel {
            id: "b".into(),
            name: "Bug".into(),
            color: "#EB5757".into(),
            description: String::new(),
            is_group: false,
            parent: None,
        });
        let shown: Vec<String> = plan(&spec, &s).iter().map(ToString::to_string).collect();
        assert_eq!(shown, [r#"~ label "Area → Garden" (parent)"#, r#"~ project "Grow the Valley campaign" (status)"#]);
    }

    #[test]
    fn rejects_duplicate_labels_and_bad_colors() {
        let dup = SPEC.replace("name = \"Tech\"", "name = \"Garden\"");
        assert!(Spec::parse(&dup).expect_err("dup").contains("used twice"));
        let bad = SPEC.replace("color = \"#5E6AD2\"", "color = \"blue\"");
        assert!(Spec::parse(&bad).expect_err("color").contains("#RRGGBB"));
    }

    fn people_spec() -> Spec {
        let mut spec = Spec::parse(SPEC).expect("spec");
        spec.members = [
            ("new@example.org", Role::Member),
            ("invited@example.org", Role::Guest),
            ("promote@example.org", Role::Admin),
            ("off-team@example.org", Role::Member),
            ("boss@example.org", Role::Member),
            ("away@example.org", Role::Member),
        ]
        .into_iter()
        .map(|(e, r)| (e.to_owned(), r))
        .collect();
        spec
    }

    #[test]
    fn members_are_invited_promoted_and_added_to_the_team() {
        let spec = people_spec();
        let mut s = converged(&Spec { members: Default::default(), ..spec.clone() });
        let u =
            |email: &str, role, owner, active| LiveUser { id: email.into(), email: email.into(), role, owner, active };
        s.users = vec![
            u("promote@example.org", Role::Member, false, true),
            u("off-team@example.org", Role::Member, false, true),
            u("boss@example.org", Role::Admin, true, true),
            u("away@example.org", Role::Admin, false, false),
            u("stray@example.org", Role::Member, false, true),
            // Linear adds an inactive placeholder user the moment it sends an
            // invite (observed 2026-09-29), so pending reads like suspended.
            u("invited@example.org", Role::Member, false, false),
        ];
        s.team_members = vec!["promote@example.org".into(), "boss@example.org".into()];
        s.invites = vec!["Invited@Example.org".into()];

        let shown: Vec<String> = plan(&spec, &s).iter().map(ToString::to_string).collect();
        assert_eq!(
            shown,
            [
                "+ invite new@example.org as member",
                "+ off-team@example.org joins the team",
                "~ promote@example.org becomes admin",
            ],
            "the owner is never demoted, a pending invite is not re-sent, a suspended user is not touched"
        );
        assert_eq!(
            people(&spec, &s),
            [
                "! away@example.org is suspended in Linear; unsuspend them by hand",
                "! boss@example.org owns the workspace, so stays owner (listed as member)",
                "… invited@example.org is invited; waiting for them to accept",
                "? unmanaged member stray@example.org (member) (left alone)",
            ]
        );
    }

    #[test]
    fn team_settings_drift_is_named_and_states_are_named_not_ided() {
        let spec = Spec::parse(SPEC).expect("spec");
        let mut s = converged(&spec);
        s.team_settings.insert("triageEnabled".into(), json!(true));
        s.team_settings.insert("autoCloseStateId".into(), json!("id-Done"));
        let shown: Vec<String> = plan(&spec, &s).iter().map(ToString::to_string).collect();
        assert_eq!(shown, ["~ team settings (autoCloseStateId, triageEnabled)"]);
        let input = team_settings(&spec, &s).expect("resolves");
        assert_eq!(input["autoCloseStateId"], json!("id-Canceled"), "sent as the state's id");
        s.states.retain(|st| st.name != "Canceled");
        assert!(team_settings(&spec, &s).is_none(), "an unknown state can't be sent");
    }

    #[test]
    fn rejects_bad_team_settings() {
        let with = |settings: &str| {
            Spec::parse(&format!(
                "[team]\nkey = \"S\"\nname = \"n\"\ndescription = \"\"\ntimezone = \"UTC\"\n[team.settings]\n{settings}"
            ))
        };
        assert!(with("cyclesEnabled = true\ncycleStartDay = 1.0").is_ok());
        assert!(with("name = \"x\"").expect_err("owned").contains("its own field"));
        assert!(with("cycles_enabled = true").expect_err("snake").contains("camelCase"));
        assert!(with("securitySettings = { a = 1 }").expect_err("object").contains("plain value"));
    }

    #[test]
    fn rejects_bad_member_entries() {
        let with = |extra: &str| {
            Spec::parse(&format!(
                "[team]\nkey = \"S\"\nname = \"n\"\ndescription = \"\"\ntimezone = \"UTC\"\n[members]\n{extra}"
            ))
        };
        assert!(with("\"kiki@example.org\" = \"member\"").is_ok());
        assert!(with("\"kiki\" = \"member\"").expect_err("no @").contains("not an email"));
        assert!(with("\"kiki@example.org\" = \"owner\"").is_err(), "ownership isn't handed out from a file");
        let twice = with("\"kiki@example.org\" = \"member\"\n\"Kiki@example.org\" = \"guest\"");
        assert!(twice.expect_err("dup").contains("listed twice"));
    }
}

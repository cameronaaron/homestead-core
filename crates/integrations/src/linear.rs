//! Linear: mirrors opted-in tasks (created, then completed), turns
//! low-stock alerts into reorder issues, and pulls the team's issues back in
//! as tasks ([`Puller`]; applied by `homestead_store::linear`).
//!
//! Idempotency: every issue Homestead creates gets an id Homestead chose,
//! derived from what it mirrors ([`issue_id_for`]): the task's id, or the
//! item and event of a low-stock crossing. Linear accepts a client-chosen
//! UUID on `issueCreate`, so a delivery first looks that id up and creates
//! only if Linear has never seen it. A crash between Linear accepting the
//! issue and Homestead recording it therefore can't produce a duplicate:
//! the retry finds the issue and writes it back. Two creates racing on the
//! same id collide on Linear's primary key instead of duplicating.
//!
//! Tasks mirrored before this scheme have random ids, recorded in
//! `tasks.linear_issue_id`; that record is still honoured first.

pub mod provision;

use homestead_bus::{BoxFuture, DeliveryError, Sink};
use homestead_domain::{Event, Task, TaskPriority};
use homestead_store::Store;
use homestead_store::linear::PulledIssue;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::transport;

pub const DEFAULT_ENDPOINT: &str = "https://api.linear.app/graphql";

const ISSUE_CREATE: &str = "mutation($input: IssueCreateInput!) {
  issueCreate(input: $input) { success issue { id identifier url state { type } } }
}";

const ISSUE_LOOKUP: &str = "query($id: String!) { issue(id: $id) { id identifier url state { type } } }";

const ISSUE_COMPLETE: &str = "mutation($id: String!, $stateId: String!) {
  issueUpdate(id: $id, input: { stateId: $stateId }) { success issue { id identifier url state { type } } }
}";

/// What Homestead needs to know about an issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub id: String,
    pub identifier: String,
    /// Linear's workflow state type: `backlog`, `unstarted`, `started`,
    /// `completed`, `canceled`, `duplicate`, `triage`.
    pub state: String,
}

impl Issue {
    /// Closed one way or another; a person's "Canceled" is never overridden.
    pub fn is_closed(&self) -> bool {
        matches!(self.state.as_str(), "completed" | "canceled" | "duplicate")
    }
}

/// The Linear issue id Homestead uses for the thing named by `key`.
///
/// SHA-256 of a namespaced key, shaped as a UUID v4 (the format Linear
/// requires): stable across retries, restarts and restores, and never equal
/// for two different keys in practice.
pub fn issue_id_for(key: &str) -> Uuid {
    let digest = Sha256::digest(format!("sunbrewed-homestead/linear-issue/{key}").as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_random_bytes(bytes).into_uuid()
}

pub fn task_issue_key(task: Uuid) -> String {
    format!("task/{task}")
}

pub fn low_stock_issue_key(item: Uuid, event_id: i64) -> String {
    format!("low-stock/{item}/{event_id}")
}

pub struct LinearSink {
    client: reqwest::Client,
    endpoint: String,
    api_key: String,
    team_id: String,
    /// The team's "completed" workflow state (Done), for finished tasks.
    done_state_id: Option<String>,
    store: Store,
}

impl LinearSink {
    pub fn new(client: reqwest::Client, endpoint: String, api_key: String, team_id: String, store: Store) -> Self {
        Self { client, endpoint, api_key, team_id, done_state_id: None, store }
    }

    /// Enables completing mirrored issues when their task is done.
    pub fn with_done_state(mut self, state_id: String) -> Self {
        self.done_state_id = Some(state_id);
        self
    }

    async fn gql(&self, query: &str, variables: Value) -> Result<Value, DeliveryError> {
        let resp = self
            .client
            .post(&self.endpoint)
            // Personal API keys go in bare; OAuth tokens would need "Bearer ".
            .header("Authorization", &self.api_key)
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await
            .map_err(transport)?;
        let status = resp.status();
        let text = resp.text().await.map_err(transport)?;
        interpret(status, &text)
    }

    /// The issue with this id, or `None` if Linear has never had it.
    pub async fn lookup(&self, id: &str) -> Result<Option<Issue>, DeliveryError> {
        match self.gql(ISSUE_LOOKUP, json!({ "id": id })).await {
            Ok(data) => issue_from(&data["issue"]).map(Some),
            Err(DeliveryError::Permanent(msg)) if msg.contains("Entity not found") => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Finds the issue with id `id`, creating it only if Linear doesn't
    /// have it. `Ok` means the issue exists now, whoever made it.
    async fn ensure_issue(
        &self,
        id: Uuid,
        title: &str,
        description: &str,
        priority: u8,
    ) -> Result<Issue, DeliveryError> {
        let id = id.to_string();
        if let Some(found) = self.lookup(&id).await? {
            return Ok(found);
        }
        let input = json!({
            "id": id, "teamId": self.team_id, "title": title, "description": description, "priority": priority
        });
        match self.gql(ISSUE_CREATE, json!({ "input": input })).await {
            Ok(data) => created(&data),
            // Rejected, perhaps because the id already exists (a concurrent
            // or half-recorded create): if it's there, that's success.
            Err(DeliveryError::Permanent(msg)) => match self.lookup(&id).await? {
                Some(found) => Ok(found),
                None => Err(DeliveryError::Permanent(msg)),
            },
            Err(e) => Err(e),
        }
    }

    async fn mirror_task(&self, event_id: i64, task: &Task) -> Result<(), DeliveryError> {
        let current = self.current(task.id).await?;
        if current.linear_issue_id.is_some() {
            return Ok(()); // already mirrored and recorded
        }
        let mut description = task.details.clone();
        if let Some(due) = task.due_on {
            description.push_str(&format!("\n\nDue: {due}"));
        }
        description.push_str(&format!(
            "\n\n_Mirrored from Sun Brewed Homestead (task {}, event {event_id}). \
             Homestead keeps this issue's id tied to the task: don't recreate it by hand._",
            task.id
        ));
        let issue = self
            .ensure_issue(
                issue_id_for(&task_issue_key(task.id)),
                &task.title,
                description.trim(),
                linear_priority(task.priority),
            )
            .await?;
        self.store
            .set_linear_issue_id(task.id, &issue.id)
            .await
            .map_err(|e| DeliveryError::Retryable(format!("store write-back: {e}")))?;
        Ok(())
    }

    /// Moves a finished task's issue to Done, unless a person has already
    /// closed it (Done, Canceled or Duplicate stay as they are).
    async fn complete_task(&self, task: &Task) -> Result<(), DeliveryError> {
        let Some(done) = &self.done_state_id else {
            return Err(DeliveryError::Permanent("the Linear team has no completed workflow state".into()));
        };
        let current = self.current(task.id).await?;
        let id = current.linear_issue_id.unwrap_or_else(|| issue_id_for(&task_issue_key(task.id)).to_string());
        let Some(issue) = self.lookup(&id).await? else {
            // Its task.created delivery hasn't landed yet (or is dead):
            // retry, and let the dead-letter view show it if it never does.
            return Err(DeliveryError::Retryable(format!("task {} has no Linear issue yet", task.id)));
        };
        if issue.is_closed() {
            return Ok(());
        }
        let data = self.gql(ISSUE_COMPLETE, json!({ "id": issue.id, "stateId": done })).await?;
        match data["issueUpdate"]["success"].as_bool() {
            Some(true) => Ok(()),
            _ => Err(DeliveryError::Permanent(format!("issueUpdate did not succeed: {data}"))),
        }
    }

    async fn reorder(&self, event_id: i64, item: &homestead_domain::InventoryItem) -> Result<(), DeliveryError> {
        let recorded =
            self.store.low_stock_issue(event_id).await.map_err(|e| DeliveryError::Retryable(format!("store: {e}")))?;
        if recorded.is_some() {
            return Ok(());
        }
        let title = format!("Reorder {}", item.name);
        let description = format!(
            "{} is at {} {} (reorder level {}).\n\n_Raised by Sun Brewed Homestead inventory \
             (item {}, event {event_id})._",
            item.name,
            item.on_hand.normalize(),
            item.unit,
            item.reorder_level.normalize(),
            item.id,
        );
        let issue =
            self.ensure_issue(issue_id_for(&low_stock_issue_key(item.id, event_id)), &title, &description, 2).await?;
        self.store
            .set_low_stock_issue(event_id, item.id, &issue.id)
            .await
            .map_err(|e| DeliveryError::Retryable(format!("store write-back: {e}")))?;
        Ok(())
    }

    async fn current(&self, task: Uuid) -> Result<Task, DeliveryError> {
        self.store.get_task(task).await.map_err(|e| DeliveryError::Retryable(format!("store: {e}")))
    }
}

const ISSUES_CHANGED: &str = "query($team: String!, $since: DateTimeOrDuration!, $after: String) {
  issues(first: 50, after: $after, orderBy: updatedAt, includeArchived: true,
         filter: { team: { key: { eq: $team } }, updatedAt: { gt: $since } }) {
    nodes { id identifier url title description priority dueDate updatedAt archivedAt
            state { type name } assignee { name displayName } project { name } }
    pageInfo { hasNextPage endCursor }
  }
}";

/// Reads a team's issues changed since a point in time.
pub struct Puller {
    client: reqwest::Client,
    endpoint: String,
    api_key: String,
    team_key: String,
}

impl Puller {
    pub fn new(client: reqwest::Client, endpoint: String, api_key: String, team_key: String) -> Self {
        Self { client, endpoint, api_key, team_key }
    }

    /// Every issue of the team updated after `since` (all of them when
    /// `None`), archived ones included so deletions are seen.
    pub async fn changed_since(&self, since: Option<OffsetDateTime>) -> Result<Vec<PulledIssue>, DeliveryError> {
        let since = since
            .unwrap_or(OffsetDateTime::UNIX_EPOCH)
            .format(&Rfc3339)
            .map_err(|e| DeliveryError::Permanent(format!("formatting the cursor: {e}")))?;
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        // A bound on pages, so a paging bug can't loop forever: 100 pages
        // is 5000 changed issues, far past this team's size.
        for _ in 0..100 {
            let resp = self
                .client
                .post(&self.endpoint)
                .header("Authorization", &self.api_key)
                .json(&json!({ "query": ISSUES_CHANGED,
                               "variables": { "team": self.team_key, "since": since, "after": after } }))
                .send()
                .await
                .map_err(transport)?;
            let status = resp.status();
            let text = resp.text().await.map_err(transport)?;
            let data = interpret(status, &text)?;
            let page = &data["issues"];
            for node in page["nodes"].as_array().into_iter().flatten() {
                out.push(pulled_from(node)?);
            }
            match (page["pageInfo"]["hasNextPage"].as_bool(), page["pageInfo"]["endCursor"].as_str()) {
                (Some(true), Some(cursor)) => after = Some(cursor.to_owned()),
                _ => return Ok(out),
            }
        }
        Err(DeliveryError::Permanent("Linear kept reporting another page after 100".into()))
    }
}

/// An `issues` node as a [`PulledIssue`].
pub fn pulled_from(v: &Value) -> Result<PulledIssue, DeliveryError> {
    let bad = |what: &str| DeliveryError::Permanent(format!("unexpected issue shape ({what}): {v}"));
    let s = |k: &str| v[k].as_str().map(str::to_owned);
    let (Some(id), Some(identifier)) = (s("id"), s("identifier")) else { return Err(bad("id")) };
    let updated_at = v["updatedAt"]
        .as_str()
        .and_then(|t| OffsetDateTime::parse(t, &Rfc3339).ok())
        .ok_or_else(|| bad("updatedAt"))?;
    let due_on = match v["dueDate"].as_str() {
        None => None,
        Some(d) => Some(
            time::Date::parse(d, time::macros::format_description!("[year]-[month]-[day]"))
                .map_err(|_| bad("dueDate"))?,
        ),
    };
    let assignee = ["displayName", "name"]
        .into_iter()
        .find_map(|k| v["assignee"][k].as_str().filter(|a| !a.is_empty()))
        .map(str::to_owned);
    Ok(PulledIssue {
        id,
        identifier,
        url: s("url").unwrap_or_default(),
        title: s("title").unwrap_or_default(),
        description: s("description").unwrap_or_default(),
        priority: task_priority(v["priority"].as_u64().unwrap_or(0)),
        due_on,
        state_type: v["state"]["type"].as_str().ok_or_else(|| bad("state"))?.to_owned(),
        state_name: v["state"]["name"].as_str().unwrap_or_default().to_owned(),
        assignee,
        project: v["project"]["name"].as_str().unwrap_or_default().to_owned(),
        archived: !v["archivedAt"].is_null(),
        updated_at,
        homestead_task: None,
    })
}

/// Linear's priority as a task's; `None` for "No priority" (0).
pub fn task_priority(p: u64) -> Option<TaskPriority> {
    match p {
        1 => Some(TaskPriority::Urgent),
        2 => Some(TaskPriority::High),
        3 => Some(TaskPriority::Normal),
        4 => Some(TaskPriority::Low),
        _ => None,
    }
}

/// Linear's numeric priority: 1 urgent, 2 high, 3 medium, 4 low (0 = none).
pub fn linear_priority(p: TaskPriority) -> u8 {
    match p {
        TaskPriority::Urgent => 1,
        TaskPriority::High => 2,
        TaskPriority::Normal => 3,
        TaskPriority::Low => 4,
    }
}

fn issue_from(v: &Value) -> Result<Issue, DeliveryError> {
    let s = |k: &str| v[k].as_str().map(str::to_owned);
    match (s("id"), s("identifier"), v["state"]["type"].as_str()) {
        (Some(id), Some(identifier), Some(state)) => Ok(Issue { id, identifier, state: state.to_owned() }),
        _ => Err(DeliveryError::Permanent(format!("unexpected issue shape: {v}"))),
    }
}

/// The issue from an `issueCreate` response's `data`.
fn created(data: &Value) -> Result<Issue, DeliveryError> {
    let c = &data["issueCreate"];
    if c["success"] != true {
        return Err(DeliveryError::Permanent(format!("issueCreate did not succeed: {c}")));
    }
    issue_from(&c["issue"])
}

/// Classifies a GraphQL response and returns its `data`. Linear reports rate
/// limiting as HTTP 400 with `extensions.code = "RATELIMITED"`, so the body
/// is inspected before the status: a status-only rule would dead-letter a
/// transient failure.
pub(crate) fn interpret(status: reqwest::StatusCode, text: &str) -> Result<Value, DeliveryError> {
    let snippet: String = text.chars().take(300).collect();
    let parsed: Option<Value> = serde_json::from_str(text).ok();
    if let Some(errors) = parsed.as_ref().and_then(|v| v["errors"].as_array()) {
        let rate_limited = errors.iter().any(|e| e["extensions"]["code"] == "RATELIMITED");
        let msg = format!("HTTP {status}: {snippet}");
        return Err(if rate_limited || status.is_server_error() {
            DeliveryError::Retryable(msg)
        } else {
            DeliveryError::Permanent(msg)
        });
    }
    if status.is_server_error() || status.as_u16() == 429 {
        return Err(DeliveryError::Retryable(format!("HTTP {status}: {snippet}")));
    }
    if !status.is_success() {
        return Err(DeliveryError::Permanent(format!("HTTP {status}: {snippet}")));
    }
    match parsed {
        Some(mut v) if v["data"].is_object() => Ok(v["data"].take()),
        _ => Err(DeliveryError::Permanent(format!("unexpected response: {snippet}"))),
    }
}

impl Sink for LinearSink {
    fn name(&self) -> &'static str {
        "linear"
    }

    fn accepts(&self, event: &Event) -> bool {
        match event {
            Event::TaskCreated { task } | Event::TaskCompleted { task } => task.mirror_to_linear,
            Event::LowStock { .. } => true,
            _ => false,
        }
    }

    fn deliver<'a>(&'a self, event_id: i64, event: &'a Event) -> BoxFuture<'a, Result<(), DeliveryError>> {
        Box::pin(async move {
            match event {
                Event::TaskCreated { task } => self.mirror_task(event_id, task).await,
                Event::TaskCompleted { task } => self.complete_task(task).await,
                Event::LowStock { item, .. } => self.reorder(event_id, item).await,
                other => Err(DeliveryError::Permanent(format!("linear does not handle {}", other.kind()))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn rate_limit_on_http_400_is_retryable() {
        let body = r#"{"errors":[{"message":"slow down","extensions":{"code":"RATELIMITED"}}]}"#;
        assert!(matches!(interpret(StatusCode::BAD_REQUEST, body), Err(DeliveryError::Retryable(_))));
    }

    #[test]
    fn auth_and_validation_errors_are_permanent() {
        let body = r#"{"errors":[{"message":"Authentication required","extensions":{"code":"AUTHENTICATION_ERROR"}}]}"#;
        assert!(matches!(interpret(StatusCode::BAD_REQUEST, body), Err(DeliveryError::Permanent(_))));
        assert!(matches!(interpret(StatusCode::UNAUTHORIZED, "nope"), Err(DeliveryError::Permanent(_))));
    }

    #[test]
    fn server_errors_are_retryable() {
        assert!(matches!(interpret(StatusCode::BAD_GATEWAY, "<html>"), Err(DeliveryError::Retryable(_))));
    }

    #[test]
    fn a_created_issue_is_read_and_false_success_is_not() {
        let ok = r#"{"data":{"issueCreate":{"success":true,"issue":{"id":"abc","identifier":"SUN-1","url":"u","state":{"type":"backlog"}}}}}"#;
        let data = interpret(StatusCode::OK, ok).expect("data");
        assert_eq!(created(&data), Ok(Issue { id: "abc".into(), identifier: "SUN-1".into(), state: "backlog".into() }));
        let no = r#"{"data":{"issueCreate":{"success":false,"issue":null}}}"#;
        assert!(matches!(created(&interpret(StatusCode::OK, no).expect("data")), Err(DeliveryError::Permanent(_))));
    }

    #[test]
    fn not_found_is_linears_input_error() {
        // Verbatim from the live API (2026-09-28) for an id it never had;
        // `lookup` turns exactly this into `None`.
        let body = r#"{"errors":[{"message":"Entity not found: Issue","path":["issue"],"extensions":{"type":"invalid input","code":"INPUT_ERROR","statusCode":400,"userError":true}}],"data":null}"#;
        match interpret(StatusCode::OK, body) {
            Err(DeliveryError::Permanent(msg)) => assert!(msg.contains("Entity not found"), "{msg}"),
            other => panic!("expected a permanent not-found, got {other:?}"),
        }
    }

    #[test]
    fn issue_ids_are_stable_v4_shaped_and_distinct() {
        let t = Uuid::from_u128(7);
        let a = issue_id_for(&task_issue_key(t));
        assert_eq!(a, issue_id_for(&task_issue_key(t)), "a retry must derive the same id");
        assert_eq!(a.get_version_num(), 4, "Linear requires UUID v4 format");
        assert_eq!(a.get_variant(), uuid::Variant::RFC4122);
        assert_ne!(a, issue_id_for(&task_issue_key(Uuid::from_u128(8))));
        // A task and a low-stock crossing never share an id, and each
        // crossing (event) of the same item gets its own issue.
        assert_ne!(a, issue_id_for(&low_stock_issue_key(t, 1)));
        assert_ne!(issue_id_for(&low_stock_issue_key(t, 1)), issue_id_for(&low_stock_issue_key(t, 2)));
    }

    #[test]
    fn only_closed_states_count_as_closed() {
        let issue = |state: &str| Issue { id: "i".into(), identifier: "SUN-1".into(), state: state.into() };
        for s in ["completed", "canceled", "duplicate"] {
            assert!(issue(s).is_closed(), "{s}");
        }
        for s in ["backlog", "unstarted", "started", "triage"] {
            assert!(!issue(s).is_closed(), "{s}");
        }
    }

    #[test]
    fn a_live_issue_node_is_read() {
        // Shape captured from the live API on 2026-09-29 (SUN-25).
        let node = json!({
            "id": "9dbf7800-90ca-4879-b023-5397a9f8ee6b", "identifier": "SUN-25",
            "url": "https://linear.app/sunbrewedhomestead/issue/SUN-25/start-fall-seeds",
            "title": "Start fall seeds", "description": null, "priority": 2, "dueDate": "2026-11-15",
            "updatedAt": "2026-09-28T20:58:22.117Z", "archivedAt": null,
            "state": { "type": "backlog", "name": "Backlog" },
            "assignee": { "name": "photomancer", "displayName": "" },
            "project": { "name": "Community workshop series" }
        });
        let i = pulled_from(&node).expect("issue");
        assert_eq!(
            (i.identifier.as_str(), i.title.as_str(), i.description.as_str()),
            ("SUN-25", "Start fall seeds", "")
        );
        assert_eq!((i.priority, i.due_on), (Some(TaskPriority::High), Some(time::macros::date!(2026 - 11 - 15))));
        assert_eq!((i.state_type.as_str(), i.assignee.as_deref(), i.archived), ("backlog", Some("photomancer"), false));
        assert_eq!(i.updated_at, time::macros::datetime!(2026-09-28 20:58:22.117 UTC));
        assert_eq!(i.project, "Community workshop series");

        let mut gone = node.clone();
        gone["archivedAt"] = json!("2026-09-29T01:00:00.000Z");
        gone["assignee"] = Value::Null;
        // No project reads as `"project": null` (seen live on SUN-37).
        gone["project"] = Value::Null;
        let g = pulled_from(&gone).expect("archived");
        assert!(g.archived && g.assignee.is_none() && g.project.is_empty());
        let mut broken = node;
        broken["dueDate"] = json!("soon");
        assert!(matches!(pulled_from(&broken), Err(DeliveryError::Permanent(_))));
    }

    #[test]
    fn priorities_round_trip_and_no_priority_is_none() {
        for p in TaskPriority::ALL {
            assert_eq!(task_priority(u64::from(linear_priority(*p))), Some(*p));
        }
        assert_eq!(task_priority(0), None);
    }

    #[test]
    fn priorities_map_to_linear_scale() {
        let got: Vec<u8> = TaskPriority::ALL.iter().map(|p| linear_priority(*p)).collect();
        assert_eq!(got, [4, 3, 2, 1]); // Low, Normal, High, Urgent
    }
}

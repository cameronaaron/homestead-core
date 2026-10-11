//! Linear issues pulled in as tasks.
//!
//! The pull only ever moves a task forward: an issue filed in Linear and
//! still open becomes an open task; a linked issue that closes closes its
//! task. Nothing here reopens a task. A task finished in Homestead may not
//! have reached Linear yet (its `task.completed` delivery is queued), so an
//! issue that still looks open is not evidence the task should be.
//!
//! Imported tasks are not announced as `task.created`: they already exist,
//! in Linear, and a first pull would otherwise post the whole backlog to
//! Discord. Closing one does emit `task.completed`, like any other finish.

use std::collections::HashMap;

use homestead_domain::TaskPriority;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::{Result, Store, close_task};

/// One issue as Linear reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct PulledIssue {
    /// Linear's issue id (a UUID string).
    pub id: String,
    /// `SUN-25`.
    pub identifier: String,
    pub url: String,
    pub title: String,
    pub description: String,
    /// `None` for Linear's "No priority".
    pub priority: Option<TaskPriority>,
    pub due_on: Option<Date>,
    /// Linear's workflow state type: `triage`, `backlog`, `unstarted`,
    /// `started`, `completed`, `canceled` or `duplicate`.
    pub state_type: String,
    pub state_name: String,
    pub assignee: Option<String>,
    /// The Linear project's name; empty when it's in none.
    pub project: String,
    pub archived: bool,
    pub updated_at: OffsetDateTime,
    /// The Homestead task this issue mirrors, when the issue's id was
    /// derived from one whose delivery hasn't recorded it yet.
    pub homestead_task: Option<Uuid>,
}

impl PulledIssue {
    fn completed(&self) -> bool {
        self.state_type == "completed"
    }

    /// Closed without the work being done: canceled, a duplicate, or
    /// archived (Linear's delete).
    fn dropped(&self) -> bool {
        !self.completed() && (self.archived || matches!(self.state_type.as_str(), "canceled" | "duplicate"))
    }

    fn open(&self) -> bool {
        !self.completed() && !self.dropped()
    }

    fn title(&self) -> &str {
        let t = self.title.trim();
        if t.is_empty() { &self.identifier } else { t }
    }
}

/// What one pull changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PullOutcome {
    pub imported: usize,
    pub updated: usize,
    pub closed: usize,
}

/// A task's Linear issue, for linking to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueLink {
    pub identifier: String,
    pub url: String,
    pub state_name: String,
    pub assignee: String,
}

/// How an inbound pull last went.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct SyncStatus {
    pub cursor: Option<OffsetDateTime>,
    pub pulled_at: Option<OffsetDateTime>,
    /// Empty when the last attempt succeeded.
    pub error: String,
    pub error_at: Option<OffsetDateTime>,
}

impl Store {
    /// Where the Linear pull got to: pull issues updated after this.
    pub async fn linear_cursor(&self) -> Result<Option<OffsetDateTime>> {
        Ok(sqlx::query_scalar::<_, Option<OffsetDateTime>>(
            "select cursor from homestead.sync_cursors where source = 'linear'",
        )
        .fetch_optional(&self.pool)
        .await?
        .flatten())
    }

    /// Tasks marked for Linear whose issue hasn't been recorded yet: the
    /// pull matches them by their derived issue id instead of importing
    /// their issue a second time.
    pub async fn unlinked_linear_tasks(&self) -> Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar(
            "select id from homestead.tasks where mirror_to_linear and linear_issue_id is null and status = 'open'",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Applies a pull and advances the cursor to `cursor`, all or nothing.
    pub async fn apply_linear_issues(&self, issues: &[PulledIssue], cursor: OffsetDateTime) -> Result<PullOutcome> {
        let mut tx = self.begin().await?;
        let mut out = PullOutcome::default();
        for issue in issues {
            let mut linked: Option<(Uuid, String)> =
                sqlx::query_as("select id, status from homestead.tasks where linear_issue_id = $1")
                    .bind(&issue.id)
                    .fetch_optional(&mut *tx)
                    .await?;
            if linked.is_none()
                && let Some(task) = issue.homestead_task
            {
                linked = sqlx::query_as(
                    "update homestead.tasks set linear_issue_id = $2
                     where id = $1 and mirror_to_linear and linear_issue_id is null returning id, status",
                )
                .bind(task)
                .bind(&issue.id)
                .fetch_optional(&mut *tx)
                .await?;
            }
            let Some((task_id, status)) = linked else {
                if !issue.open() {
                    continue; // history from before the pull, or never ours
                }
                let task_id: Uuid = sqlx::query_scalar(
                    "insert into homestead.tasks (title, details, due_on, priority, mirror_to_linear, linear_issue_id)
                     values ($1, $2, $3, $4, true, $5) returning id",
                )
                .bind(issue.title())
                .bind(issue.description.trim())
                .bind(issue.due_on)
                .bind(issue.priority.unwrap_or(TaskPriority::Normal).as_str())
                .bind(&issue.id)
                .fetch_one(&mut *tx)
                .await?;
                upsert_link(&mut tx, issue, task_id, true).await?;
                out.imported += 1;
                continue;
            };
            let imported = upsert_link(&mut tx, issue, task_id, false).await?;
            if status != "open" {
                continue;
            }
            if imported {
                let n = sqlx::query(
                    "update homestead.tasks set title = $2, details = $3, due_on = $4, priority = $5
                     where id = $1 and (title, details, due_on, priority) is distinct from ($2, $3, $4, $5)",
                )
                .bind(task_id)
                .bind(issue.title())
                .bind(issue.description.trim())
                .bind(issue.due_on)
                .bind(issue.priority.unwrap_or(TaskPriority::Normal).as_str())
                .execute(&mut *tx)
                .await?
                .rows_affected();
                out.updated += n as usize;
            }
            if issue.completed() {
                if close_task(&mut tx, task_id, &self.timezone).await?.is_some() {
                    out.closed += 1;
                }
            } else if issue.dropped() {
                let n =
                    sqlx::query("update homestead.tasks set status = 'cancelled' where id = $1 and status = 'open'")
                        .bind(task_id)
                        .execute(&mut *tx)
                        .await?
                        .rows_affected();
                out.closed += n as usize;
            }
        }
        sqlx::query(
            "insert into homestead.sync_cursors (source, cursor, pulled_at, error, error_at)
             values ('linear', $1, now(), '', null)
             on conflict (source) do update set cursor = greatest(sync_cursors.cursor, excluded.cursor),
               pulled_at = now(), error = '', error_at = null",
        )
        .bind(cursor)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(out)
    }

    /// Records a failed pull, keeping the cursor where it was.
    pub async fn record_linear_pull_error(&self, error: &str) -> Result<()> {
        let error: String = error.chars().take(500).collect();
        let mut tx = self.begin().await?;
        sqlx::query(
            "insert into homestead.sync_cursors (source, error, error_at) values ('linear', $1, now())
             on conflict (source) do update set error = excluded.error, error_at = now()",
        )
        .bind(error)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn linear_sync_status(&self) -> Result<Option<SyncStatus>> {
        Ok(sqlx::query_as(
            "select cursor, pulled_at, error, error_at from homestead.sync_cursors where source = 'linear'",
        )
        .fetch_optional(&self.pool)
        .await?)
    }

    /// The Linear issue behind each open task that has one.
    pub async fn open_task_issue_links(&self) -> Result<HashMap<Uuid, IssueLink>> {
        let rows: Vec<(Uuid, String, String, String, String)> = sqlx::query_as(
            "select l.task_id, l.identifier, l.url, l.state_name, l.assignee
             from homestead.linear_issues l join homestead.tasks t on t.id = l.task_id where t.status = 'open'",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(task, identifier, url, state_name, assignee)| {
                (task, IssueLink { identifier, url, state_name, assignee })
            })
            .collect())
    }
}

/// Records what's known about `issue`, returning whether its task was
/// imported from Linear. `imported` applies only to a first record.
async fn upsert_link(tx: &mut crate::Tx<'_>, issue: &PulledIssue, task_id: Uuid, imported: bool) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "insert into homestead.linear_issues
           (issue_id, task_id, identifier, url, state_name, assignee, imported, updated_at, project)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         on conflict (issue_id) do update set identifier = excluded.identifier, url = excluded.url,
           state_name = excluded.state_name, assignee = excluded.assignee, updated_at = excluded.updated_at,
           project = excluded.project
         returning imported",
    )
    .bind(&issue.id)
    .bind(task_id)
    .bind(&issue.identifier)
    .bind(&issue.url)
    .bind(&issue.state_name)
    .bind(issue.assignee.as_deref().unwrap_or(""))
    .bind(imported)
    .bind(issue.updated_at)
    .bind(&issue.project)
    .fetch_one(&mut **tx)
    .await?)
}

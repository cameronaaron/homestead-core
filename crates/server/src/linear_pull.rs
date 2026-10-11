//! Pulls the Linear team's issues in as tasks (see `homestead_store::linear`
//! for what a pull changes).
//!
//! The container sleeps when idle, so a timer alone would leave a woken
//! app showing yesterday's issues. Pages that list tasks call [`refresh`],
//! which waits briefly for a pull when the last one is stale; the timer in
//! `main` keeps it current while the app stays awake. Linear's webhooks
//! would be instant but would wake (and bill) the container on every edit.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use homestead_integrations::linear::{self, Puller};
use homestead_store::Store;
use homestead_store::linear::{IssueLink, PullOutcome, SyncStatus};
use maud::{Markup, html};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::AppState;

/// How far back each pull re-reads, so an issue saved in the same instant
/// as the cursor isn't skipped. Re-applying an issue changes nothing.
const OVERLAP: time::Duration = time::Duration::minutes(1);

/// How long a page waits for a pull before rendering what it has; the
/// pull carries on in the background.
const PAGE_WAIT: Duration = Duration::from_secs(4);

pub struct LinearPull {
    puller: Puller,
    every: Duration,
    /// When the last pull succeeded. Holding the lock is holding the pull,
    /// so a page and the timer never pull at once.
    last: tokio::sync::Mutex<Option<Instant>>,
}

impl LinearPull {
    pub fn new(puller: Puller, every: Duration) -> Self {
        Self { puller, every, last: tokio::sync::Mutex::new(None) }
    }

    pub fn every(&self) -> Duration {
        self.every
    }

    /// Pulls unless the last success is fresher than the interval.
    pub async fn pull_if_stale(&self, store: &Store) {
        let mut last = self.last.lock().await;
        if last.is_some_and(|t| t.elapsed() < self.every) {
            return;
        }
        match pull(&self.puller, store).await {
            Ok(out) => {
                *last = Some(Instant::now());
                if out != PullOutcome::default() {
                    tracing::info!(
                        imported = out.imported,
                        updated = out.updated,
                        closed = out.closed,
                        "pulled Linear"
                    );
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "Linear pull failed");
                if let Err(e) = store.record_linear_pull_error(&e).await {
                    tracing::error!(error = %e, "could not record the Linear pull failure");
                }
            }
        }
    }
}

/// The store the pull writes with: its changes are attributed to `linear`.
fn store_for(state: &AppState) -> Store {
    state.job_store("linear")
}

/// Brings tasks up to date with Linear if they're stale, waiting at most
/// [`PAGE_WAIT`]. A no-op when the pull isn't configured.
pub async fn refresh(state: &AppState) {
    let Some(p) = state.linear_pull.clone() else { return };
    let store = store_for(state);
    let handle = tokio::spawn(async move { p.pull_if_stale(&store).await });
    if tokio::time::timeout(PAGE_WAIT, handle).await.is_err() {
        tracing::warn!("Linear pull is slow; showing tasks as they were");
    }
}

pub async fn pull(puller: &Puller, store: &Store) -> Result<PullOutcome, String> {
    let since = store.linear_cursor().await.map_err(|e| format!("reading the cursor: {e}"))?;
    let mut issues = puller.changed_since(since.map(|t| t - OVERLAP)).await.map_err(|e| e.to_string())?;
    let unlinked = store.unlinked_linear_tasks().await.map_err(|e| format!("reading tasks: {e}"))?;
    let derived: HashMap<String, Uuid> =
        unlinked.into_iter().map(|t| (linear::issue_id_for(&linear::task_issue_key(t)).to_string(), t)).collect();
    for i in &mut issues {
        i.homestead_task = derived.get(&i.id).copied();
    }
    let cursor = issues.iter().map(|i| i.updated_at).max().or(since).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    store.apply_linear_issues(&issues, cursor).await.map_err(|e| format!("applying: {e}"))
}

/// The Linear chip for a task: its SUN-n linking to the issue, and who
/// has it.
pub fn chip(link: Option<&IssueLink>) -> Markup {
    html! {
        @if let Some(l) = link {
            " "
            a.pill href=(l.url) target="_blank" rel="noopener" title={"Open " (l.identifier) " in Linear (" (l.state_name) ")"} { (l.identifier) }
            @if !l.assignee.is_empty() { span.muted { " · " (l.assignee) } }
        }
    }
}

/// One line saying how current the Linear tasks are.
pub fn status_line(status: Option<&SyncStatus>, now: OffsetDateTime) -> Markup {
    html! {
        @match status {
            None => p.muted { "Linear: not pulled yet." },
            Some(s) if !s.error.is_empty() => p.warn {
                "Linear pull failing" @if let Some(at) = s.error_at { " (" (crate::ui::ago(now - at)) ")" } ": " (s.error)
                @if let Some(at) = s.pulled_at { " Last good pull " (crate::ui::ago(now - at)) "." }
            },
            Some(s) => p.muted { "Tasks from Linear, pulled " (s.pulled_at.map(|at| crate::ui::ago(now - at)).unwrap_or_else(|| "never".into())) "." },
        }
    }
}

/// Pulls on start (a woken container catches up at once), then every
/// interval until `stop` changes.
pub fn spawn(state: &AppState, mut stop: tokio::sync::watch::Receiver<bool>) -> Option<tokio::task::JoinHandle<()>> {
    let p: Arc<LinearPull> = state.linear_pull.clone()?;
    let store = store_for(state);
    Some(tokio::spawn(async move {
        loop {
            p.pull_if_stale(&store).await;
            tokio::select! {
                _ = tokio::time::sleep(p.every()) => {}
                _ = stop.changed() => break,
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_pull_says_so_and_when_it_last_worked() {
        let now = time::macros::datetime!(2026-09-29 12:00 UTC);
        let failing = SyncStatus {
            cursor: None,
            pulled_at: Some(now - time::Duration::hours(3)),
            error: "HTTP 401".into(),
            error_at: Some(now - time::Duration::minutes(2)),
        };
        let html = status_line(Some(&failing), now).into_string();
        assert!(html.contains("warn") && html.contains("HTTP 401") && html.contains("3 hours ago"), "{html}");
        let ok = SyncStatus { error: String::new(), error_at: None, ..failing };
        assert!(status_line(Some(&ok), now).into_string().contains("pulled 3 hours ago"));
        assert!(status_line(None, now).into_string().contains("not pulled yet"));
    }
}

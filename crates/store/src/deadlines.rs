//! Deadline reminders: finds open tasks whose due date reached a new
//! stage and announces them once (see `homestead_domain::deadlines`).

use std::collections::BTreeMap;

use homestead_domain::Event;
use homestead_domain::deadlines::{DueItem, HORIZON_DAYS, Stage};
use time::Date;
use uuid::Uuid;

use crate::{Result, Store, outbox};

/// An open task with a due date inside the horizon, and its Linear issue
/// when it has one.
#[derive(sqlx::FromRow)]
struct Candidate {
    task_id: Uuid,
    title: String,
    due_on: Date,
    issue: Option<String>,
    url: Option<String>,
    assignee: Option<String>,
    project: Option<String>,
}

impl Store {
    /// Posts every reminder due today on the farm. Returns how many tasks
    /// were reminded about.
    pub async fn send_deadline_reminders(&self) -> Result<usize> {
        let today = self.today().await?;
        self.send_deadline_reminders_as_of(today).await
    }

    /// Records each task's current stage and emits one `task.deadlines`
    /// event per Linear project, in one transaction. A stage already
    /// recorded for that due date is skipped, so running this again, or
    /// in two places at once, posts nothing twice: the second insert of a
    /// key waits for the first to commit and then finds it taken.
    ///
    /// Routine tasks are left out. They come due every day or week by
    /// design, and their reminder is the task appearing on Today.
    pub async fn send_deadline_reminders_as_of(&self, today: Date) -> Result<usize> {
        let horizon = today + time::Duration::days(HORIZON_DAYS);
        let mut tx = self.begin().await?;
        let rows: Vec<Candidate> = sqlx::query_as(
            "select t.id as task_id, t.title, t.due_on, l.identifier as issue, l.url,
                    coalesce(a.display_name, nullif(l.assignee, '')) as assignee, l.project
                 from homestead.tasks t left join homestead.linear_issues l on l.task_id = t.id
                 left join homestead.actors a on a.id = t.assigned_to
                 where t.status = 'open' and t.template_id is null and t.due_on <= $1
                 order by t.due_on, t.title",
        )
        .bind(horizon)
        .fetch_all(&mut *tx)
        .await?;
        let mut by_project: BTreeMap<String, Vec<DueItem>> = BTreeMap::new();
        for Candidate { task_id, title, due_on, issue, url, assignee, project } in rows {
            let days_left = (due_on - today).whole_days();
            let Some(stage) = Stage::for_days_left(days_left) else { continue };
            let fresh = sqlx::query(
                "insert into homestead.deadline_reminders (task_id, due_on, stage) values ($1, $2, $3)
                 on conflict do nothing",
            )
            .bind(task_id)
            .bind(due_on)
            .bind(stage.as_str())
            .execute(&mut *tx)
            .await?
            .rows_affected()
                == 1;
            if fresh {
                by_project.entry(project.unwrap_or_default()).or_default().push(DueItem {
                    title,
                    due_on,
                    days_left,
                    issue,
                    url,
                    assignee: assignee.unwrap_or_default(),
                });
            }
        }
        let mut reminded = 0;
        for (project, items) in by_project {
            reminded += items.len();
            outbox::emit(&mut tx, &Event::TaskDeadlines { project, items }).await?;
        }
        tx.commit().await?;
        Ok(reminded)
    }
}

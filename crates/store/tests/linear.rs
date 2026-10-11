//! Linear issues pulled in as tasks, against a real Postgres.

use homestead_domain::{NewTask, TaskPriority, TaskStatus};
use homestead_store::Store;
use homestead_store::linear::{PullOutcome, PulledIssue};
use sqlx::PgPool;
use time::OffsetDateTime;
use time::macros::{date, datetime};

fn issue(n: u32, state: &str) -> PulledIssue {
    PulledIssue {
        id: format!("00000000-0000-4000-8000-{n:012}"),
        identifier: format!("SUN-{n}"),
        url: format!("https://linear.app/x/issue/SUN-{n}"),
        title: format!("Issue {n}"),
        description: "From Linear".into(),
        priority: Some(TaskPriority::High),
        due_on: Some(date!(2026 - 11 - 15)),
        state_type: state.into(),
        state_name: state.into(),
        assignee: Some("photomancer".into()),
        project: String::new(),
        archived: false,
        updated_at: datetime!(2026-09-28 20:00 UTC),
        homestead_task: None,
    }
}

const AT: OffsetDateTime = datetime!(2026-09-28 21:00 UTC);

async fn event_kinds(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("select kind from homestead.events order by id").fetch_all(pool).await.expect("events")
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn open_issues_become_tasks_quietly_and_closed_history_is_skipped(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let pulled = [issue(1, "backlog"), issue(2, "started"), issue(3, "completed"), issue(4, "canceled")];
    let out = store.apply_linear_issues(&pulled, AT).await.expect("apply");
    assert_eq!(out, PullOutcome { imported: 2, updated: 0, closed: 0 });

    let open = store.list_open_tasks().await.expect("tasks");
    let titles: Vec<&str> = open.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["Issue 1", "Issue 2"]);
    let t = &open[0];
    assert_eq!(
        (t.priority, t.due_on, t.details.as_str()),
        (TaskPriority::High, Some(date!(2026 - 11 - 15)), "From Linear")
    );
    assert!(t.mirror_to_linear, "finishing it in Homestead must close the issue");
    assert_eq!(t.linear_issue_id.as_deref(), Some(pulled[0].id.as_str()));
    assert!(event_kinds(&pool).await.is_empty(), "an import is not news: nothing goes to Discord");

    let links = store.open_task_issue_links().await.expect("links");
    assert_eq!(links[&t.id].identifier, "SUN-1");
    assert_eq!(links[&t.id].assignee, "photomancer");
    assert_eq!(store.linear_cursor().await.expect("cursor"), Some(AT));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn pulling_twice_is_idempotent_and_linear_edits_flow_in(pool: PgPool) {
    let store = Store::from_pool(pool);
    let mut i = issue(5, "backlog");
    store.apply_linear_issues(&[i.clone()], AT).await.expect("first");
    assert_eq!(store.apply_linear_issues(&[i.clone()], AT).await.expect("again"), PullOutcome::default());

    i.title = "Start fall seeds (brassicas)".into();
    i.priority = None;
    i.due_on = None;
    let out = store.apply_linear_issues(&[i], AT).await.expect("edit");
    assert_eq!(out.updated, 1);
    let t = &store.list_open_tasks().await.expect("tasks")[0];
    assert_eq!((t.title.as_str(), t.priority, t.due_on), ("Start fall seeds (brassicas)", TaskPriority::Normal, None));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn closing_in_linear_closes_the_task_and_nothing_reopens_it(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let (mut done, mut dropped) = (issue(6, "backlog"), issue(7, "backlog"));
    store.apply_linear_issues(&[done.clone(), dropped.clone()], AT).await.expect("import");

    done.state_type = "completed".into();
    dropped.archived = true;
    let out = store.apply_linear_issues(&[done.clone(), dropped.clone()], AT).await.expect("close");
    assert_eq!(out.closed, 2);
    assert!(store.list_open_tasks().await.expect("tasks").is_empty());
    let status = |id: &str| {
        let pool = pool.clone();
        let id = id.to_owned();
        async move {
            sqlx::query_scalar::<_, String>("select status from homestead.tasks where linear_issue_id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("status")
        }
    };
    assert_eq!(status(&done.id).await, "done");
    assert_eq!(status(&dropped.id).await, "cancelled", "a canceled issue is not work done");
    assert_eq!(event_kinds(&pool).await, ["task.completed"], "only real completions are announced");

    // Linear showing an issue open again doesn't undo a finish: Homestead
    // can't tell a reopen from a completion that hasn't reached Linear yet.
    done.state_type = "started".into();
    store.apply_linear_issues(&[done], AT).await.expect("reopen");
    assert!(store.list_open_tasks().await.expect("tasks").is_empty());
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_mirrored_task_is_linked_not_imported_and_keeps_its_own_details(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let task = store
        .create_task(NewTask {
            title: "Fix the fence".into(),
            details: "North side".into(),
            location_id: None,
            due_on: None,
            priority: TaskPriority::Normal,
            mirror_to_linear: true,
            assigned_to: None,
        })
        .await
        .expect("task");
    assert_eq!(store.unlinked_linear_tasks().await.expect("unlinked"), [task.id]);

    // The pull sees the issue before the mirror delivery recorded its id.
    let mut i = issue(8, "backlog");
    i.title = "Fix the fence".into();
    i.description = "North side\n\n_Mirrored from Sun Brewed Homestead_".into();
    i.homestead_task = Some(task.id);
    let out = store.apply_linear_issues(&[i.clone()], AT).await.expect("link");
    assert_eq!(out, PullOutcome::default(), "linked, not imported");
    let open = store.list_open_tasks().await.expect("tasks");
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].linear_issue_id.as_deref(), Some(i.id.as_str()));
    assert_eq!(open[0].details, "North side", "Homestead owns what it mirrored");
    assert!(!store.set_linear_issue_id(task.id, &i.id).await.expect("late write-back"), "first write wins");

    i.state_type = "completed".into();
    store.apply_linear_issues(&[i], AT).await.expect("close");
    assert_eq!(store.get_task(task.id).await.expect("task").status, TaskStatus::Done);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_failed_pull_is_recorded_without_moving_the_cursor(pool: PgPool) {
    let store = Store::from_pool(pool);
    assert!(store.linear_sync_status().await.expect("status").is_none());
    store.apply_linear_issues(&[], AT).await.expect("empty pull");
    store.record_linear_pull_error("HTTP 401: bad key").await.expect("error");
    let s = store.linear_sync_status().await.expect("status").expect("row");
    assert_eq!((s.cursor, s.error.as_str()), (Some(AT), "HTTP 401: bad key"));
    // An older cursor never rewinds a newer one.
    store.apply_linear_issues(&[], datetime!(2026-01-01 0:00 UTC)).await.expect("older");
    let s = store.linear_sync_status().await.expect("status").expect("row");
    assert_eq!((s.cursor, s.error.as_str()), (Some(AT), ""));
}

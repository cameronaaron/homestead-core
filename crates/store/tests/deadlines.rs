//! Deadline reminders, against a real Postgres.

use homestead_domain::Event;
use homestead_domain::deadlines::DueItem;
use homestead_domain::recurring::{NewTemplate, Schedule};
use homestead_domain::{NewTask, TaskPriority};
use homestead_store::Store;
use homestead_store::linear::PulledIssue;
use sqlx::PgPool;
use time::Date;
use time::macros::{date, datetime};

const TODAY: Date = date!(2026 - 10 - 01);

fn due(title: &str, on: Date) -> NewTask {
    NewTask { due_on: Some(on), ..NewTask::titled(title) }
}

/// Every `task.deadlines` event so far, as (project, items).
async fn reminders(pool: &PgPool) -> Vec<(String, Vec<DueItem>)> {
    let rows: Vec<(String, serde_json::Value)> =
        sqlx::query_as("select kind, payload from homestead.events where kind = 'task.deadlines' order by id")
            .fetch_all(pool)
            .await
            .expect("events");
    rows.into_iter()
        .map(|(kind, payload)| match Event::from_parts(&kind, payload).expect("decodes") {
            Event::TaskDeadlines { project, items } => (project, items),
            other => panic!("unexpected {other:?}"),
        })
        .collect()
}

fn titles(items: &[DueItem]) -> Vec<(&str, i64)> {
    items.iter().map(|i| (i.title.as_str(), i.days_left)).collect()
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn each_stage_is_posted_once_and_only_for_open_one_off_tasks(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    store.create_task(due("Order seed garlic", date!(2026 - 10 - 08))).await.expect("week");
    store.create_task(due("File the 990-N", date!(2026 - 10 - 15))).await.expect("two weeks");
    store.create_task(due("Plan spring beds", date!(2026 - 10 - 16))).await.expect("too far");
    store.create_task(NewTask::titled("Someday: paint the shed")).await.expect("no date");
    let done = store.create_task(due("Mend the fence", date!(2026 - 09 - 30))).await.expect("done");
    store.complete_task(done.id).await.expect("complete");
    store
        .create_template(NewTemplate {
            title: "Collect eggs".into(),
            details: String::new(),
            location_id: None,
            priority: TaskPriority::Normal,
            schedule: Schedule::Days { days: 1 },
            starts_on: TODAY,
            lead_days: 0,
            catch_up: false,
        })
        .await
        .expect("routine");
    store.generate_recurring_tasks_as_of(TODAY).await.expect("routine task due today");

    assert_eq!(store.send_deadline_reminders_as_of(TODAY).await.expect("run"), 2);
    let got = reminders(&pool).await;
    assert_eq!(got.len(), 1, "one post for tasks outside any Linear project");
    assert_eq!(got[0].0, "");
    assert_eq!(titles(&got[0].1), [("Order seed garlic", 7), ("File the 990-N", 14)], "soonest first");

    // Same day again: nothing new. The next day, garlic and the 990-N are
    // still in the same stages, and only the spring beds come inside two
    // weeks.
    assert_eq!(store.send_deadline_reminders_as_of(TODAY).await.expect("rerun"), 0);
    assert_eq!(store.send_deadline_reminders_as_of(date!(2026 - 10 - 02)).await.expect("next day"), 1);
    assert_eq!(titles(&reminders(&pool).await[1].1), [("Plan spring beds", 14)]);

    // Garlic is due tomorrow; the others stay in stages already posted.
    assert_eq!(store.send_deadline_reminders_as_of(date!(2026 - 10 - 07)).await.expect("day before"), 1);
    // The day itself, and the 990-N enters its last week.
    assert_eq!(store.send_deadline_reminders_as_of(date!(2026 - 10 - 08)).await.expect("the day"), 2);
    assert_eq!(titles(&reminders(&pool).await[3].1), [("Order seed garlic", 0), ("File the 990-N", 7)]);

    // Overdue is said once, not every day after.
    store.send_deadline_reminders_as_of(date!(2026 - 10 - 09)).await.expect("overdue");
    let before = reminders(&pool).await.len();
    store.send_deadline_reminders_as_of(date!(2026 - 10 - 12)).await.expect("still overdue");
    let after = reminders(&pool).await;
    assert!(after[before - 1].1.iter().any(|i| i.title == "Order seed garlic" && i.days_left == -1));
    assert!(after[before..].iter().all(|(_, items)| items.iter().all(|i| i.title != "Order seed garlic")));
}

fn issue(project: &str, due_on: Date) -> PulledIssue {
    PulledIssue {
        id: "00000000-0000-4000-8000-000000000046".into(),
        identifier: "SUN-46".into(),
        url: "https://linear.app/x/issue/SUN-46".into(),
        title: "Submit the grant application".into(),
        description: String::new(),
        priority: Some(TaskPriority::Urgent),
        due_on: Some(due_on),
        state_type: "unstarted".into(),
        state_name: "Todo".into(),
        assignee: Some("Ava".into()),
        project: project.into(),
        archived: false,
        updated_at: datetime!(2026-09-29 20:00 UTC),
        homestead_task: None,
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn linear_tasks_carry_their_project_and_a_moved_date_is_reminded_again(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    store
        .apply_linear_issues(&[issue("Grant writing", date!(2026 - 10 - 08))], datetime!(2026-09-29 21:00 UTC))
        .await
        .expect("pull");
    store.create_task(due("Order seed garlic", date!(2026 - 10 - 08))).await.expect("local");

    assert_eq!(store.send_deadline_reminders_as_of(TODAY).await.expect("run"), 2);
    let got = reminders(&pool).await;
    let projects: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(projects, ["", "Grant writing"], "one post per project");
    let grant = &got[1].1[0];
    assert_eq!(
        (grant.issue.as_deref(), grant.url.as_deref(), grant.assignee.as_str()),
        (Some("SUN-46"), Some("https://linear.app/x/issue/SUN-46"), "Ava")
    );

    // The funder extends the deadline by two days: same stage, new date,
    // so it's said again; the unchanged task is not.
    let moved =
        PulledIssue { updated_at: datetime!(2026-09-30 20:00 UTC), ..issue("Grant writing", date!(2026 - 10 - 10)) };
    store.apply_linear_issues(&[moved], datetime!(2026-09-30 21:00 UTC)).await.expect("pull again");
    assert_eq!(store.send_deadline_reminders_as_of(TODAY).await.expect("rerun"), 1);
    let got = reminders(&pool).await;
    assert_eq!((got[2].0.as_str(), got[2].1[0].due_on), ("Grant writing", date!(2026 - 10 - 10)));
}

/// Two runs at once must not both post. Racing two calls with
/// `tokio::join!` doesn't reliably overlap them (see AGENTS.md), so this
/// holds one run's insert open by hand, starts the real run, checks it is
/// blocked on that key, then commits and checks it posted nothing for it.
#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_concurrent_run_waits_and_then_posts_nothing_twice(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let task = store.create_task(due("Order seed garlic", date!(2026 - 10 - 08))).await.expect("task");

    let mut first = pool.begin().await.expect("tx");
    sqlx::query("insert into homestead.deadline_reminders (task_id, due_on, stage) values ($1, $2, 'week')")
        .bind(task.id)
        .bind(date!(2026 - 10 - 08))
        .execute(&mut *first)
        .await
        .expect("held insert");

    let second = tokio::spawn({
        let store = store.clone();
        async move { store.send_deadline_reminders_as_of(TODAY).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!second.is_finished(), "the second run must wait on the uncommitted reminder");

    first.commit().await.expect("commit");
    assert_eq!(second.await.expect("join").expect("run"), 0);
    assert!(reminders(&pool).await.is_empty());
}

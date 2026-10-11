//! Recurring tasks: generated as ordinary tasks, exactly once per occurrence.

use homestead_domain::recurring::{NewTemplate, Schedule, WeekdaySet};
use homestead_domain::{TaskPriority, TaskStatus};
use homestead_store::Store;
use sqlx::PgPool;
use time::Weekday;
use time::macros::date;

fn template(title: &str, schedule: Schedule, starts_on: time::Date, catch_up: bool) -> NewTemplate {
    NewTemplate {
        title: title.into(),
        details: String::new(),
        location_id: None,
        priority: TaskPriority::Normal,
        schedule,
        starts_on,
        lead_days: 0,
        catch_up,
    }
}

async fn tasks_of(pool: &PgPool, template: uuid::Uuid) -> Vec<time::Date> {
    sqlx::query_scalar("select occurs_on from homestead.tasks where template_id = $1 order by occurs_on")
        .bind(template)
        .fetch_all(pool)
        .await
        .expect("tasks")
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_daily_chore_makes_todays_task_once_however_often_it_runs(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let t = store
        .create_template(template("Feed the hens", Schedule::Days { days: 1 }, date!(2026 - 09 - 25), false))
        .await
        .expect("template");
    // Asleep since the 25th: only today's feeding is made, not four stale ones.
    assert_eq!(store.generate_recurring_tasks_as_of(date!(2026 - 09 - 28)).await.expect("run"), 1);
    assert_eq!(store.generate_recurring_tasks_as_of(date!(2026 - 09 - 28)).await.expect("again"), 0);
    assert_eq!(tasks_of(&pool, t.id).await, vec![date!(2026 - 09 - 28)]);
    assert_eq!(store.get_template(t.id).await.expect("t").next_due_on, date!(2026 - 09 - 29));
    store.generate_recurring_tasks_as_of(date!(2026 - 09 - 29)).await.expect("next day");
    assert_eq!(tasks_of(&pool, t.id).await, vec![date!(2026 - 09 - 28), date!(2026 - 09 - 29)]);

    // They're ordinary tasks: listed, due that day, and completable.
    let open = store.list_open_tasks().await.expect("open");
    let feed = open.iter().find(|x| x.due_on == Some(date!(2026 - 09 - 28))).expect("listed");
    assert_eq!((feed.title.as_str(), feed.template_id), ("Feed the hens", Some(t.id)));
    assert_eq!(store.complete_task(feed.id).await.expect("done").status, TaskStatus::Done);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn medication_catches_up_on_every_missed_dose(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let meds = store
        .create_template(template("Give Mochi her antibiotic", Schedule::Days { days: 2 }, date!(2026 - 09 - 20), true))
        .await
        .expect("template");
    store.generate_recurring_tasks_as_of(date!(2026 - 09 - 27)).await.expect("run");
    assert_eq!(
        tasks_of(&pool, meds.id).await,
        vec![date!(2026 - 09 - 20), date!(2026 - 09 - 22), date!(2026 - 09 - 24), date!(2026 - 09 - 26)]
    );
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn concurrent_runs_never_duplicate_an_occurrence(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let t = store
        .create_template(template(
            "Irrigation inspection",
            Schedule::Weekdays { on: WeekdaySet::of(&[Weekday::Tuesday]) },
            date!(2026 - 09 - 01),
            true,
        ))
        .await
        .expect("template");
    let runs: Vec<_> = (0..8)
        .map(|_| {
            let s = store.clone();
            tokio::spawn(async move { s.generate_recurring_tasks_as_of(date!(2026 - 09 - 30)).await })
        })
        .collect();
    let mut total = 0;
    for r in runs {
        total += r.await.expect("join").expect("run");
    }
    let made = tasks_of(&pool, t.id).await;
    assert_eq!(
        made,
        vec![
            date!(2026 - 09 - 01),
            date!(2026 - 09 - 08),
            date!(2026 - 09 - 15),
            date!(2026 - 09 - 22),
            date!(2026 - 09 - 29)
        ]
    );
    assert_eq!(total, 5, "each occurrence made by exactly one run");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_monthly_count_can_appear_days_ahead(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let count = store
        .create_template(NewTemplate {
            lead_days: 3,
            ..template("Count the feed", Schedule::MonthDay { day: 1 }, date!(2026 - 09 - 15), false)
        })
        .await
        .expect("template");
    store.generate_recurring_tasks_as_of(date!(2026 - 09 - 27)).await.expect("too early");
    assert!(tasks_of(&pool, count.id).await.is_empty());
    store.generate_recurring_tasks_as_of(date!(2026 - 09 - 28)).await.expect("3 days ahead");
    assert_eq!(tasks_of(&pool, count.id).await, vec![date!(2026 - 10 - 01)]);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_paused_template_makes_nothing_and_resumes_from_now(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let today = store.today().await.expect("today");
    let t = store
        .create_template(template("Check water", Schedule::Days { days: 1 }, today - time::Duration::days(10), true))
        .await
        .expect("template");
    store.set_template_enabled(t.id, false).await.expect("pause");
    assert_eq!(store.generate_recurring_tasks().await.expect("run"), 0);
    store.set_template_enabled(t.id, true).await.expect("resume");
    store.generate_recurring_tasks().await.expect("run");
    assert_eq!(tasks_of(&pool, t.id).await, vec![today], "the paused days are not caught up");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn today_is_the_farms_date_and_time_zones_are_checked(pool: PgPool) {
    let store = Store::from_pool(pool.clone()).with_timezone("America/Los_Angeles").await.expect("tz");
    let expected: time::Date = sqlx::query_scalar("select (now() at time zone 'America/Los_Angeles')::date")
        .fetch_one(&pool)
        .await
        .expect("q");
    assert_eq!(store.today().await.expect("today"), expected);
    assert!(Store::from_pool(pool).with_timezone("America/Los_Angles").await.is_err());
}

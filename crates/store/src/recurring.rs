//! Recurring task templates and the generator that turns their occurrences
//! into ordinary tasks.
//!
//! The generator keeps no state in memory and assumes no timer: every run
//! looks at each template's cursor (`next_due_on`), makes the tasks due by
//! today, and moves the cursor past them, in one transaction per template.
//! A container that slept for a week catches up on its next request; two
//! runs at once serialise on the template row, and the unique
//! `(template_id, occurs_on)` index makes a duplicate task impossible.

use homestead_domain::recurring::{NewTemplate, Schedule, Template, WeekdaySet, to_create};
use time::{Date, Duration};
use uuid::Uuid;

use crate::{Result, Store, StoreError, Tx, parse};

macro_rules! template_cols {
    () => {
        "id, title, details, location_id, priority, schedule, cadence_days, weekdays, day_of_month, anchor_on,
         next_due_on, lead_days, catch_up, enabled"
    };
}

type Row = (
    Uuid,
    String,
    String,
    Option<Uuid>,
    String,
    String,
    Option<i32>,
    Option<i16>,
    Option<i16>,
    Date,
    Date,
    i16,
    bool,
    bool,
);

fn template(r: Row) -> Result<Template> {
    let (
        id,
        title,
        details,
        location_id,
        priority,
        schedule,
        days,
        weekdays,
        day,
        anchor_on,
        next_due_on,
        lead,
        catch_up,
        enabled,
    ) = r;
    let corrupt = || StoreError::Corrupt(format!("recurring template {id} schedule"));
    let schedule = match schedule.as_str() {
        "days" => Schedule::Days { days: u16::try_from(days.ok_or_else(corrupt)?).map_err(|_| corrupt())? },
        "weekdays" => {
            Schedule::Weekdays { on: WeekdaySet(u8::try_from(weekdays.ok_or_else(corrupt)?).map_err(|_| corrupt())?) }
        }
        "month_day" => Schedule::MonthDay { day: u8::try_from(day.ok_or_else(corrupt)?).map_err(|_| corrupt())? },
        _ => return Err(corrupt()),
    };
    Ok(Template {
        id,
        title,
        details,
        location_id,
        priority: parse(&priority)?,
        schedule,
        anchor_on,
        next_due_on,
        lead_days: u8::try_from(lead).map_err(|_| corrupt())?,
        catch_up,
        enabled,
    })
}

/// The schedule's columns: `(kind, cadence_days, weekdays, day_of_month)`.
fn columns(s: Schedule) -> (&'static str, Option<i32>, Option<i16>, Option<i16>) {
    match s {
        Schedule::Days { days } => ("days", Some(i32::from(days)), None, None),
        Schedule::Weekdays { on } => ("weekdays", None, Some(i16::from(on.0)), None),
        Schedule::MonthDay { day } => ("month_day", None, None, Some(i16::from(day))),
    }
}

/// Makes the tasks one template owes as of `today`. Returns how many.
async fn generate_one(tx: &mut Tx<'_>, id: Uuid, today: Date) -> Result<u64> {
    let row: Option<Row> = sqlx::query_as(concat!(
        "select ",
        template_cols!(),
        " from homestead.recurring_task_templates where id = $1 and enabled for update"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(t) = row.map(template).transpose()? else { return Ok(0) };
    let horizon = today + Duration::days(i64::from(t.lead_days));
    let due = t.schedule.between(t.next_due_on, horizon, t.anchor_on);
    if due.is_empty() {
        return Ok(0);
    }
    let mut made = 0;
    for occurs_on in to_create(&due, t.catch_up) {
        made += sqlx::query(
            "insert into homestead.tasks (title, details, location_id, due_on, priority, template_id, occurs_on)
             values ($1, $2, $3, $4, $5, $6, $4)
             on conflict (template_id, occurs_on) where template_id is not null do nothing",
        )
        .bind(&t.title)
        .bind(&t.details)
        .bind(t.location_id)
        .bind(occurs_on)
        .bind(t.priority.as_str())
        .bind(t.id)
        .execute(&mut **tx)
        .await?
        .rows_affected();
    }
    let next = t.schedule.next_on_or_after(horizon + Duration::days(1), t.anchor_on);
    sqlx::query("update homestead.recurring_task_templates set next_due_on = $2, updated_at = now() where id = $1")
        .bind(t.id)
        .bind(next)
        .execute(&mut **tx)
        .await?;
    Ok(made)
}

impl Store {
    pub async fn create_template(&self, input: NewTemplate) -> Result<Template> {
        let t = input.validated()?;
        let first = t.first_occurrence();
        let (kind, days, weekdays, day) = columns(t.schedule);
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.recurring_task_templates
               (title, details, location_id, priority, schedule, cadence_days, weekdays, day_of_month,
                anchor_on, next_due_on, lead_days, catch_up)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9, $10, $11) returning id",
        )
        .bind(&t.title)
        .bind(&t.details)
        .bind(t.location_id)
        .bind(t.priority.as_str())
        .bind(kind)
        .bind(days)
        .bind(weekdays)
        .bind(day)
        .bind(first)
        .bind(i16::from(t.lead_days))
        .bind(t.catch_up)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match StoreError::from(e) {
            StoreError::NotFound(_) => StoreError::NotFound("location"),
            other => other,
        })?;
        tx.commit().await?;
        self.get_template(id).await
    }

    pub async fn get_template(&self, id: Uuid) -> Result<Template> {
        let row: Option<Row> = sqlx::query_as(concat!(
            "select ",
            template_cols!(),
            " from homestead.recurring_task_templates where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        template(row.ok_or(StoreError::NotFound("recurring task"))?)
    }

    pub async fn list_templates(&self) -> Result<Vec<Template>> {
        let rows: Vec<Row> = sqlx::query_as(concat!(
            "select ",
            template_cols!(),
            " from homestead.recurring_task_templates order by enabled desc, next_due_on, title"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(template).collect()
    }

    /// Pausing keeps the cursor where it is; resuming skips what was missed
    /// while paused (catch-up templates included: a pause is deliberate).
    pub async fn set_template_enabled(&self, id: Uuid, enabled: bool) -> Result<()> {
        let today = self.today().await?;
        let t = self.get_template(id).await?;
        let resume_from = t.schedule.next_on_or_after(today.max(t.next_due_on), t.anchor_on);
        let mut tx = self.begin().await?;
        sqlx::query(
            "update homestead.recurring_task_templates
             set enabled = $2, updated_at = now(), next_due_on = case when $2 and not enabled then $3 else next_due_on end
             where id = $1",
        )
        .bind(id)
        .bind(enabled)
        .bind(resume_from)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Makes every recurring task due as of today (in the farm's time zone).
    /// Safe to call as often as you like, from anywhere.
    pub async fn generate_recurring_tasks(&self) -> Result<u64> {
        let today = self.today().await?;
        self.generate_recurring_tasks_as_of(today).await
    }

    pub async fn generate_recurring_tasks_as_of(&self, today: Date) -> Result<u64> {
        let due: Vec<Uuid> = sqlx::query_scalar(
            "select id from homestead.recurring_task_templates
             where enabled and next_due_on - lead_days <= $1 order by next_due_on",
        )
        .bind(today)
        .fetch_all(&self.pool)
        .await?;
        let mut made = 0;
        for id in due {
            let mut tx = self.begin().await?;
            made += generate_one(&mut tx, id, today).await?;
            tx.commit().await?;
        }
        Ok(made)
    }
}

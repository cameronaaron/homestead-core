//! The morning digest: gathers open tasks and low stock once a farm day
//! and announces them (see `homestead_domain::digest`).

use homestead_domain::digest::{Digest, DigestTask, HOURS};
use homestead_domain::{Event, TaskPriority};
use rust_decimal::Decimal;
use time::Date;

use crate::{Result, Store, outbox, parse};

impl Store {
    /// Sends today's digest if it's morning on the farm and it hasn't gone
    /// out yet. Returns the digest when one was posted.
    pub async fn send_daily_digest(&self) -> Result<Option<Digest>> {
        let (today, hour): (Date, i32) =
            sqlx::query_as("select (now() at time zone $1)::date, extract(hour from now() at time zone $1)::int")
                .bind(&*self.timezone)
                .fetch_one(&self.pool)
                .await?;
        if !u8::try_from(hour).is_ok_and(|h| HOURS.contains(&h)) {
            return Ok(None);
        }
        self.send_daily_digest_as_of(today).await
    }

    /// Claims `today` and, when there's anything to say, emits one
    /// `digest.daily`, in one transaction. A second run the same day finds
    /// the date claimed (waiting for the first to commit if they race) and
    /// posts nothing; a quiet day is claimed too, so it stays quiet.
    pub async fn send_daily_digest_as_of(&self, today: Date) -> Result<Option<Digest>> {
        let mut tx = self.begin().await?;
        let claimed = sqlx::query(
            "insert into homestead.daily_digests (on_date, posted) values ($1, false) on conflict do nothing",
        )
        .bind(today)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if claimed == 0 {
            return Ok(None);
        }
        let rows: Vec<(String, Option<Date>, String, Option<String>)> = sqlx::query_as(
            "select t.title, t.due_on, t.priority, coalesce(a.display_name, nullif(l.assignee, ''))
             from homestead.tasks t
             left join homestead.actors a on a.id = t.assigned_to
             left join homestead.linear_issues l on l.task_id = t.id
             where t.status = 'open'
             order by t.due_on nulls last,
                      array_position(array['urgent','high','normal','low'], t.priority), t.title",
        )
        .fetch_all(&mut *tx)
        .await?;
        let mut digest =
            Digest { on: today, overdue: vec![], due_today: vec![], high_priority: vec![], low_stock: vec![] };
        for (title, due_on, priority, assignee) in rows {
            let task =
                DigestTask { title, due_on, priority: parse(&priority)?, assignee: assignee.unwrap_or_default() };
            match due_on {
                Some(d) if d < today => digest.overdue.push(task),
                Some(d) if d == today => digest.due_today.push(task),
                _ if matches!(task.priority, TaskPriority::Urgent | TaskPriority::High) => {
                    digest.high_priority.push(task)
                }
                _ => {}
            }
        }
        let low: Vec<(String, Decimal, String)> = sqlx::query_as(
            "select name, on_hand, unit from homestead.inventory_items
             where reorder_level > 0 and on_hand <= reorder_level order by name",
        )
        .fetch_all(&mut *tx)
        .await?;
        digest.low_stock =
            low.into_iter().map(|(name, n, unit)| format!("{name} ({} {unit})", n.normalize())).collect();
        if digest.is_empty() {
            tx.commit().await?;
            return Ok(None);
        }
        sqlx::query("update homestead.daily_digests set posted = true where on_date = $1")
            .bind(today)
            .execute(&mut *tx)
            .await?;
        outbox::emit(&mut tx, &Event::DailyDigest { digest: digest.clone() }).await?;
        tx.commit().await?;
        Ok(Some(digest))
    }
}

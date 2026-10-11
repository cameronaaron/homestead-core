//! Transactional outbox: the storage half of the integration bus.
//!
//! Lifecycle of an event:
//! 1. [`emit`] appends it inside the domain write's transaction.
//! 2. [`Store::fan_out`] turns it into one `deliveries` row per sink whose
//!    routing accepts it.
//! 3. [`Store::claim_due`] leases due deliveries to a worker. The lease is the
//!    `next_attempt_at` push-forward: a worker that dies mid-delivery simply
//!    lets the lease lapse and another worker retries. Delivery is therefore
//!    at-least-once — sinks must tolerate repeats (they get the event id).
//! 4. [`Store::mark_delivered`] / [`Store::mark_failed`] settle it, with
//!    retries dead-lettering after `max_attempts`.

use std::time::Duration;

use homestead_domain::Event;
use serde_json::Value;
use time::OffsetDateTime;

use crate::{Result, Store, Tx};

/// Sink name recorded for events that could not be decoded at fan-out time,
/// so they surface in the dead-letter view instead of vanishing.
pub const UNDECODABLE_SINK: &str = "_undecodable";

pub(crate) async fn emit(tx: &mut Tx<'_>, event: &Event) -> Result<i64> {
    Ok(sqlx::query_scalar("insert into homestead.events (kind, payload) values ($1, $2) returning id")
        .bind(event.kind())
        .bind(event.payload())
        .fetch_one(&mut **tx)
        .await?)
}

#[derive(Debug, Clone)]
pub struct Claimed {
    pub event_id: i64,
    pub sink: String,
    /// Attempts made before this one.
    pub attempts: i32,
    pub kind: String,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeliveryRecord {
    pub event_id: i64,
    pub sink: String,
    pub kind: String,
    pub status: String,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub occurred_at: OffsetDateTime,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BusStats {
    pub unfanned: i64,
    pub pending: i64,
    pub dead: i64,
    pub delivered: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecentEvent {
    pub id: i64,
    pub kind: String,
    pub payload: Value,
    pub occurred_at: OffsetDateTime,
}

type DeliveryRow = (i64, String, String, String, i32, Option<String>, OffsetDateTime);

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

impl Store {
    /// Fans out up to `limit` unrouted events. `route` names the sinks that
    /// want a decoded event. Returns how many events were fanned out.
    pub async fn fan_out(&self, limit: i64, route: &(dyn Fn(&Event) -> Vec<&'static str> + Sync)) -> Result<usize> {
        let mut tx = self.pool.begin().await?;
        let batch: Vec<(i64, String, Value)> = sqlx::query_as(
            "select id, kind, payload from homestead.events where fanned_out_at is null
             order by id limit $1 for update skip locked",
        )
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        for (id, kind, payload) in &batch {
            let (sinks, error): (Vec<String>, Option<String>) = match Event::from_parts(kind, payload.clone()) {
                Ok(ev) => (route(&ev).into_iter().map(str::to_owned).collect(), None),
                Err(e) => {
                    tracing::error!(event_id = id, %kind, error = %e, "undecodable event dead-lettered");
                    (vec![UNDECODABLE_SINK.to_owned()], Some(format!("cannot decode {kind}: {e}")))
                }
            };
            let status = if error.is_some() { "dead" } else { "pending" };
            sqlx::query(
                "insert into homestead.deliveries (event_id, sink, status, last_error)
                 select $1, s, $3, $4 from unnest($2::text[]) s on conflict do nothing",
            )
            .bind(id)
            .bind(&sinks)
            .bind(status)
            .bind(error)
            .execute(&mut *tx)
            .await?;
            sqlx::query("update homestead.events set fanned_out_at = now() where id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(batch.len())
    }

    /// Leases up to `limit` due deliveries for `lease`. Concurrent workers
    /// never receive the same delivery while its lease is live.
    pub async fn claim_due(&self, limit: i64, lease: Duration) -> Result<Vec<Claimed>> {
        let rows: Vec<(i64, String, i32, String, Value)> = sqlx::query_as(
            "with due as (
               select event_id, sink from homestead.deliveries
               where status = 'pending' and next_attempt_at <= now()
               order by next_attempt_at limit $1 for update skip locked
             )
             update homestead.deliveries d
               set next_attempt_at = now() + make_interval(secs => $2)
             from due, homestead.events e
             where d.event_id = due.event_id and d.sink = due.sink and e.id = d.event_id
             returning d.event_id, d.sink, d.attempts, e.kind, e.payload",
        )
        .bind(limit)
        .bind(secs(lease))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(event_id, sink, attempts, kind, payload)| Claimed { event_id, sink, attempts, kind, payload })
            .collect())
    }

    pub async fn mark_delivered(&self, event_id: i64, sink: &str) -> Result<()> {
        sqlx::query(
            "update homestead.deliveries set status = 'delivered', delivered_at = now(),
               attempts = attempts + 1, last_error = null
             where event_id = $1 and sink = $2 and status = 'pending'",
        )
        .bind(event_id)
        .bind(sink)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Records a failed attempt. `retry_in: None` dead-letters immediately
    /// (permanent failure or attempts exhausted — the caller decides).
    pub async fn mark_failed(&self, event_id: i64, sink: &str, error: &str, retry_in: Option<Duration>) -> Result<()> {
        sqlx::query(
            "update homestead.deliveries set attempts = attempts + 1, last_error = $3,
               status = case when $4::float8 is null then 'dead' else 'pending' end,
               next_attempt_at = now() + make_interval(secs => coalesce($4::float8, 0))
             where event_id = $1 and sink = $2 and status = 'pending'",
        )
        .bind(event_id)
        .bind(sink)
        .bind(error)
        .bind(retry_in.map(secs))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Puts a dead delivery back in the queue with a fresh attempt budget.
    pub async fn retry_dead(&self, event_id: i64, sink: &str) -> Result<bool> {
        let n = sqlx::query(
            "update homestead.deliveries set status = 'pending', attempts = 0, next_attempt_at = now()
             where event_id = $1 and sink = $2 and status = 'dead' and sink <> $3",
        )
        .bind(event_id)
        .bind(sink)
        .bind(UNDECODABLE_SINK)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n == 1)
    }

    pub async fn bus_stats(&self) -> Result<BusStats> {
        let (unfanned, pending, dead, delivered): (i64, i64, i64, i64) = sqlx::query_as(
            "select
               (select count(*) from homestead.events where fanned_out_at is null),
               (select count(*) from homestead.deliveries where status = 'pending'),
               (select count(*) from homestead.deliveries where status = 'dead'),
               (select count(*) from homestead.deliveries where status = 'delivered')",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(BusStats { unfanned, pending, dead, delivered })
    }

    /// Deliveries that are dead or have failed at least once, newest first.
    pub async fn troubled_deliveries(&self, limit: i64) -> Result<Vec<DeliveryRecord>> {
        let rows: Vec<DeliveryRow> = sqlx::query_as(
            "select d.event_id, d.sink, e.kind, d.status, d.attempts, d.last_error, e.occurred_at
             from homestead.deliveries d join homestead.events e on e.id = d.event_id
             where d.status = 'dead' or (d.status = 'pending' and d.last_error is not null)
             order by d.event_id desc limit $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(event_id, sink, kind, status, attempts, last_error, occurred_at)| DeliveryRecord {
                event_id,
                sink,
                kind,
                status,
                attempts,
                last_error,
                occurred_at,
            })
            .collect())
    }

    pub async fn recent_events(&self, limit: i64) -> Result<Vec<RecentEvent>> {
        let rows: Vec<(i64, String, Value, OffsetDateTime)> =
            sqlx::query_as("select id, kind, payload, occurred_at from homestead.events order by id desc limit $1")
                .bind(limit)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|(id, kind, payload, occurred_at)| RecentEvent { id, kind, payload, occurred_at })
            .collect())
    }
}

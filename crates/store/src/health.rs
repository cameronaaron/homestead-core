//! Crop-health observations, their follow-up tasks, and how they ended.

use homestead_domain::health::{NewObservation, Observation, ObservationCategory, Resolution};
use homestead_domain::{NewTask, TaskPriority};
use time::Date;
use uuid::Uuid;

use crate::{Result, Store, StoreError, parse};

macro_rules! observation_cols {
    () => {
        "o.id, o.observed_on, o.location_id, l.name, o.planting_id, p.crop, o.category, o.observation, o.severity,
         o.affected, o.action_taken, o.follow_up_on, o.outcome, o.resolved_on
         from homestead.crop_observations o
         left join homestead.plantings p on p.id = o.planting_id
         left join homestead.locations l on l.id = o.location_id"
    };
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    observed_on: Date,
    location_id: Option<Uuid>,
    name: Option<String>,
    planting_id: Option<Uuid>,
    crop: Option<String>,
    category: String,
    observation: String,
    severity: String,
    affected: String,
    action_taken: String,
    follow_up_on: Option<Date>,
    outcome: String,
    resolved_on: Option<Date>,
}

impl TryFrom<Row> for Observation {
    type Error = StoreError;
    fn try_from(r: Row) -> Result<Observation> {
        Ok(Observation {
            id: r.id,
            observed_on: r.observed_on,
            location_id: r.location_id,
            place: r.name,
            planting_id: r.planting_id,
            crop: r.crop,
            category: parse(&r.category)?,
            observation: r.observation,
            severity: parse(&r.severity)?,
            affected: r.affected,
            action_taken: r.action_taken,
            follow_up_on: r.follow_up_on,
            outcome: r.outcome,
            resolved_on: r.resolved_on,
        })
    }
}

/// Which observations to list.
#[derive(Debug, Clone, Default)]
pub struct ObservationFilter {
    /// This place and every place inside it.
    pub location_id: Option<Uuid>,
    /// Plantings of this crop (any case).
    pub crop: Option<String>,
    pub open_only: bool,
}

/// How often each kind of trouble was seen on a crop (or, with no planting
/// named, a place).
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct HealthTally {
    pub subject: String,
    pub category: String,
    pub seen: i64,
    pub high: i64,
    pub last_seen: Date,
}

impl Store {
    /// Records an observation. Naming a planting fills in its place; a
    /// follow-up date makes an ordinary task, closed when the observation is.
    pub async fn record_observation(&self, input: NewObservation) -> Result<Observation> {
        let o = input.validated()?;
        let mut tx = self.begin().await?;
        let location_id = match (o.location_id, o.planting_id) {
            (Some(l), _) => Some(l),
            (None, Some(p)) => Some(
                sqlx::query_scalar("select location_id from homestead.plantings where id = $1")
                    .bind(p)
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or(StoreError::NotFound("planting"))?,
            ),
            (None, None) => None,
        };
        let task = match o.follow_up_on {
            Some(due) => {
                let short: String = o.observation.chars().take(80).collect();
                let t = NewTask {
                    title: format!("Follow up: {short}"),
                    details: format!("Crop health ({}, {}) seen {}", o.category, o.severity, o.observed_on),
                    location_id,
                    due_on: Some(due),
                    priority: if o.severity == homestead_domain::health::Severity::High {
                        TaskPriority::High
                    } else {
                        TaskPriority::Normal
                    },
                    mirror_to_linear: false,
                    assigned_to: None,
                };
                let task = crate::insert_task(&mut tx, t).await?;
                Some(task.id)
            }
            None => None,
        };
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.crop_observations (observed_on, location_id, planting_id, category, observation,
               severity, affected, action_taken, follow_up_on, follow_up_task_id)
             values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) returning id",
        )
        .bind(o.observed_on)
        .bind(location_id)
        .bind(o.planting_id)
        .bind(o.category.as_str())
        .bind(&o.observation)
        .bind(o.severity.as_str())
        .bind(&o.affected)
        .bind(&o.action_taken)
        .bind(o.follow_up_on)
        .bind(task)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        self.get_observation(id).await
    }

    pub async fn get_observation(&self, id: Uuid) -> Result<Observation> {
        let row: Row = sqlx::query_as(concat!("select ", observation_cols!(), " where o.id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound("observation"))?;
        row.try_into()
    }

    /// Closes an observation with how it ended, and its follow-up task with
    /// it. Closing one already closed changes nothing.
    pub async fn resolve_observation(&self, id: Uuid, input: Resolution) -> Result<Observation> {
        let r = input.validated()?;
        let mut tx = self.begin().await?;
        let task: Option<Option<Uuid>> = sqlx::query_scalar(
            "update homestead.crop_observations set resolved_on = $2, outcome = $3
             where id = $1 and resolved_on is null returning follow_up_task_id",
        )
        .bind(id)
        .bind(r.resolved_on)
        .bind(&r.outcome)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(Some(task)) = task {
            crate::close_task(&mut tx, task, &self.timezone).await?;
        }
        tx.commit().await?;
        self.get_observation(id).await
    }

    /// Observations newest first, filtered by place (and what's inside it),
    /// crop, and whether they're still open.
    pub async fn observations(&self, f: &ObservationFilter, limit: i64) -> Result<Vec<Observation>> {
        let rows: Vec<Row> = sqlx::query_as(concat!(
            "with recursive place as (
               select id from homestead.locations where id = $1
               union all select c.id from homestead.locations c join place on c.parent_id = place.id
             ) select ",
            observation_cols!(),
            " where ($1::uuid is null or o.location_id in (select id from place))
                and ($2::text is null or lower(p.crop) = lower($2))
                and (not $3 or o.resolved_on is null)
              order by o.observed_on desc, o.created_at desc limit $4"
        ))
        .bind(f.location_id)
        .bind(f.crop.as_deref().map(str::trim).filter(|c| !c.is_empty()))
        .bind(f.open_only)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// What trouble each crop (or place) has had since `since`, most seen first.
    pub async fn health_tally(&self, since: Date) -> Result<Vec<HealthTally>> {
        Ok(sqlx::query_as(
            "select coalesce(p.crop, l.name, 'unknown') as subject, o.category, count(*) as seen,
                    count(*) filter (where o.severity = 'high') as high, max(o.observed_on) as last_seen
             from homestead.crop_observations o
             left join homestead.plantings p on p.id = o.planting_id
             left join homestead.locations l on l.id = o.location_id
             where o.observed_on >= $1
             group by 1, 2 order by seen desc, last_seen desc",
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await?)
    }
}

impl HealthTally {
    pub fn category(&self) -> Result<ObservationCategory> {
        parse(&self.category)
    }
}

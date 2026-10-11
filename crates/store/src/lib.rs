//! Postgres persistence for farm operations.
//!
//! Every mutating method runs in one transaction that also appends its
//! [`Event`] to the outbox (see [`outbox`]), so "the change committed" and
//! "integrations will hear about it" are the same fact.

pub mod adoptions;
pub mod animals;
pub mod attachments;
pub mod auth;
pub mod community;
pub mod contacts;
pub mod crops;
pub mod deadlines;
pub mod digest;
pub mod equipment;
pub mod field;
pub mod gifts;
pub mod grants;
pub mod harvests;
pub mod health;
pub mod incidents;
pub mod inventory;
pub mod linear;
pub mod map;
pub mod movements;
pub mod notify;
pub mod ops;
pub mod outbox;
pub mod places;
pub mod readings;
pub mod recurring;
pub mod releases;
pub mod reports;
pub mod search;
pub mod seeds;
pub mod soil;
pub mod stand;
pub mod tags;
pub mod water;

use homestead_domain::{
    Event, Harvest, InventoryItem, Location, NewHarvest, NewInventoryItem, NewLocation, NewPlanting, NewTask, Planting,
    PlantingStatus, Task, TaskStatus, ValidationError,
};
use rust_decimal::Decimal;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{Postgres, Transaction};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct AuditEntry {
    pub id: i64,
    pub occurred_at: OffsetDateTime,
    pub actor_label: String,
    pub request_id: Option<String>,
    pub action: String,
    pub resource_type: String,
    pub resource_id: Option<String>,
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
    pub metadata: serde_json::Value,
}

/// The audit entries a caller may see: their table's capability, or for
/// attachments, the owning record's (`$1` tables, `$2` owner kinds).
macro_rules! audit_visible {
    () => {
        "(resource_type = any($1)
          or (resource_type = 'attachments' and coalesce(after, before) ->> 'resource_type' = any($2)))"
    };
}
pub(crate) use audit_visible;

pub(crate) fn audit_scope(caps: homestead_domain::auth::CapabilitySet) -> (Vec<String>, Vec<String>) {
    use homestead_domain::attachments::AttachmentOwner;
    let tables = homestead_domain::auth::visible_audit_tables(caps).into_iter().map(str::to_owned).collect();
    let owners =
        AttachmentOwner::ALL.iter().filter(|o| caps.has(o.read_cap())).map(|o| o.as_str().to_owned()).collect();
    (tables, owners)
}

pub enum IdempotencyClaim {
    Proceed,
    Replay { status: i16, body: Vec<u8> },
    InProgress,
}

/// Column list matching [`TaskRow`]; a macro so queries stay `&'static str`.
macro_rules! task_cols {
    () => {
        "id, title, details, location_id, due_on, priority, status, mirror_to_linear, linear_issue_id, completed_at,
         template_id, assigned_to"
    };
}

/// Columns matching [`PlantingRow`], joined to the planting's location.
macro_rules! planting_cols {
    () => {
        "p.id, p.location_id, p.crop, p.variety, p.sown_on, p.expected_harvest_on, p.status, p.notes, p.seed_lot_id,
         p.crop_plan_id, p.family, p.method, p.expected_yield, p.expected_yield_unit, p.ended_on,
         p.transplant_on, p.expected_harvest_until, p.seed_quantity_used, l.name as location_name
         from homestead.plantings p join homestead.locations l on l.id = p.location_id"
    };
}
pub(crate) use planting_cols;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Invalid(#[from] ValidationError),
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("{0}")]
    Conflict(String),
    #[error("not enough stock there: it would go below zero")]
    InsufficientStock,
    #[error("stored value is not valid: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Db(sqlx::Error),
}

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        if let sqlx::Error::Database(db) = &e {
            match (db.code().as_deref(), db.constraint()) {
                (Some("23514"), Some("inventory_items_on_hand_check" | "inventory_balances_on_hand_check")) => {
                    return StoreError::InsufficientStock;
                }
                (Some("23505"), c) => {
                    return StoreError::Conflict(format!("already exists ({})", c.unwrap_or("unique")));
                }
                (Some("23503"), _) => return StoreError::NotFound("referenced record"),
                _ if db.message().contains("location parent") => {
                    return StoreError::Conflict("that parent would make a loop".into());
                }
                // A trigger's business rule (`raise exception`). Their messages
                // name the rule, never a row's data, so they're safe to show.
                (Some("P0001"), _) => return StoreError::Conflict(db.message().to_owned()),
                _ => {}
            }
        }
        StoreError::Db(e)
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

pub(crate) fn parse<T: std::str::FromStr<Err = ValidationError>>(s: &str) -> Result<T> {
    s.parse().map_err(|e: ValidationError| StoreError::Corrupt(e.to_string()))
}

/// Who a write is attributed to in `audit_entries`. `id` is the
/// `actors` row when there is one; an unlinked Discord user or the command
/// line is named by `label` alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub id: Option<Uuid>,
    pub label: String,
}

/// A handle on the database. A plain `Store` writes as `system`; one from
/// [`Store::acting_as`] stamps every row its transactions touch with that
/// actor, via transaction-local settings the audit trigger reads.
#[derive(Clone)]
pub struct Store {
    pool: PgPool,
    actor: Option<std::sync::Arc<Actor>>,
    /// The farm's IANA time zone: what "today" means for due dates, the
    /// fiscal year and default form dates.
    timezone: std::sync::Arc<str>,
}

impl Store {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new().max_connections(10).connect(database_url).await?;
        Ok(Self::from_pool(pool))
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool, actor: None, timezone: "UTC".into() }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The same database, with writes attributed to `actor`.
    pub fn acting_as(&self, actor: Actor) -> Store {
        Store { pool: self.pool.clone(), actor: Some(std::sync::Arc::new(actor)), timezone: self.timezone.clone() }
    }

    /// The same store, with "today" in `timezone` (checked against the
    /// database's time zone list, so a typo fails at startup).
    pub async fn with_timezone(mut self, timezone: &str) -> Result<Store> {
        let known: bool = sqlx::query_scalar("select exists (select 1 from pg_timezone_names where name = $1)")
            .bind(timezone)
            .fetch_one(&self.pool)
            .await?;
        if !known {
            return Err(StoreError::Invalid(ValidationError::Malformed(
                "time zone (use an IANA name like America/Los_Angeles)",
            )));
        }
        self.timezone = timezone.into();
        Ok(self)
    }

    /// Today's date on the farm, not in UTC (at 5 pm in Los Angeles it is
    /// already tomorrow in UTC).
    pub async fn today(&self) -> Result<Date> {
        Ok(sqlx::query_scalar("select (now() at time zone $1)::date")
            .bind(&*self.timezone)
            .fetch_one(&self.pool)
            .await?)
    }

    pub fn actor(&self) -> Option<&Actor> {
        self.actor.as_deref()
    }

    /// Starts a transaction that attributes its writes. Every write goes
    /// through one of these (`writes_are_attributed` checks the source):
    /// the settings are transaction-local, so a pooled connection never
    /// carries one request's identity into the next.
    pub(crate) async fn begin(&self) -> Result<Tx<'static>> {
        let mut tx = self.pool.begin().await?;
        if let Some(a) = &self.actor {
            sqlx::query("select set_config('homestead.actor', $1, true), set_config('homestead.actor_id', $2, true)")
                .bind(&a.label)
                .bind(a.id.map(|id| id.to_string()).unwrap_or_default())
                .execute(&mut *tx)
                .await?;
        }
        Ok(tx)
    }

    pub async fn migrate(&self) -> std::result::Result<(), sqlx::migrate::MigrateError> {
        MIGRATOR.run(&self.pool).await
    }

    pub async fn ping(&self) -> Result<()> {
        sqlx::query("select 1").execute(&self.pool).await?;
        Ok(())
    }

    /// Append a verified Zeffy delivery once. The event id is stable across
    /// retries, so duplicate deliveries acknowledge without duplicating data.
    pub async fn record_zeffy_event(
        &self,
        id: Uuid,
        event_type: &str,
        schema_version: i32,
        dispatched_at: OffsetDateTime,
        payload: serde_json::Value,
    ) -> Result<bool> {
        let mut tx = self.begin().await?;
        let inserted = sqlx::query(
            "insert into homestead.zeffy_webhook_events (id, event_type, schema_version, dispatched_at, payload)
             values ($1, $2, $3, $4, $5) on conflict (id) do nothing",
        )
        .bind(id)
        .bind(event_type)
        .bind(schema_version)
        .bind(dispatched_at)
        .bind(payload)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        tx.commit().await?;
        Ok(inserted)
    }

    /// Recent audit history, only for records `caps` may read: an entry
    /// holds the whole row, so it is exactly as private as the row.
    pub async fn recent_audit_entries(
        &self,
        caps: homestead_domain::auth::CapabilitySet,
        limit: i64,
    ) -> Result<Vec<AuditEntry>> {
        let (tables, owners) = audit_scope(caps);
        Ok(sqlx::query_as(concat!(
            "select id, occurred_at, actor_label, request_id, action, resource_type, resource_id,
                    before, after, metadata
             from homestead.audit_entries where ",
            audit_visible!(),
            " order by id desc limit $3"
        ))
        .bind(tables)
        .bind(owners)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn claim_idempotency(&self, scope: &str, key: &str, request_hash: &str) -> Result<IdempotencyClaim> {
        let inserted = sqlx::query(
            "insert into homestead.idempotency_records (scope, key, request_hash) values ($1, $2, $3)
             on conflict (scope, key) do nothing",
        )
        .bind(scope)
        .bind(key)
        .bind(request_hash)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1;
        if inserted {
            return Ok(IdempotencyClaim::Proceed);
        }
        let (stored_hash, status, body): (String, Option<i16>, Option<Vec<u8>>) = sqlx::query_as(
            "select request_hash, response_status, response_body from homestead.idempotency_records
             where scope = $1 and key = $2",
        )
        .bind(scope)
        .bind(key)
        .fetch_one(&self.pool)
        .await?;
        if stored_hash != request_hash {
            return Err(StoreError::Conflict("idempotency key was already used for a different request".into()));
        }
        Ok(match (status, body) {
            (Some(status), Some(body)) => IdempotencyClaim::Replay { status, body },
            _ => IdempotencyClaim::InProgress,
        })
    }

    pub async fn finish_idempotency(&self, scope: &str, key: &str, status: i16, body: &[u8]) -> Result<()> {
        sqlx::query(
            "update homestead.idempotency_records
             set response_status = $3, response_body = $4, response_headers = '{}'::jsonb, completed_at = now()
             where scope = $1 and key = $2 and completed_at is null",
        )
        .bind(scope)
        .bind(key)
        .bind(status)
        .bind(body)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ── Locations ───────────────────────────────────────────────────────────

    pub async fn create_location(&self, input: NewLocation) -> Result<Location> {
        let l = input.validated()?;
        let mut tx = self.begin().await?;
        let (id, created_at): (Uuid, OffsetDateTime) = sqlx::query_as(
            "insert into homestead.locations (name, kind, notes, parent_id, active, area_sqft, soil_notes)
             values ($1, $2, $3, $4, $5, $6, $7) returning id, created_at",
        )
        .bind(&l.name)
        .bind(l.kind.as_str())
        .bind(&l.notes)
        .bind(l.parent_id)
        .bind(l.active)
        .bind(l.area_sqft)
        .bind(&l.soil_notes)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Location {
            id,
            name: l.name,
            kind: l.kind,
            notes: l.notes,
            parent_id: l.parent_id,
            active: l.active,
            area_sqft: l.area_sqft,
            soil_notes: l.soil_notes,
            created_at,
        })
    }

    pub async fn list_locations(&self) -> Result<Vec<Location>> {
        let rows = location_rows(
            sqlx::query_as(
                "select id, name, kind, notes, parent_id, active, area_sqft, soil_notes, created_at
                 from homestead.locations order by active desc, name",
            )
            .fetch_all(&self.pool)
            .await?,
        )?;
        Ok(rows)
    }

    pub async fn get_location(&self, id: Uuid) -> Result<Location> {
        let row = sqlx::query_as(
            "select id, name, kind, notes, parent_id, active, area_sqft, soil_notes, created_at
             from homestead.locations where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound("location"))?;
        Ok(location_rows(vec![row])?.pop().expect("one"))
    }
}

/// Adds and announces a task inside `tx`, for work another record creates
/// (a repair after an irrigation problem, a follow-up).
pub(crate) async fn insert_task(tx: &mut Tx<'_>, input: NewTask) -> Result<Task> {
    let t = input.validated()?;
    let id: Uuid = sqlx::query_scalar(
        "insert into homestead.tasks (title, details, location_id, due_on, priority, mirror_to_linear, assigned_to)
         values ($1, $2, $3, $4, $5, $6, $7) returning id",
    )
    .bind(&t.title)
    .bind(&t.details)
    .bind(t.location_id)
    .bind(t.due_on)
    .bind(t.priority.as_str())
    .bind(t.mirror_to_linear)
    .bind(t.assigned_to)
    .fetch_one(&mut **tx)
    .await?;
    let task = Task {
        id,
        title: t.title,
        details: t.details,
        location_id: t.location_id,
        due_on: t.due_on,
        priority: t.priority,
        status: TaskStatus::Open,
        mirror_to_linear: t.mirror_to_linear,
        linear_issue_id: None,
        completed_at: None,
        template_id: None,
        assigned_to: t.assigned_to,
    };
    outbox::emit(tx, &Event::TaskCreated { task: task.clone() }).await?;
    Ok(task)
}

/// Closes an open task inside `tx`, announcing it and recording what a
/// planting task did. `None` if it wasn't open.
pub(crate) async fn close_task(tx: &mut Tx<'_>, id: Uuid, timezone: &str) -> Result<Option<Task>> {
    let row: Option<TaskRow> = sqlx::query_as(concat!(
        "update homestead.tasks set status = 'done', completed_at = now()
         where id = $1 and status = 'open' returning ",
        task_cols!()
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let task: Task = row.try_into()?;
    outbox::emit(tx, &Event::TaskCompleted { task: task.clone() }).await?;
    let today: Date =
        sqlx::query_scalar("select (now() at time zone $1)::date").bind(timezone).fetch_one(&mut **tx).await?;
    if let Some(planted) = crops::planting_step_done(tx, id, today).await? {
        outbox::emit(tx, &planted).await?;
    }
    Ok(Some(task))
}

/// The announcement that a planting is in the ground, read inside `tx`.
pub(crate) async fn planting_created(tx: &mut Tx<'_>, id: Uuid) -> Result<Event> {
    let row: PlantingRow =
        sqlx::query_as(concat!("select ", planting_cols!(), " where p.id = $1")).bind(id).fetch_one(&mut **tx).await?;
    let (planting, location_name) = row.with_place()?;
    Ok(Event::PlantingCreated { planting, location_name })
}

#[derive(sqlx::FromRow)]
pub(crate) struct PlantingRow {
    pub(crate) id: Uuid,
    location_id: Uuid,
    crop: String,
    variety: String,
    sown_on: Date,
    expected_harvest_on: Option<Date>,
    status: String,
    notes: String,
    seed_lot_id: Option<Uuid>,
    crop_plan_id: Option<Uuid>,
    family: String,
    method: String,
    expected_yield: Option<Decimal>,
    expected_yield_unit: Option<String>,
    ended_on: Option<Date>,
    transplant_on: Option<Date>,
    expected_harvest_until: Option<Date>,
    pub(crate) seed_quantity_used: Option<Decimal>,
    pub(crate) location_name: String,
}

impl PlantingRow {
    pub(crate) fn planting(&self) -> Result<Planting> {
        Ok(Planting {
            id: self.id,
            location_id: self.location_id,
            crop: self.crop.clone(),
            variety: self.variety.clone(),
            sown_on: self.sown_on,
            expected_harvest_on: self.expected_harvest_on,
            status: parse(&self.status)?,
            notes: self.notes.clone(),
            seed_lot_id: self.seed_lot_id,
            crop_plan_id: self.crop_plan_id,
            family: self.family.clone(),
            method: parse(&self.method)?,
            expected_yield: self.expected_yield,
            expected_yield_unit: self.expected_yield_unit.as_deref().map(parse).transpose()?,
            ended_on: self.ended_on,
            transplant_on: self.transplant_on,
            expected_harvest_until: self.expected_harvest_until,
        })
    }

    pub(crate) fn with_place(self) -> Result<(Planting, String)> {
        Ok((self.planting()?, self.location_name))
    }
}

pub(crate) type LocationRow =
    (Uuid, String, String, String, Option<Uuid>, bool, Option<Decimal>, String, OffsetDateTime);

pub(crate) fn location_rows(rows: Vec<LocationRow>) -> Result<Vec<Location>> {
    rows.into_iter()
        .map(|(id, name, kind, notes, parent_id, active, area_sqft, soil_notes, created_at)| {
            Ok(Location { id, name, kind: parse(&kind)?, notes, parent_id, active, area_sqft, soil_notes, created_at })
        })
        .collect()
}

impl Store {
    // ── Plantings & harvests ────────────────────────────────────────────────

    pub async fn create_planting(&self, input: NewPlanting) -> Result<Planting> {
        let p = input.validated()?;
        let mut tx = self.begin().await?;
        let location_name: String = sqlx::query_scalar("select name from homestead.locations where id = $1")
            .bind(p.location_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StoreError::NotFound("location"))?;
        if let Some(lot) = p.seed_lot_id {
            seeds::take_seed(&mut tx, lot, p.seed_quantity_used).await?;
        }
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.plantings
               (location_id, crop, variety, sown_on, expected_harvest_on, notes, seed_lot_id, seed_quantity_used,
                crop_plan_id, family, method, expected_yield, expected_yield_unit, status)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) returning id",
        )
        .bind(p.location_id)
        .bind(&p.crop)
        .bind(&p.variety)
        .bind(p.sown_on)
        .bind(p.expected_harvest_on)
        .bind(&p.notes)
        .bind(p.seed_lot_id)
        .bind(p.seed_quantity_used)
        .bind(p.crop_plan_id)
        .bind(&p.family)
        .bind(p.method.as_str())
        .bind(p.expected_yield)
        .bind(p.expected_yield_unit.map(|u| u.as_str()))
        .bind(p.status.unwrap_or(PlantingStatus::Growing).as_str())
        .fetch_one(&mut *tx)
        .await?;
        let status = p.status.unwrap_or(PlantingStatus::Growing);
        let planting = Planting {
            id,
            location_id: p.location_id,
            crop: p.crop,
            variety: p.variety,
            sown_on: p.sown_on,
            expected_harvest_on: p.expected_harvest_on,
            status,
            notes: p.notes,
            seed_lot_id: p.seed_lot_id,
            crop_plan_id: p.crop_plan_id,
            family: p.family,
            method: p.method,
            expected_yield: p.expected_yield,
            expected_yield_unit: p.expected_yield_unit,
            ended_on: None,
            transplant_on: None,
            expected_harvest_until: None,
        };
        // A plan isn't news; it's announced when it goes in the ground.
        if status != PlantingStatus::Planned {
            outbox::emit(&mut tx, &Event::PlantingCreated { planting: planting.clone(), location_name }).await?;
        }
        tx.commit().await?;
        Ok(planting)
    }

    pub async fn get_planting(&self, id: Uuid) -> Result<(Planting, String)> {
        let row: PlantingRow = sqlx::query_as(concat!("select ", planting_cols!(), " where p.id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound("planting"))?;
        row.with_place()
    }

    /// Plantings not yet finished or failed, with their location's name.
    pub async fn list_active_plantings(&self) -> Result<Vec<(Planting, String)>> {
        let rows: Vec<PlantingRow> = sqlx::query_as(concat!(
            "select ",
            planting_cols!(),
            " where p.status in ('planned', 'growing', 'harvesting') order by p.sown_on desc, p.crop"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(PlantingRow::with_place).collect()
    }

    /// Sets a planting's status. Finishing or failing it records today as
    /// its end (unless one is already recorded); bringing it back clears it.
    /// A planned planting that goes in the ground is announced then.
    pub async fn set_planting_status(&self, id: Uuid, status: PlantingStatus) -> Result<()> {
        let mut tx = self.begin().await?;
        let was: String = sqlx::query_scalar("select status from homestead.plantings where id = $1 for update")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StoreError::NotFound("planting"))?;
        sqlx::query(
            "update homestead.plantings set status = $2,
               ended_on = case when $3 then greatest(sown_on, coalesce(ended_on, (now() at time zone $4)::date))
                          end
             where id = $1",
        )
        .bind(id)
        .bind(status.as_str())
        .bind(status.is_over())
        .bind(&*self.timezone)
        .execute(&mut *tx)
        .await?;
        let planted = matches!(status, PlantingStatus::Growing | PlantingStatus::Harvesting);
        if was == PlantingStatus::Planned.as_str() && planted {
            let event = planting_created(&mut tx, id).await?;
            outbox::emit(&mut tx, &event).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn log_harvest(&self, input: NewHarvest) -> Result<Harvest> {
        let h = input.validated()?;
        let mut tx = self.begin().await?;
        let (crop, location_name): (String, String) = sqlx::query_as(
            "select p.crop, l.name from homestead.plantings p
             join homestead.locations l on l.id = p.location_id where p.id = $1",
        )
        .bind(h.planting_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::NotFound("planting"))?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.harvests (planting_id, harvested_on, quantity, unit, notes)
             values ($1, $2, $3, $4, $5) returning id",
        )
        .bind(h.planting_id)
        .bind(h.harvested_on)
        .bind(h.quantity)
        .bind(h.unit.as_str())
        .bind(&h.notes)
        .fetch_one(&mut *tx)
        .await?;
        // First harvest moves a growing planting into its harvesting phase.
        sqlx::query("update homestead.plantings set status = 'harvesting' where id = $1 and status = 'growing'")
            .bind(h.planting_id)
            .execute(&mut *tx)
            .await?;
        let harvest = Harvest {
            id,
            planting_id: h.planting_id,
            harvested_on: h.harvested_on,
            quantity: h.quantity,
            unit: h.unit,
            notes: h.notes,
        };
        outbox::emit(&mut tx, &Event::HarvestLogged { harvest: harvest.clone(), crop, location_name }).await?;
        tx.commit().await?;
        Ok(harvest)
    }

    /// Total harvested per (crop, unit) since `since`. Units are never summed
    /// across each other: 3 lb + 2 bunch is not 5 of anything.
    pub async fn harvest_totals_since(&self, since: Date) -> Result<Vec<(String, String, Decimal)>> {
        Ok(sqlx::query_as(
            "select p.crop, h.unit, sum(h.quantity)
             from homestead.harvests h join homestead.plantings p on p.id = h.planting_id
             where h.harvested_on >= $1 group by p.crop, h.unit order by p.crop, h.unit",
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await?)
    }

    // ── Tasks ───────────────────────────────────────────────────────────────

    pub async fn create_task(&self, input: NewTask) -> Result<Task> {
        let mut tx = self.begin().await?;
        let task = insert_task(&mut tx, input).await?;
        tx.commit().await?;
        Ok(task)
    }

    pub async fn get_task(&self, id: Uuid) -> Result<Task> {
        let row: TaskRow = sqlx::query_as(concat!("select ", task_cols!(), " from homestead.tasks where id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound("task"))?;
        row.try_into()
    }

    /// Open tasks, most urgent first, then soonest due (undated last).
    pub async fn list_open_tasks(&self) -> Result<Vec<Task>> {
        let rows: Vec<TaskRow> = sqlx::query_as(concat!(
            "select ",
            task_cols!(),
            " from homestead.tasks where status = 'open'
             order by array_position(array['urgent','high','normal','low'], priority), due_on nulls last, created_at"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Gives an open task to someone, or back to anyone (`None`). The
    /// database refuses service accounts and disabled people. A closed task
    /// keeps whoever did it.
    pub async fn assign_task(&self, id: Uuid, to: Option<Uuid>) -> Result<Task> {
        let mut tx = self.begin().await?;
        let row: Option<TaskRow> = sqlx::query_as(concat!(
            "update homestead.tasks set assigned_to = $2 where id = $1 and status = 'open' returning ",
            task_cols!()
        ))
        .bind(id)
        .bind(to)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            drop(tx);
            self.get_task(id).await?;
            return Err(StoreError::Conflict("that task is already closed".into()));
        };
        tx.commit().await?;
        row.try_into()
    }

    /// The people a task can be given to: active human accounts, by name.
    pub async fn assignable_people(&self) -> Result<Vec<(Uuid, String)>> {
        Ok(sqlx::query_as(
            "select id, display_name from homestead.actors
             where kind = 'human' and disabled_at is null order by lower(display_name)",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Everyone's display name, disabled people included, so a task done
    /// by someone who has since left still says who.
    pub async fn person_names(&self) -> Result<std::collections::HashMap<Uuid, String>> {
        let rows: Vec<(Uuid, String)> =
            sqlx::query_as("select id, display_name from homestead.actors where kind = 'human'")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().collect())
    }

    /// Marks an open task done. Completing an already-done task is a no-op
    /// that emits nothing, so double-clicks and chat retries don't re-announce.
    pub async fn complete_task(&self, id: Uuid) -> Result<Task> {
        let mut tx = self.begin().await?;
        let Some(task) = close_task(&mut tx, id, &self.timezone).await? else {
            drop(tx);
            return self.get_task(id).await; // NotFound, or already closed.
        };
        tx.commit().await?;
        Ok(task)
    }

    /// Records the Linear issue mirroring a task. Only the first write wins,
    /// so a redelivered `task.created` can detect it already mirrored.
    pub async fn set_linear_issue_id(&self, task_id: Uuid, issue_id: &str) -> Result<bool> {
        let mut tx = self.begin().await?;
        let n =
            sqlx::query("update homestead.tasks set linear_issue_id = $2 where id = $1 and linear_issue_id is null")
                .bind(task_id)
                .bind(issue_id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        tx.commit().await?;
        Ok(n == 1)
    }

    /// Records the Linear reorder issue raised for a low-stock event. Only
    /// the first write wins, like [`Store::set_linear_issue_id`].
    pub async fn set_low_stock_issue(&self, event_id: i64, item_id: Uuid, issue_id: &str) -> Result<bool> {
        let mut tx = self.begin().await?;
        let n = sqlx::query(
            "insert into homestead.low_stock_issues (event_id, item_id, linear_issue_id) values ($1, $2, $3)
             on conflict (event_id) do nothing",
        )
        .bind(event_id)
        .bind(item_id)
        .bind(issue_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(n == 1)
    }

    /// The Linear issue recorded for a low-stock event, if any.
    pub async fn low_stock_issue(&self, event_id: i64) -> Result<Option<String>> {
        Ok(sqlx::query_scalar("select linear_issue_id from homestead.low_stock_issues where event_id = $1")
            .bind(event_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    // ── Inventory ───────────────────────────────────────────────────────────

    pub async fn create_inventory_item(&self, input: NewInventoryItem) -> Result<InventoryItem> {
        let i = input.validated()?;
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.inventory_items (name, unit, reorder_level) values ($1, $2, $3) returning id",
        )
        .bind(&i.name)
        .bind(&i.unit)
        .bind(i.reorder_level)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(InventoryItem { id, name: i.name, unit: i.unit, reorder_level: i.reorder_level, on_hand: Decimal::ZERO })
    }

    pub async fn list_inventory(&self) -> Result<Vec<InventoryItem>> {
        let rows: Vec<(Uuid, String, String, Decimal, Decimal)> = sqlx::query_as(
            "select id, name, unit, reorder_level, on_hand from homestead.inventory_items order by name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, name, unit, reorder_level, on_hand)| InventoryItem { id, name, unit, reorder_level, on_hand })
            .collect())
    }
}

#[derive(sqlx::FromRow)]
struct TaskRow {
    id: Uuid,
    title: String,
    details: String,
    location_id: Option<Uuid>,
    due_on: Option<Date>,
    priority: String,
    status: String,
    mirror_to_linear: bool,
    linear_issue_id: Option<String>,
    completed_at: Option<OffsetDateTime>,
    template_id: Option<Uuid>,
    assigned_to: Option<Uuid>,
}

impl TryFrom<TaskRow> for Task {
    type Error = StoreError;
    fn try_from(r: TaskRow) -> Result<Task> {
        Ok(Task {
            id: r.id,
            title: r.title,
            details: r.details,
            location_id: r.location_id,
            due_on: r.due_on,
            priority: parse(&r.priority)?,
            status: parse(&r.status)?,
            mirror_to_linear: r.mirror_to_linear,
            linear_issue_id: r.linear_issue_id,
            completed_at: r.completed_at,
            template_id: r.template_id,
            assigned_to: r.assigned_to,
        })
    }
}

/// Appends events inside the caller's transaction. Exposed for `outbox`.
pub(crate) type Tx<'a> = Transaction<'a, Postgres>;

#[cfg(test)]
mod tests {
    /// Every store module, so the attribution lint below sees new ones.
    const SOURCES: &[(&str, &str)] = &[
        ("lib.rs", include_str!("lib.rs")),
        ("adoptions.rs", include_str!("adoptions.rs")),
        ("animals.rs", include_str!("animals.rs")),
        ("attachments.rs", include_str!("attachments.rs")),
        ("auth.rs", include_str!("auth.rs")),
        ("community.rs", include_str!("community.rs")),
        ("contacts.rs", include_str!("contacts.rs")),
        ("crops.rs", include_str!("crops.rs")),
        ("equipment.rs", include_str!("equipment.rs")),
        ("deadlines.rs", include_str!("deadlines.rs")),
        ("digest.rs", include_str!("digest.rs")),
        ("field.rs", include_str!("field.rs")),
        ("gifts.rs", include_str!("gifts.rs")),
        ("grants.rs", include_str!("grants.rs")),
        ("harvests.rs", include_str!("harvests.rs")),
        ("health.rs", include_str!("health.rs")),
        ("incidents.rs", include_str!("incidents.rs")),
        ("inventory.rs", include_str!("inventory.rs")),
        ("linear.rs", include_str!("linear.rs")),
        ("map.rs", include_str!("map.rs")),
        ("movements.rs", include_str!("movements.rs")),
        ("notify.rs", include_str!("notify.rs")),
        ("ops.rs", include_str!("ops.rs")),
        ("places.rs", include_str!("places.rs")),
        ("readings.rs", include_str!("readings.rs")),
        ("recurring.rs", include_str!("recurring.rs")),
        ("releases.rs", include_str!("releases.rs")),
        ("reports.rs", include_str!("reports.rs")),
        ("search.rs", include_str!("search.rs")),
        ("seeds.rs", include_str!("seeds.rs")),
        ("soil.rs", include_str!("soil.rs")),
        ("stand.rs", include_str!("stand.rs")),
        ("tags.rs", include_str!("tags.rs")),
        ("water.rs", include_str!("water.rs")),
    ];

    /// Bookkeeping that is nobody's action: the idempotency ledger.
    const UNATTRIBUTED: &[&str] = &["insert into homestead.idempotency", "update homestead.idempotency"];

    /// Whether lower-cased SQL changes rows. A CTE is a write only when one
    /// of its parts is; a recursive read (`with recursive up as (select …)`)
    /// is not.
    fn is_write(sql: &str) -> bool {
        ["insert ", "update ", "delete "].iter().any(|w| sql.starts_with(w))
            || (sql.starts_with("with ") && ["insert into", "update ", "delete from"].iter().any(|w| sql.contains(w)))
    }

    #[test]
    fn a_cte_counts_as_a_write_only_when_it_writes() {
        assert!(is_write("insert into homestead.tasks (title) values ($1)"));
        assert!(is_write("with moved as (delete from homestead.x returning *) select * from moved"));
        assert!(is_write("with n as (select 1) update homestead.tasks set title = $1"));
        assert!(!is_write("with recursive up as (select l.* from homestead.locations l) select * from up"));
        assert!(!is_write("select * from homestead.tasks"));
    }

    /// A write executed straight on the pool never sees the transaction-
    /// local actor settings, so the audit log would say "system" for
    /// something a person did. Writes must run on a `self.begin()`
    /// transaction. (`outbox.rs` is the dispatcher's own bookkeeping.)
    #[test]
    fn writes_are_attributed() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let on_disk: Vec<String> = std::fs::read_dir(dir)
            .expect("src")
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(".rs") && n != "outbox.rs")
            .collect();
        for name in &on_disk {
            assert!(SOURCES.iter().any(|(n, _)| n == name), "add {name} to SOURCES");
        }
        let mut bad = Vec::new();
        for (name, src) in SOURCES {
            let src = &src[..src.find("#[cfg(test)]").unwrap_or(src.len())];
            let lines = src.lines().filter(|l| l.contains("self.pool.begin()")).count();
            let allowed = usize::from(*name == "lib.rs"); // inside `begin` itself
            if lines != allowed {
                bad.push(format!("{name}: use self.begin(), not self.pool.begin()"));
            }
            for (i, _) in src.match_indices("sqlx::query") {
                let rest = &src[i..];
                let stmt = &rest[..rest.find(".await").unwrap_or(rest.len())];
                let Some(q) = stmt.find('"') else { continue };
                let sql = stmt[q + 1..].trim_start().to_lowercase();
                let writes = is_write(&sql);
                if writes && stmt.contains("&self.pool") && !UNATTRIBUTED.iter().any(|u| sql.starts_with(u)) {
                    let line = src[..i].lines().count() + 1;
                    bad.push(format!("{name}:{line}: write on &self.pool: {}", &sql[..sql.len().min(40)]));
                }
            }
        }
        assert!(bad.is_empty(), "unattributed writes:\n{}", bad.join("\n"));
    }
}

//! A place's history: what grew there (or anywhere inside it), what came
//! out of it, and the work done there.

use homestead_domain::{Location, NewLocation, Planting};
use rust_decimal::Decimal;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::{LocationRow, PlantingRow, Result, Store, StoreError, location_rows, planting_cols};

/// Everything recorded against one place and the places inside it.
pub struct LocationHistory {
    /// The places it sits in, outermost first.
    pub ancestors: Vec<Location>,
    pub children: Vec<Location>,
    /// Plantings here or inside, newest first, with the place each is in.
    pub plantings: Vec<(Planting, String)>,
    pub harvests: Vec<HarvestLine>,
    pub tasks: Vec<(Uuid, String, Option<Date>, String)>,
    pub irrigation: Vec<(OffsetDateTime, Option<Decimal>, String, String)>,
    pub waterings: i64,
}

/// One harvest, named by its crop and where it grew.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct HarvestLine {
    pub id: Uuid,
    pub planting_id: Uuid,
    pub crop: String,
    pub quantity: Decimal,
    pub unit: String,
    pub harvested_on: Date,
    pub place: String,
}

/// The ids of a place and every place inside it, as a CTE named `place`.
macro_rules! subtree {
    () => {
        "with recursive place as (
           select id from homestead.locations where id = $1
           union all select c.id from homestead.locations c join place on c.parent_id = place.id
         ) "
    };
}

impl Store {
    /// Changes a place's details. Deactivating keeps its history; a parent
    /// that would make a loop is refused by the database.
    pub async fn update_location(&self, id: Uuid, input: NewLocation) -> Result<Location> {
        let l = input.validated()?;
        let mut tx = self.begin().await?;
        let row: LocationRow = sqlx::query_as(
            "update homestead.locations
             set name = $2, kind = $3, notes = $4, parent_id = $5, active = $6, area_sqft = $7, soil_notes = $8
             where id = $1
             returning id, name, kind, notes, parent_id, active, area_sqft, soil_notes, created_at",
        )
        .bind(id)
        .bind(&l.name)
        .bind(l.kind.as_str())
        .bind(&l.notes)
        .bind(l.parent_id)
        .bind(l.active)
        .bind(l.area_sqft)
        .bind(&l.soil_notes)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::NotFound("location"))?;
        tx.commit().await?;
        Ok(location_rows(vec![row])?.pop().expect("one row"))
    }

    /// What has grown, been harvested, tasked and watered here, counting
    /// the places inside it (a bed's rows belong to the bed's history).
    pub async fn location_history(&self, id: Uuid) -> Result<LocationHistory> {
        let _ = self.get_location(id).await?;
        let ancestors = location_rows(
            sqlx::query_as(
                "with recursive up as (
                   select l.*, 0 as depth from homestead.locations l
                     where id = (select parent_id from homestead.locations where id = $1)
                   union all select l.*, up.depth + 1 from homestead.locations l join up on l.id = up.parent_id
                 ) select id, name, kind, notes, parent_id, active, area_sqft, soil_notes, created_at
                   from up order by depth desc",
            )
            .bind(id)
            .fetch_all(&self.pool)
            .await?,
        )?;
        let children = location_rows(
            sqlx::query_as(
                "select id, name, kind, notes, parent_id, active, area_sqft, soil_notes, created_at
                 from homestead.locations where parent_id = $1 order by active desc, name",
            )
            .bind(id)
            .fetch_all(&self.pool)
            .await?,
        )?;
        let plantings: Vec<PlantingRow> = sqlx::query_as(concat!(
            subtree!(),
            "select ",
            planting_cols!(),
            " where p.location_id in (select id from place) order by p.sown_on desc, p.crop"
        ))
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        let plantings = plantings.into_iter().map(PlantingRow::with_place).collect::<Result<_>>()?;
        let harvests = sqlx::query_as(concat!(
            subtree!(),
            "select h.id, h.planting_id, p.crop, h.quantity, h.unit, h.harvested_on, l.name as place
             from homestead.harvests h join homestead.plantings p on p.id = h.planting_id
             join homestead.locations l on l.id = p.location_id
             where p.location_id in (select id from place) order by h.harvested_on desc, p.crop"
        ))
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        let tasks = sqlx::query_as(concat!(
            subtree!(),
            "select id, title, due_on, status from homestead.tasks
             where location_id in (select id from place) order by created_at desc limit 40"
        ))
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        let irrigation = sqlx::query_as(concat!(
            subtree!(),
            "select r.started_at, r.water_liters, r.method, l.name from homestead.irrigation_records r
             join homestead.locations l on l.id = r.location_id
             where r.location_id in (select id from place) order by r.started_at desc limit 10"
        ))
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        let waterings = sqlx::query_scalar(concat!(
            subtree!(),
            "select count(*) from homestead.irrigation_records where location_id in (select id from place)"
        ))
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        Ok(LocationHistory { ancestors, children, plantings, harvests, tasks, irrigation, waterings })
    }
}

//! Weather observations and irrigation runs.

use homestead_domain::water::{Irrigation, NewIrrigation, NewWeather, NewZone, Weather, Zone};
use homestead_domain::{NewTask, TaskPriority, ValidationError};
use rust_decimal::Decimal;
use time::{Date, OffsetDateTime, PrimitiveDateTime};
use uuid::Uuid;

use crate::{Result, Store, StoreError, parse};

type WeatherRow = (
    i64,
    OffsetDateTime,
    String,
    Option<Uuid>,
    Option<String>,
    Option<Decimal>,
    Option<Decimal>,
    Option<Decimal>,
    Option<Decimal>,
    String,
    Option<Decimal>,
    bool,
);

macro_rules! weather_cols {
    () => {
        "w.id, w.observed_at, w.source, w.location_id, l.name, w.temperature_c, w.precipitation_mm, w.wind_kph,
         w.humidity_pct, w.conditions, w.soil_temperature_c, w.frost
         from homestead.weather_records w left join homestead.locations l on l.id = w.location_id"
    };
}

fn weather(r: WeatherRow) -> Weather {
    let (
        id,
        observed_at,
        source,
        location_id,
        location_name,
        temperature_c,
        precipitation_mm,
        wind_kph,
        humidity_pct,
        conditions,
        soil_temperature_c,
        frost,
    ) = r;
    Weather {
        id,
        observed_at,
        source,
        location_id,
        location_name,
        temperature_c,
        precipitation_mm,
        wind_kph,
        humidity_pct,
        conditions,
        soil_temperature_c,
        frost,
    }
}

type IrrigationRow = (
    Uuid,
    Option<Uuid>,
    String,
    Option<Uuid>,
    Option<String>,
    Option<Uuid>,
    Option<String>,
    OffsetDateTime,
    Option<OffsetDateTime>,
    Option<Decimal>,
    String,
    String,
    String,
);

macro_rules! irrigation_cols {
    () => {
        "r.id, r.location_id, coalesce(l.name, z.name, ''), r.zone_id, r.problem, r.planting_id, p.crop, r.started_at,
         r.ended_at, r.water_liters, r.method, r.notes, r.source
         from homestead.irrigation_records r left join homestead.locations l on l.id = r.location_id
         left join homestead.irrigation_zones z on z.id = r.zone_id
         left join homestead.plantings p on p.id = r.planting_id"
    };
}

fn irrigation(r: IrrigationRow) -> Result<Irrigation> {
    let (
        id,
        location_id,
        location_name,
        zone_id,
        problem,
        planting_id,
        planting_crop,
        started_at,
        ended_at,
        water_liters,
        method,
        notes,
        source,
    ) = r;
    Ok(Irrigation {
        id,
        location_id,
        location_name,
        zone_id,
        problem: problem.as_deref().map(parse).transpose()?,
        planting_id,
        planting_crop,
        started_at,
        ended_at,
        water_liters,
        method: parse(&method)?,
        notes,
        source,
    })
}

impl Store {
    /// A wall-clock time on the farm (what a form's `datetime-local` gives)
    /// as an instant.
    pub async fn farm_time(&self, local: PrimitiveDateTime) -> Result<OffsetDateTime> {
        Ok(sqlx::query_scalar("select $1::timestamp at time zone $2")
            .bind(local)
            .bind(&*self.timezone)
            .fetch_one(&self.pool)
            .await?)
    }

    /// Instants as wall-clock times on the farm (exact across DST changes).
    pub async fn farm_times(&self, instants: &[OffsetDateTime]) -> Result<Vec<PrimitiveDateTime>> {
        Ok(sqlx::query_scalar(
            "select t at time zone $2 from unnest($1::timestamptz[]) with ordinality as u(t, n) order by n",
        )
        .bind(instants)
        .bind(&*self.timezone)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Records an observation. The same source reporting the same instant
    /// again (a retried sensor upload) returns the first record unchanged.
    pub async fn record_weather(&self, input: NewWeather) -> Result<Weather> {
        let w = input.validated()?;
        let mut tx = self.begin().await?;
        let id: Option<i64> = sqlx::query_scalar(
            "insert into homestead.weather_records
               (observed_at, source, location_id, temperature_c, precipitation_mm, wind_kph, humidity_pct, conditions, raw,
                soil_temperature_c, frost)
             values ($1, $2, $3, $4, $5, $6, $7, $8, coalesce($9, '{}'::jsonb), $10, $11)
             on conflict (source, observed_at) do nothing returning id",
        )
        .bind(w.observed_at)
        .bind(&w.source)
        .bind(w.location_id)
        .bind(w.temperature_c)
        .bind(w.precipitation_mm)
        .bind(w.wind_kph)
        .bind(w.humidity_pct)
        .bind(&w.conditions)
        .bind(&w.raw)
        .bind(w.soil_temperature_c)
        .bind(w.frost)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| match StoreError::from(e) {
            StoreError::NotFound(_) => StoreError::NotFound("location"),
            other => other,
        })?;
        tx.commit().await?;
        let row: WeatherRow = match id {
            Some(id) => sqlx::query_as(concat!("select ", weather_cols!(), " where w.id = $1")).bind(id),
            None => sqlx::query_as(concat!("select ", weather_cols!(), " where w.source = $1 and w.observed_at = $2"))
                .bind(&w.source)
                .bind(w.observed_at),
        }
        .fetch_one(&self.pool)
        .await?;
        Ok(weather(row))
    }

    pub async fn recent_weather(&self, limit: i64) -> Result<Vec<Weather>> {
        let rows: Vec<WeatherRow> =
            sqlx::query_as(concat!("select ", weather_cols!(), " order by w.observed_at desc limit $1"))
                .bind(limit)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(weather).collect())
    }

    /// Rain since a date, per source: a person's gauge and a station
    /// measure the same rain, so their totals are never added together.
    pub async fn rain_since(&self, since: Date) -> Result<Vec<(String, Decimal)>> {
        Ok(sqlx::query_as(
            "select source, sum(precipitation_mm) from homestead.weather_records
             where precipitation_mm is not null and (observed_at at time zone $2)::date >= $1
             group by source order by source",
        )
        .bind(since)
        .bind(&*self.timezone)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Records a watering. Given a planting, the location is the
    /// planting's; a zone alone waters every place it serves. A machine
    /// report with an id it already sent is returned as first recorded. A
    /// problem found opens a high-priority task to fix it.
    pub async fn record_irrigation(&self, input: NewIrrigation) -> Result<Irrigation> {
        let r = input.validated()?;
        let mut tx = self.begin().await?;
        let location_id = match r.planting_id {
            Some(p) => {
                let at: Uuid = sqlx::query_scalar("select location_id from homestead.plantings where id = $1")
                    .bind(p)
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or(StoreError::NotFound("planting"))?;
                if r.location_id.is_some_and(|l| l != at) {
                    return Err(ValidationError::Malformed("location (that planting is somewhere else)").into());
                }
                Some(at)
            }
            None => r.location_id,
        };
        let id: Option<Uuid> = sqlx::query_scalar(
            "insert into homestead.irrigation_records
               (location_id, planting_id, started_at, ended_at, water_liters, method, notes, source, external_id, zone_id, problem)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
             on conflict (source, external_id) where external_id is not null do nothing returning id",
        )
        .bind(location_id)
        .bind(r.planting_id)
        .bind(r.started_at)
        .bind(r.ended_at)
        .bind(r.water_liters)
        .bind(r.method.as_str())
        .bind(&r.notes)
        .bind(&r.source)
        .bind(&r.external_id)
        .bind(r.zone_id)
        .bind(r.problem.map(|p| p.as_str()))
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| match StoreError::from(e) {
            StoreError::NotFound(_) => StoreError::NotFound("location or zone"),
            other => other,
        })?;
        if let (Some(id), Some(problem)) = (id, r.problem) {
            let place: String = sqlx::query_scalar(
                "select coalesce(l.name, z.name, '') from homestead.irrigation_records r
                 left join homestead.locations l on l.id = r.location_id
                 left join homestead.irrigation_zones z on z.id = r.zone_id where r.id = $1",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            let task = crate::insert_task(
                &mut tx,
                NewTask {
                    title: format!("Fix irrigation: {} at {place}", problem.label()),
                    details: r.notes.clone(),
                    location_id,
                    due_on: None,
                    priority: TaskPriority::High,
                    mirror_to_linear: false,
                    assigned_to: None,
                },
            )
            .await?;
            sqlx::query("update homestead.irrigation_records set problem_task_id = $2 where id = $1")
                .bind(id)
                .bind(task.id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        let row: IrrigationRow = match id {
            Some(id) => sqlx::query_as(concat!("select ", irrigation_cols!(), " where r.id = $1")).bind(id),
            None => {
                sqlx::query_as(concat!("select ", irrigation_cols!(), " where r.source = $1 and r.external_id = $2"))
                    .bind(&r.source)
                    .bind(&r.external_id)
            }
        }
        .fetch_one(&self.pool)
        .await?;
        irrigation(row)
    }

    /// Problems found while watering whose fix is still an open task.
    pub async fn open_irrigation_problems(&self) -> Result<Vec<(Irrigation, Uuid)>> {
        let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "select r.id, t.id from homestead.irrigation_records r join homestead.tasks t on t.id = r.problem_task_id
             where t.status = 'open' order by r.started_at desc",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::new();
        for (id, task) in rows {
            let row: IrrigationRow = sqlx::query_as(concat!("select ", irrigation_cols!(), " where r.id = $1"))
                .bind(id)
                .fetch_one(&self.pool)
                .await?;
            out.push((irrigation(row)?, task));
        }
        Ok(out)
    }

    pub async fn create_zone(&self, input: NewZone) -> Result<Zone> {
        let z = input.validated()?;
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.irrigation_zones (name, controller_ref, notes) values ($1,$2,$3) returning id",
        )
        .bind(&z.name)
        .bind(&z.controller_ref)
        .bind(&z.notes)
        .fetch_one(&mut *tx)
        .await?;
        for l in &z.location_ids {
            sqlx::query("insert into homestead.irrigation_zone_locations (zone_id, location_id) values ($1,$2)")
                .bind(id)
                .bind(l)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        self.get_zone(id).await
    }

    /// Changes a zone's details and the places it serves (replacing them).
    pub async fn update_zone(&self, id: Uuid, input: NewZone, active: bool) -> Result<Zone> {
        let z = input.validated()?;
        let mut tx = self.begin().await?;
        let n = sqlx::query(
            "update homestead.irrigation_zones set name = $2, controller_ref = $3, notes = $4, active = $5 where id = $1",
        )
        .bind(id)
        .bind(&z.name)
        .bind(&z.controller_ref)
        .bind(&z.notes)
        .bind(active)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if n == 0 {
            return Err(StoreError::NotFound("zone"));
        }
        sqlx::query(
            "delete from homestead.irrigation_zone_locations where zone_id = $1 and not (location_id = any($2))",
        )
        .bind(id)
        .bind(&z.location_ids)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "insert into homestead.irrigation_zone_locations (zone_id, location_id)
             select $1, unnest($2::uuid[]) on conflict do nothing",
        )
        .bind(id)
        .bind(&z.location_ids)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.get_zone(id).await
    }

    pub async fn get_zone(&self, id: Uuid) -> Result<Zone> {
        self.zones(Some(id)).await?.pop().ok_or(StoreError::NotFound("zone"))
    }

    pub async fn list_zones(&self) -> Result<Vec<Zone>> {
        self.zones(None).await
    }

    async fn zones(&self, only: Option<Uuid>) -> Result<Vec<Zone>> {
        type Row = (Uuid, String, String, String, bool, Vec<Uuid>, Vec<String>);
        let rows: Vec<Row> = sqlx::query_as(
            "select z.id, z.name, z.controller_ref, z.notes, z.active,
                    coalesce(array_agg(l.id order by l.name) filter (where l.id is not null), '{}'),
                    coalesce(array_agg(l.name order by l.name) filter (where l.id is not null), '{}')
             from homestead.irrigation_zones z
             left join homestead.irrigation_zone_locations zl on zl.zone_id = z.id
             left join homestead.locations l on l.id = zl.location_id
             where $1::uuid is null or z.id = $1
             group by z.id order by z.active desc, z.name",
        )
        .bind(only)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, name, controller_ref, notes, active, ids, names)| Zone {
                id,
                name,
                controller_ref,
                notes,
                active,
                places: ids.into_iter().zip(names).collect(),
            })
            .collect())
    }

    pub async fn recent_irrigation(&self, limit: i64) -> Result<Vec<Irrigation>> {
        let rows: Vec<IrrigationRow> =
            sqlx::query_as(concat!("select ", irrigation_cols!(), " order by r.started_at desc limit $1"))
                .bind(limit)
                .fetch_all(&self.pool)
                .await?;
        rows.into_iter().map(irrigation).collect()
    }

    /// Water put on each location since a date, in litres (runs whose
    /// amount wasn't measured are counted, not guessed).
    pub async fn water_by_location_since(&self, since: Date) -> Result<Vec<(String, Decimal, i64, i64)>> {
        Ok(sqlx::query_as(
            "select coalesce(l.name, z.name, ''), coalesce(sum(r.water_liters), 0), count(*),
                    count(*) filter (where r.water_liters is null)
             from homestead.irrigation_records r left join homestead.locations l on l.id = r.location_id
             left join homestead.irrigation_zones z on z.id = r.zone_id
             where (r.started_at at time zone $2)::date >= $1 group by 1 order by 1",
        )
        .bind(since)
        .bind(&*self.timezone)
        .fetch_all(&self.pool)
        .await?)
    }
}

//! Readings: numbers taken off a place, planting, animal or machine.

use homestead_domain::auth::{Capability, CapabilitySet};
use homestead_domain::readings::{NewReading, Reading, ReadingSubject, ReadingValue};
use rust_decimal::Decimal;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Result, Store, StoreError};

/// Which subjects' readings a caller may see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadingScope {
    pub garden: bool,
    pub animals: bool,
    pub equipment: bool,
}

impl ReadingScope {
    pub fn read(caps: CapabilitySet) -> Self {
        Self {
            garden: caps.has(Capability::GardenRead),
            animals: caps.has(Capability::AnimalsRead),
            equipment: caps.has(Capability::EquipmentRead),
        }
    }

    /// The subjects a caller may take readings of.
    pub fn write(caps: CapabilitySet) -> Self {
        Self {
            garden: caps.has(Capability::GardenWrite),
            animals: caps.has(Capability::AnimalsWrite),
            equipment: caps.has(Capability::EquipmentWrite),
        }
    }

    pub fn any(self) -> bool {
        self.garden || self.animals || self.equipment
    }
}

#[derive(Debug, Clone, Default)]
pub struct ReadingFilter {
    pub kind: Option<String>,
    /// A place and everything inside it, plantings in them included.
    pub within: Option<Uuid>,
    pub subject: Option<ReadingSubject>,
}

macro_rules! reading_cols {
    () => {
        "r.id, r.observed_at, r.kind, r.location_id, r.planting_id, r.animal_id, r.equipment_id, r.notes,
         coalesce(l.name, p.crop || ' in ' || pl.name, a.name, e.name) as subject_name
         from homestead.readings r
         left join homestead.locations l on l.id = r.location_id
         left join homestead.plantings p on p.id = r.planting_id
         left join homestead.locations pl on pl.id = p.location_id
         left join homestead.animals a on a.id = r.animal_id
         left join homestead.equipment e on e.id = r.equipment_id"
    };
}

type ReadingRow =
    (Uuid, OffsetDateTime, String, Option<Uuid>, Option<Uuid>, Option<Uuid>, Option<Uuid>, String, String);

impl Store {
    /// Records a reading and its values in one transaction.
    pub async fn record_reading(&self, input: NewReading) -> Result<Reading> {
        let r = input.validated()?;
        let (location, planting, animal, equipment) = r.subject.ids();
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.readings (observed_at, kind, location_id, planting_id, animal_id, equipment_id, notes)
             values ($1, $2, $3, $4, $5, $6, $7) returning id",
        )
        .bind(r.observed_at)
        .bind(&r.kind)
        .bind(location)
        .bind(planting)
        .bind(animal)
        .bind(equipment)
        .bind(&r.notes)
        .fetch_one(&mut *tx)
        .await?;
        for (i, v) in r.values.iter().enumerate() {
            sqlx::query(
                "insert into homestead.reading_values (reading_id, position, name, value, unit)
                 values ($1, $2, $3, $4, $5)",
            )
            .bind(id)
            .bind(i as i16)
            .bind(&v.name)
            .bind(v.value)
            .bind(&v.unit)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        let row: ReadingRow = sqlx::query_as(concat!("select ", reading_cols!(), " where r.id = $1"))
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        Ok(self.with_values(vec![row]).await?.pop().expect("the reading just written"))
    }

    /// Readings the scope may see, newest first.
    pub async fn readings(&self, f: &ReadingFilter, scope: ReadingScope, limit: i64) -> Result<Vec<Reading>> {
        let (sl, sp, sa, se) = f.subject.map(ReadingSubject::ids).unwrap_or_default();
        let rows: Vec<ReadingRow> = sqlx::query_as(concat!(
            "with recursive place as (
               select id from homestead.locations where id = $2
               union all select c.id from homestead.locations c join place on c.parent_id = place.id
             ) select ",
            reading_cols!(),
            " where ($1::text is null or r.kind = $1)
               and ($2::uuid is null or r.location_id in (select id from place)
                    or p.location_id in (select id from place))
               and (($3 and (r.location_id is not null or r.planting_id is not null))
                    or ($4 and r.animal_id is not null) or ($5 and r.equipment_id is not null))
               and ($6::uuid is null or r.location_id = $6) and ($7::uuid is null or r.planting_id = $7)
               and ($8::uuid is null or r.animal_id = $8) and ($9::uuid is null or r.equipment_id = $9)
             order by r.observed_at desc, r.created_at desc limit $10"
        ))
        .bind(f.kind.as_deref())
        .bind(f.within)
        .bind(scope.garden)
        .bind(scope.animals)
        .bind(scope.equipment)
        .bind(sl)
        .bind(sp)
        .bind(sa)
        .bind(se)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        self.with_values(rows).await
    }

    /// Every kind recorded so far, for suggestions.
    pub async fn reading_kinds(&self) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar("select distinct kind from homestead.readings order by kind")
            .fetch_all(&self.pool)
            .await?)
    }

    /// What the scope may take readings of, still in use: places, plantings
    /// in the ground, animals still here, equipment not retired.
    pub async fn reading_subjects(&self, scope: ReadingScope) -> Result<Vec<(ReadingSubject, String)>> {
        let rows: Vec<(String, Uuid, String)> = sqlx::query_as(
            "select 'location', id, name from homestead.locations where $1 and active
             union all select 'planting', p.id, p.crop || ' in ' || l.name from homestead.plantings p
               join homestead.locations l on l.id = p.location_id where $1 and p.status in ('growing', 'harvesting')
             union all select 'animal', id, name from homestead.animals
               where $2 and status in ('resident', 'in_care', 'fostered', 'adoptable')
             union all select 'equipment', id, name from homestead.equipment where $3 and status <> 'retired'
             order by 1, 3",
        )
        .bind(scope.garden)
        .bind(scope.animals)
        .bind(scope.equipment)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(kind, id, name)| {
                let s = format!("{kind}:{id}")
                    .parse()
                    .map_err(|e: homestead_domain::ValidationError| StoreError::Corrupt(e.to_string()))?;
                Ok((s, name))
            })
            .collect()
    }

    async fn with_values(&self, rows: Vec<ReadingRow>) -> Result<Vec<Reading>> {
        let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
        let values: Vec<(Uuid, String, Decimal, String)> = sqlx::query_as(
            "select reading_id, name, value, unit from homestead.reading_values
             where reading_id = any($1) order by reading_id, position",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(id, observed_at, kind, l, p, a, e, notes, subject_name)| {
                let subject = ReadingSubject::from_ids(l, p, a, e)
                    .ok_or_else(|| StoreError::Corrupt(format!("reading {id} measures more than one thing")))?;
                let values = values
                    .iter()
                    .filter(|v| v.0 == id)
                    .map(|(_, name, value, unit)| ReadingValue {
                        name: name.clone(),
                        value: *value,
                        unit: unit.clone(),
                    })
                    .collect();
                Ok(Reading { id, observed_at, kind, subject, subject_name, values, notes })
            })
            .collect()
    }
}

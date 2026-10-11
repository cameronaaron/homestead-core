//! Soil tests and amendments, and a place's soil history.

use homestead_domain::soil::{Amendment, Measurements, NewAmendment, NewSoilTest, SoilEntry, SoilTest, soil_history};
use rust_decimal::Decimal;
use sqlx::types::Json;
use time::Date;
use uuid::Uuid;

use crate::{Result, Store, StoreError, parse};

macro_rules! test_cols {
    () => {
        "t.id, t.location_id, t.tested_on, t.ph, t.nitrogen, t.phosphorus, t.potassium, t.organic_matter, t.lab,
         t.measurements, t.notes, l.name
         from homestead.soil_tests t join homestead.locations l on l.id = t.location_id"
    };
}

macro_rules! amendment_cols {
    () => {
        "a.id, a.location_id, a.applied_on, a.kind, a.material, a.amount, a.unit, a.reason, a.cost, a.notes, l.name
         from homestead.soil_amendments a join homestead.locations l on l.id = a.location_id"
    };
}

type TestRow =
    (Uuid, Uuid, Date, Option<Decimal>, String, String, String, String, String, Json<Measurements>, String, String);
type AmendmentRow =
    (Uuid, Uuid, Date, String, String, Option<Decimal>, String, String, Option<Decimal>, String, String);

fn test(r: TestRow) -> (SoilTest, String) {
    let (id, location_id, tested_on, ph, nitrogen, phosphorus, potassium, organic_matter, lab, m, notes, place) = r;
    let t = SoilTest {
        id,
        location_id,
        tested_on,
        ph,
        nitrogen,
        phosphorus,
        potassium,
        organic_matter,
        lab,
        measurements: m.0,
        notes,
    };
    (t, place)
}

fn amendment(r: AmendmentRow) -> Result<(Amendment, String)> {
    let (id, location_id, applied_on, kind, material, amount, unit, reason, cost, notes, place) = r;
    let a = Amendment { id, location_id, applied_on, kind: parse(&kind)?, material, amount, unit, reason, cost, notes };
    Ok((a, place))
}

/// A place and everything inside it, as a CTE named `place`.
macro_rules! within {
    () => {
        "with recursive place as (
           select id from homestead.locations where id = $1
           union all select c.id from homestead.locations c join place on c.parent_id = place.id
         ) select "
    };
}

impl Store {
    pub async fn add_soil_test(&self, input: NewSoilTest) -> Result<SoilTest> {
        let t = input.validated()?;
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.soil_tests (location_id, tested_on, ph, nitrogen, phosphorus, potassium,
               organic_matter, lab, measurements, notes)
             values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) returning id",
        )
        .bind(t.location_id)
        .bind(t.tested_on)
        .bind(t.ph)
        .bind(&t.nitrogen)
        .bind(&t.phosphorus)
        .bind(&t.potassium)
        .bind(&t.organic_matter)
        .bind(&t.lab)
        .bind(Json(&t.measurements))
        .bind(&t.notes)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(SoilTest {
            id,
            location_id: t.location_id,
            tested_on: t.tested_on,
            ph: t.ph,
            nitrogen: t.nitrogen,
            phosphorus: t.phosphorus,
            potassium: t.potassium,
            organic_matter: t.organic_matter,
            lab: t.lab,
            measurements: t.measurements,
            notes: t.notes,
        })
    }

    /// A soil test and the name of the place it was taken in.
    pub async fn get_soil_test(&self, id: Uuid) -> Result<(SoilTest, String)> {
        let row: TestRow = sqlx::query_as(concat!("select ", test_cols!(), " where t.id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound("soil test"))?;
        Ok(test(row))
    }

    pub async fn add_amendment(&self, input: NewAmendment) -> Result<Amendment> {
        let a = input.validated()?;
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.soil_amendments
               (location_id, applied_on, kind, material, amount, unit, reason, cost, notes)
             values ($1,$2,$3,$4,$5,$6,$7,$8,$9) returning id",
        )
        .bind(a.location_id)
        .bind(a.applied_on)
        .bind(a.kind.as_str())
        .bind(&a.material)
        .bind(a.amount)
        .bind(&a.unit)
        .bind(&a.reason)
        .bind(a.cost)
        .bind(&a.notes)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Amendment {
            id,
            location_id: a.location_id,
            applied_on: a.applied_on,
            kind: a.kind,
            material: a.material,
            amount: a.amount,
            unit: a.unit,
            reason: a.reason,
            cost: a.cost,
            notes: a.notes,
        })
    }

    /// Every test and amendment in a place and the places inside it, newest
    /// first, each with the name of the place it was in.
    pub async fn soil_history(&self, location_id: Uuid) -> Result<Vec<(SoilEntry, String)>> {
        let tests: Vec<TestRow> =
            sqlx::query_as(concat!(within!(), test_cols!(), " where t.location_id in (select id from place)"))
                .bind(location_id)
                .fetch_all(&self.pool)
                .await?;
        let amendments: Vec<AmendmentRow> =
            sqlx::query_as(concat!(within!(), amendment_cols!(), " where a.location_id in (select id from place)"))
                .bind(location_id)
                .fetch_all(&self.pool)
                .await?;
        let amendments = amendments.into_iter().map(amendment).collect::<Result<_>>()?;
        Ok(soil_history(tests.into_iter().map(test).collect(), amendments))
    }
}

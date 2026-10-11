//! Seed lots and the plantings sown from them.

use homestead_domain::Planting;
use homestead_domain::seeds::{NewSeedLot, SeedLot};
use rust_decimal::Decimal;
use time::Date;
use uuid::Uuid;

use crate::{PlantingRow, Result, Store, StoreError, Tx, parse, planting_cols};

macro_rules! lot_cols {
    () => {
        "s.id, s.crop, s.variety, s.supplier, s.supplier_lot, s.vendor_id, v.name, s.acquired_on, s.packed_for_year,
         s.organic, s.quantity, s.unit,
         coalesce((select sum(p.seed_quantity_used) from homestead.plantings p where p.seed_lot_id = s.id), 0),
         (select count(*) from homestead.plantings p where p.seed_lot_id = s.id),
         s.notes
         from homestead.seed_lots s left join homestead.vendors v on v.id = s.vendor_id"
    };
}

type Row = (
    Uuid,
    String,
    String,
    String,
    String,
    Option<Uuid>,
    Option<String>,
    Option<Date>,
    Option<i32>,
    Option<bool>,
    Option<Decimal>,
    Option<String>,
    Decimal,
    i64,
    String,
);

fn lot(r: Row) -> Result<SeedLot> {
    let (
        id,
        crop,
        variety,
        supplier,
        supplier_lot,
        vendor_id,
        vendor_name,
        acquired_on,
        packed_for_year,
        organic,
        quantity,
        unit,
        used,
        plantings,
        notes,
    ) = r;
    Ok(SeedLot {
        id,
        crop,
        variety,
        supplier,
        supplier_lot,
        vendor_id,
        vendor_name,
        acquired_on,
        packed_for_year,
        organic,
        quantity,
        unit: unit.as_deref().map(parse).transpose()?,
        used,
        plantings,
        notes,
    })
}

/// Checks a planting's seed use against what's left in the (locked) lot.
pub(crate) async fn take_seed(tx: &mut Tx<'_>, lot_id: Uuid, used: Option<Decimal>) -> Result<()> {
    let row: Option<(Option<Decimal>, Decimal)> = sqlx::query_as(
        "select s.quantity, coalesce((select sum(p.seed_quantity_used) from homestead.plantings p
                                      where p.seed_lot_id = s.id), 0)
         from homestead.seed_lots s where s.id = $1 for update of s",
    )
    .bind(lot_id)
    .fetch_optional(&mut **tx)
    .await?;
    let (quantity, already) = row.ok_or(StoreError::NotFound("seed lot"))?;
    if let (Some(q), Some(u)) = (quantity, used)
        && already + u > q
    {
        return Err(StoreError::Conflict(format!("that lot only has {} left", (q - already).normalize())));
    }
    Ok(())
}

impl Store {
    pub async fn create_seed_lot(&self, input: NewSeedLot) -> Result<SeedLot> {
        let s = input.validated()?;
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.seed_lots
               (crop, variety, supplier, supplier_lot, vendor_id, acquired_on, packed_for_year, organic, quantity, unit,
                notes)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) returning id",
        )
        .bind(&s.crop)
        .bind(&s.variety)
        .bind(&s.supplier)
        .bind(&s.supplier_lot)
        .bind(s.vendor_id)
        .bind(s.acquired_on)
        .bind(s.packed_for_year)
        .bind(s.organic)
        .bind(s.quantity)
        .bind(s.unit.map(|u| u.as_str()))
        .bind(&s.notes)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match StoreError::from(e) {
            StoreError::NotFound(_) => StoreError::NotFound("vendor"),
            other => other,
        })?;
        tx.commit().await?;
        self.get_seed_lot(id).await
    }

    pub async fn get_seed_lot(&self, id: Uuid) -> Result<SeedLot> {
        let row: Option<Row> = sqlx::query_as(concat!("select ", lot_cols!(), " where s.id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        lot(row.ok_or(StoreError::NotFound("seed lot"))?)
    }

    pub async fn list_seed_lots(&self) -> Result<Vec<SeedLot>> {
        let rows: Vec<Row> = sqlx::query_as(concat!(
            "select ",
            lot_cols!(),
            " order by s.crop, s.variety, s.acquired_on desc nulls last"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(lot).collect()
    }

    /// Every planting sown from a lot, with where it is and how much seed.
    pub async fn plantings_from_lot(&self, lot_id: Uuid) -> Result<Vec<(Planting, String, Option<Decimal>)>> {
        let rows: Vec<PlantingRow> =
            sqlx::query_as(concat!("select ", planting_cols!(), " where p.seed_lot_id = $1 order by p.sown_on desc"))
                .bind(lot_id)
                .fetch_all(&self.pool)
                .await?;
        rows.into_iter()
            .map(|r| {
                let used = r.seed_quantity_used;
                let (p, place) = r.with_place()?;
                Ok((p, place, used))
            })
            .collect()
    }
}

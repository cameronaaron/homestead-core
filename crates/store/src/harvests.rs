//! A harvest's trace: what it grew from, and where every part of it went.

use homestead_domain::crops::CropPlan;
use homestead_domain::harvests::{Disposition, DispositionKind, NewDisposition, NewProduceDonation, ProduceDonation};
use homestead_domain::seeds::SeedLot;
use homestead_domain::{Harvest, Planting};
use rust_decimal::Decimal;
use time::Date;
use uuid::Uuid;

use crate::places::HarvestLine;
use crate::{Result, Store, StoreError, Tx, parse};

/// A harvest, where it came from, and where it went.
pub struct HarvestTrace {
    pub harvest: Harvest,
    pub planting: Planting,
    pub place: String,
    pub plan: Option<CropPlan>,
    pub seed_lot: Option<SeedLot>,
    pub dispositions: Vec<Disposition>,
}

macro_rules! disposition_cols {
    () => {
        "d.id, d.harvest_id, d.kind, d.quantity, d.unit, d.notes, d.stand_sale_id, g.id, g.recipient
         from homestead.harvest_dispositions d left join homestead.produce_donations g on g.disposition_id = d.id"
    };
}

type DispositionRow = (Uuid, Uuid, String, Decimal, String, String, Option<Uuid>, Option<Uuid>, Option<String>);

fn disposition(r: DispositionRow) -> Result<Disposition> {
    let (id, harvest_id, kind, quantity, unit, notes, stand_sale_id, donation_id, recipient) = r;
    Ok(Disposition {
        id,
        harvest_id,
        kind: parse(&kind)?,
        quantity,
        unit: parse(&unit)?,
        notes,
        stand_sale_id,
        donation: donation_id.zip(recipient),
    })
}

macro_rules! donation_cols {
    () => {
        "g.id, g.donated_on, g.recipient, g.product, g.quantity, g.unit, g.operational_value, g.notes, d.harvest_id
         from homestead.produce_donations g left join homestead.harvest_dispositions d on d.id = g.disposition_id"
    };
}

type DonationRow = (Uuid, Date, String, String, Decimal, String, Option<Decimal>, String, Option<Uuid>);

fn donation(r: DonationRow) -> Result<ProduceDonation> {
    let (id, donated_on, recipient, product, quantity, unit, operational_value, notes, harvest_id) = r;
    Ok(ProduceDonation {
        id,
        donated_on,
        recipient,
        product,
        quantity,
        unit: parse(&unit)?,
        operational_value,
        notes,
        harvest_id,
    })
}

macro_rules! harvest_line_cols {
    () => {
        "select h.id, h.planting_id, p.crop, h.quantity, h.unit, h.harvested_on, l.name as place
         from homestead.harvests h join homestead.plantings p on p.id = h.planting_id
         join homestead.locations l on l.id = p.location_id "
    };
}

impl Store {
    pub async fn get_harvest(&self, id: Uuid) -> Result<HarvestTrace> {
        let (planting_id, harvested_on, quantity, unit, notes): (Uuid, Date, Decimal, String, String) = sqlx::query_as(
            "select planting_id, harvested_on, quantity, unit, notes from homestead.harvests where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound("harvest"))?;
        let harvest = Harvest { id, planting_id, harvested_on, quantity, unit: parse(&unit)?, notes };
        let (planting, place) = self.get_planting(planting_id).await?;
        let plan = match planting.crop_plan_id {
            Some(p) => Some(self.get_crop_plan(p).await?.0),
            None => None,
        };
        let seed_lot = match planting.seed_lot_id {
            Some(l) => Some(self.get_seed_lot(l).await?),
            None => None,
        };
        let rows: Vec<DispositionRow> =
            sqlx::query_as(concat!("select ", disposition_cols!(), " where d.harvest_id = $1 order by d.created_at"))
                .bind(id)
                .fetch_all(&self.pool)
                .await?;
        let dispositions = rows.into_iter().map(disposition).collect::<Result<_>>()?;
        Ok(HarvestTrace { harvest, planting, place, plan, seed_lot, dispositions })
    }

    /// Sends part of a harvest somewhere, in the harvest's unit. A donated
    /// part is recorded as a produce donation of that crop in the same step.
    pub async fn split_harvest(&self, harvest_id: Uuid, input: NewDisposition) -> Result<Disposition> {
        let d = input.validated()?;
        let mut tx = self.begin().await?;
        let id = insert_disposition(&mut tx, harvest_id, d.kind, d.quantity, &d.notes, None).await?;
        if d.kind == DispositionKind::Donation {
            let (crop, variety, harvested_on, unit): (String, String, Date, String) = sqlx::query_as(
                "select p.crop, p.variety, h.harvested_on, h.unit from homestead.harvests h
                 join homestead.plantings p on p.id = h.planting_id where h.id = $1",
            )
            .bind(harvest_id)
            .fetch_one(&mut *tx)
            .await?;
            let product = if variety.is_empty() { crop } else { format!("{crop} ({variety})") };
            sqlx::query(
                "insert into homestead.produce_donations
                   (donated_on, recipient, product, quantity, unit, disposition_id, operational_value, notes)
                 values ($1,$2,$3,$4,$5,$6,$7,$8)",
            )
            .bind(d.donated_on.unwrap_or(harvested_on))
            .bind(&d.recipient)
            .bind(product)
            .bind(d.quantity)
            .bind(unit)
            .bind(id)
            .bind(d.operational_value)
            .bind(&d.notes)
            .execute(&mut *tx)
            .await?;
        }
        let row: DispositionRow = sqlx::query_as(concat!("select ", disposition_cols!(), " where d.id = $1"))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        disposition(row)
    }

    /// Undoes a mistaken disposition, with its donation record. A farm-stand
    /// sale it came from stays: that money was still taken.
    pub async fn remove_disposition(&self, id: Uuid) -> Result<Uuid> {
        let mut tx = self.begin().await?;
        sqlx::query("delete from homestead.produce_donations where disposition_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let harvest: Uuid =
            sqlx::query_scalar("delete from homestead.harvest_dispositions where id = $1 returning harvest_id")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::NotFound("disposition"))?;
        tx.commit().await?;
        Ok(harvest)
    }

    /// Produce given away outside a harvest split (eggs, or a harvest
    /// never weighed).
    pub async fn donate_produce(&self, input: NewProduceDonation) -> Result<ProduceDonation> {
        let g = input.validated()?;
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.produce_donations
               (donated_on, recipient, product, quantity, unit, operational_value, notes)
             values ($1,$2,$3,$4,$5,$6,$7) returning id",
        )
        .bind(g.donated_on)
        .bind(&g.recipient)
        .bind(&g.product)
        .bind(g.quantity)
        .bind(g.unit.as_str())
        .bind(g.operational_value)
        .bind(&g.notes)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(ProduceDonation {
            id,
            donated_on: g.donated_on,
            recipient: g.recipient,
            product: g.product,
            quantity: g.quantity,
            unit: g.unit,
            operational_value: g.operational_value,
            notes: g.notes,
            harvest_id: None,
        })
    }

    /// Every produce donation given in `[from, to]`, newest first.
    pub async fn donations_between(&self, from: Date, to: Date) -> Result<Vec<ProduceDonation>> {
        let rows: Vec<DonationRow> = sqlx::query_as(concat!(
            "select ",
            donation_cols!(),
            " where g.donated_on between $1 and $2 order by g.donated_on desc, g.created_at desc"
        ))
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(donation).collect()
    }

    /// Harvests from plantings sown from a seed lot.
    pub async fn harvests_from_lot(&self, lot_id: Uuid) -> Result<Vec<HarvestLine>> {
        Ok(sqlx::query_as(concat!(harvest_line_cols!(), "where p.seed_lot_id = $1 order by h.harvested_on desc"))
            .bind(lot_id)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn harvests_of_planting(&self, planting_id: Uuid) -> Result<Vec<HarvestLine>> {
        Ok(sqlx::query_as(concat!(harvest_line_cols!(), "where h.planting_id = $1 order by h.harvested_on desc"))
            .bind(planting_id)
            .fetch_all(&self.pool)
            .await?)
    }

    /// Recent harvests, for picking one to split or sell from.
    pub async fn recent_harvests(&self, limit: i64) -> Result<Vec<HarvestLine>> {
        Ok(sqlx::query_as(concat!(harvest_line_cols!(), "order by h.harvested_on desc, h.created_at desc limit $1"))
            .bind(limit)
            .fetch_all(&self.pool)
            .await?)
    }
}

/// Inserts a disposition in the harvest's own unit; the database refuses one
/// that would take more than was picked.
pub(crate) async fn insert_disposition(
    tx: &mut Tx<'_>,
    harvest_id: Uuid,
    kind: DispositionKind,
    quantity: Decimal,
    notes: &str,
    stand_sale_id: Option<Uuid>,
) -> Result<Uuid> {
    let unit: String = sqlx::query_scalar("select unit from homestead.harvests where id = $1")
        .bind(harvest_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(StoreError::NotFound("harvest"))?;
    Ok(sqlx::query_scalar(
        "insert into homestead.harvest_dispositions (harvest_id, kind, quantity, unit, notes, stand_sale_id)
         values ($1,$2,$3,$4,$5,$6) returning id",
    )
    .bind(harvest_id)
    .bind(kind.as_str())
    .bind(quantity)
    .bind(unit)
    .bind(notes)
    .bind(stand_sale_id)
    .fetch_one(&mut **tx)
    .await?)
}

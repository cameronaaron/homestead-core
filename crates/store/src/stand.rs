//! Farm stand sales and compost drop-offs.

use homestead_domain::stand::{CompostDropoff, ItemTotal, NewCompostDropoff, NewStandSale, StandSale};
use rust_decimal::Decimal;
use time::Date;
use uuid::Uuid;

use homestead_domain::ValidationError;
use homestead_domain::harvests::DispositionKind;

use crate::{Result, Store, StoreError, parse};

type SaleRow = (Uuid, Date, String, Decimal, String, Decimal, String, String);

impl Store {
    pub async fn record_sale(&self, input: NewStandSale) -> Result<StandSale> {
        let s = input.validated()?;
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.stand_sales (sold_on, item, quantity, unit, amount, payment, notes)
             values ($1, $2, $3, $4, $5, $6, $7) returning id",
        )
        .bind(s.sold_on)
        .bind(&s.item)
        .bind(s.quantity)
        .bind(s.unit.as_str())
        .bind(s.amount)
        .bind(s.payment.as_str())
        .bind(&s.notes)
        .fetch_one(&mut *tx)
        .await?;
        // Counted against its harvest as that harvest's sale, in the
        // harvest's own unit: a bag of greens can't come out of pounds.
        if let Some(harvest) = s.harvest_id {
            let unit: String = sqlx::query_scalar("select unit from homestead.harvests where id = $1")
                .bind(harvest)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::NotFound("harvest"))?;
            if s.unit.as_harvest_unit().map(|u| u.as_str()) != Some(unit.as_str()) {
                return Err(ValidationError::Malformed(
                    "unit (sold in a different unit from the harvest it came from)",
                )
                .into());
            }
            crate::harvests::insert_disposition(&mut tx, harvest, DispositionKind::Sale, s.quantity, "", Some(id))
                .await?;
        }
        tx.commit().await?;
        Ok(StandSale {
            id,
            sold_on: s.sold_on,
            item: s.item,
            quantity: s.quantity,
            unit: s.unit,
            amount: s.amount,
            payment: s.payment,
            notes: s.notes,
        })
    }

    pub async fn recent_sales(&self, limit: i64) -> Result<Vec<StandSale>> {
        let rows: Vec<SaleRow> = sqlx::query_as(
            "select id, sold_on, item, quantity, unit, amount, payment, notes from homestead.stand_sales
             order by sold_on desc, created_at desc limit $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(id, sold_on, item, quantity, unit, amount, payment, notes)| {
                Ok(StandSale {
                    id,
                    sold_on,
                    item,
                    quantity,
                    unit: parse(&unit)?,
                    amount,
                    payment: parse(&payment)?,
                    notes,
                })
            })
            .collect()
    }

    /// Quantity and takings per item and unit, `from..=to`. Units are never
    /// summed across each other (a dozen and an each stay separate).
    pub async fn sales_totals(&self, from: Date, to: Date) -> Result<Vec<ItemTotal>> {
        let rows: Vec<(String, String, Decimal, Decimal)> = sqlx::query_as(
            "select item, unit, sum(quantity), sum(amount) from homestead.stand_sales
             where sold_on between $1 and $2 group by item, unit order by sum(amount) desc, item",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(item, unit, quantity, amount)| Ok(ItemTotal { item, unit: parse(&unit)?, quantity, amount }))
            .collect()
    }

    pub async fn record_compost_dropoff(&self, input: NewCompostDropoff) -> Result<CompostDropoff> {
        let c = input.validated()?;
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.compost_dropoffs (received_on, contributor, weight_lb, notes)
             values ($1, $2, $3, $4) returning id",
        )
        .bind(c.received_on)
        .bind(&c.contributor)
        .bind(c.weight_lb)
        .bind(&c.notes)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(CompostDropoff {
            id,
            received_on: c.received_on,
            contributor: c.contributor,
            weight_lb: c.weight_lb,
            notes: c.notes,
        })
    }

    /// Drop-offs and weighed pounds `from..=to`. Unweighed drop-offs count
    /// toward the number but not the pounds.
    pub async fn compost_totals(&self, from: Date, to: Date) -> Result<(i64, Decimal)> {
        Ok(sqlx::query_as(
            "select count(*), coalesce(sum(weight_lb), 0) from homestead.compost_dropoffs
             where received_on between $1 and $2",
        )
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await?)
    }
}

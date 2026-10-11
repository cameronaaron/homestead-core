//! Gifts outside Zeffy and whether each has been acknowledged.

use homestead_domain::gifts::{Gift, NewGift};
use rust_decimal::Decimal;
use time::Date;
use uuid::Uuid;

use crate::{Result, Store, StoreError, parse};

macro_rules! gift_cols {
    () => {
        "id, donor_name, donor_email, kind, received_on, amount, description, estimated_value,
         goods_or_services, goods_value, fund, acknowledged_on, notes"
    };
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    donor_name: String,
    donor_email: String,
    kind: String,
    received_on: Date,
    amount: Option<Decimal>,
    description: String,
    estimated_value: Option<Decimal>,
    goods_or_services: String,
    goods_value: Option<Decimal>,
    fund: String,
    acknowledged_on: Option<Date>,
    notes: String,
}

impl TryFrom<Row> for Gift {
    type Error = StoreError;
    fn try_from(r: Row) -> Result<Self> {
        Ok(Gift {
            id: r.id,
            donor_name: r.donor_name,
            donor_email: r.donor_email,
            kind: parse(&r.kind)?,
            received_on: r.received_on,
            amount: r.amount,
            description: r.description,
            estimated_value: r.estimated_value,
            goods_or_services: r.goods_or_services,
            goods_value: r.goods_value,
            fund: r.fund,
            acknowledged_on: r.acknowledged_on,
            notes: r.notes,
        })
    }
}

impl Store {
    pub async fn record_gift(&self, input: NewGift) -> Result<Gift> {
        let g = input.validated()?;
        let mut tx = self.begin().await?;
        let row: Row = sqlx::query_as(concat!(
            "insert into homestead.gifts
               (donor_name, donor_email, kind, received_on, amount, description, estimated_value,
                goods_or_services, goods_value, fund, notes)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) returning ",
            gift_cols!()
        ))
        .bind(&g.donor_name)
        .bind(&g.donor_email)
        .bind(g.kind.as_str())
        .bind(g.received_on)
        .bind(g.amount)
        .bind(&g.description)
        .bind(g.estimated_value)
        .bind(&g.goods_or_services)
        .bind(g.goods_value)
        .bind(&g.fund)
        .bind(&g.notes)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        row.try_into()
    }

    pub async fn get_gift(&self, id: Uuid) -> Result<Gift> {
        let row: Option<Row> = sqlx::query_as(concat!("select ", gift_cols!(), " from homestead.gifts where id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.ok_or(StoreError::NotFound("gift"))?.try_into()
    }

    /// Newest first.
    pub async fn list_gifts(&self) -> Result<Vec<Gift>> {
        let rows: Vec<Row> = sqlx::query_as(concat!(
            "select ",
            gift_cols!(),
            " from homestead.gifts order by received_on desc, created_at desc"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Records that the letter went out. Only the first date counts, so a
    /// double click can't move it.
    pub async fn mark_gift_acknowledged(&self, id: Uuid, on: Date) -> Result<()> {
        let mut tx = self.begin().await?;
        let n = sqlx::query("update homestead.gifts set acknowledged_on = coalesce(acknowledged_on, $2) where id = $1")
            .bind(id)
            .bind(on)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        if n == 0 { Err(StoreError::NotFound("gift")) } else { Ok(()) }
    }
}

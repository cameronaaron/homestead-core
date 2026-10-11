//! The stock ledger, vendors and purchases.
//!
//! Code here only ever *inserts* movements. The database turns each one
//! into per-location balances and the item's total (`apply_stock_movement`)
//! and refuses any other change to those numbers, so "what we have" is
//! always exactly the sum of "what happened".

use homestead_domain::inventory::{
    Balance, Movement, MovementKind, NewMovement, NewPurchase, NewPurchaseLine, NewVendor, PACE_WINDOW_DAYS, Pace,
    Purchase, PurchaseLine, Transfer, Vendor,
};
use homestead_domain::{Event, InventoryItem, StockAdjustment, ValidationError};
use rust_decimal::Decimal;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::{Result, Store, StoreError, Tx, outbox, parse};

/// One movement to append.
struct Entry<'a> {
    kind: MovementKind,
    delta: Decimal,
    location_id: Option<Uuid>,
    note: &'a str,
    transfer_id: Option<Uuid>,
    purchase_line_id: Option<Uuid>,
}

async fn lock_item(tx: &mut Tx<'_>, item_id: Uuid) -> Result<InventoryItem> {
    let row: Option<(String, String, Decimal, Decimal)> = sqlx::query_as(
        "select name, unit, reorder_level, on_hand from homestead.inventory_items where id = $1 for update",
    )
    .bind(item_id)
    .fetch_optional(&mut **tx)
    .await?;
    let (name, unit, reorder_level, on_hand) = row.ok_or(StoreError::NotFound("inventory item"))?;
    Ok(InventoryItem { id: item_id, name, unit, reorder_level, on_hand })
}

/// Appends movements for one (locked) item and announces a low-stock
/// crossing: only on the change that takes it from above its reorder level
/// to at-or-below, never again while it stays low.
async fn append(tx: &mut Tx<'_>, before: &InventoryItem, entries: &[Entry<'_>]) -> Result<InventoryItem> {
    for e in entries {
        sqlx::query(
            "insert into homestead.stock_movements
               (item_id, delta, reason, kind, location_id, transfer_id, purchase_line_id)
             values ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(before.id)
        .bind(e.delta)
        .bind(if e.note.is_empty() { e.kind.label() } else { e.note })
        .bind(e.kind.as_str())
        .bind(e.location_id)
        .bind(e.transfer_id)
        .bind(e.purchase_line_id)
        .execute(&mut **tx)
        .await?;
    }
    let on_hand: Decimal = sqlx::query_scalar("select on_hand from homestead.inventory_items where id = $1")
        .bind(before.id)
        .fetch_one(&mut **tx)
        .await?;
    let after = InventoryItem { on_hand, ..before.clone() };
    if after.is_low() && !before.is_low() {
        outbox::emit(tx, &Event::LowStock { item: after.clone(), previous_on_hand: before.on_hand }).await?;
    }
    Ok(after)
}

/// Where stock leaves from when nobody said: the one place that has any.
async fn only_stocked_place(tx: &mut Tx<'_>, item_id: Uuid) -> Result<Option<Uuid>> {
    let places: Vec<(Option<Uuid>,)> =
        sqlx::query_as("select location_id from homestead.inventory_balances where item_id = $1 and on_hand > 0")
            .bind(item_id)
            .fetch_all(&mut **tx)
            .await?;
    match places.as_slice() {
        [] => Ok(None), // nothing anywhere: the CHECK will refuse it
        [(only,)] => Ok(*only),
        _ => Err(ValidationError::Blank("location (this item is kept in more than one place)").into()),
    }
}

type MovementRow = (
    i64,
    Uuid,
    String,
    Decimal,
    Option<Uuid>,
    Option<String>,
    String,
    Option<Uuid>,
    Option<Uuid>,
    String,
    OffsetDateTime,
);

type PurchaseRow = (Uuid, Option<Uuid>, Option<String>, Date, Decimal, String, String, String, Option<String>);

fn purchase(r: PurchaseRow) -> Result<Purchase> {
    let (id, vendor_id, vendor_name, purchased_on, amount, currency, category, description, external_reference) = r;
    Ok(Purchase {
        id,
        vendor_id,
        vendor_name,
        purchased_on,
        amount,
        currency: parse(&currency)?,
        category,
        description,
        external_reference,
    })
}

macro_rules! purchase_cols {
    () => {
        "p.id, p.vendor_id, v.name, p.purchased_on, p.amount, p.currency, p.category, p.description, p.external_reference
         from homestead.purchases p left join homestead.vendors v on v.id = p.vendor_id"
    };
}

macro_rules! line_cols {
    () => {
        "l.id, l.purchase_id, l.item_id, i.name, i.unit, l.description, l.quantity, l.unit_cost, l.received_at,
         l.received_location_id
         from homestead.purchase_lines l left join homestead.inventory_items i on i.id = l.item_id"
    };
}

type LineRow = (
    Uuid,
    Uuid,
    Option<Uuid>,
    Option<String>,
    Option<String>,
    String,
    Decimal,
    Option<Decimal>,
    Option<OffsetDateTime>,
    Option<Uuid>,
);

fn line(r: LineRow) -> PurchaseLine {
    let (
        id,
        purchase_id,
        item_id,
        item_name,
        unit,
        description,
        quantity,
        unit_cost,
        received_at,
        received_location_id,
    ) = r;
    PurchaseLine {
        id,
        purchase_id,
        item_id,
        item_name,
        unit,
        description,
        quantity,
        unit_cost,
        received_at,
        received_location_id,
    }
}

impl Store {
    /// A count correction or legacy adjustment (`/inventory/{id}/adjust`).
    pub async fn adjust_stock(&self, item_id: Uuid, input: StockAdjustment) -> Result<InventoryItem> {
        let adj = input.validated()?;
        self.record_movement(
            item_id,
            NewMovement { kind: MovementKind::Adjustment, quantity: adj.delta, location_id: None, note: adj.reason },
        )
        .await
    }

    /// Each item's pace over the last [`PACE_WINDOW_DAYS`]: what was used or
    /// wasted, over the days of history it has in that window. Items with
    /// no movements at all are absent.
    pub async fn usage_paces(&self) -> Result<std::collections::HashMap<Uuid, Pace>> {
        let rows: Vec<(Uuid, Decimal, i64)> = sqlx::query_as(
            "select item_id,
                    coalesce(-sum(delta) filter (where kind in ('consumption', 'waste')
                                                   and at > now() - make_interval(days => $1)), 0),
                    greatest(1, least($1, round(extract(epoch from now() - min(at)) / 86400)))::bigint
             from homestead.stock_movements group by item_id",
        )
        .bind(i32::try_from(PACE_WINDOW_DAYS).expect("small"))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(id, used, over_days)| (id, Pace { used, over_days })).collect())
    }

    /// Records use, waste, harvest-in or a count correction.
    pub async fn record_movement(&self, item_id: Uuid, input: NewMovement) -> Result<InventoryItem> {
        let (m, delta) = input.validated()?;
        let mut tx = self.begin().await?;
        let before = lock_item(&mut tx, item_id).await?;
        let location_id = match m.location_id {
            None if delta.is_sign_negative() => only_stocked_place(&mut tx, item_id).await?,
            given => given,
        };
        let entry =
            Entry { kind: m.kind, delta, location_id, note: &m.note, transfer_id: None, purchase_line_id: None };
        let after = append(&mut tx, &before, &[entry]).await?;
        tx.commit().await?;
        Ok(after)
    }

    /// Moves stock between places; the total doesn't change.
    pub async fn transfer_stock(&self, item_id: Uuid, input: Transfer) -> Result<InventoryItem> {
        let t = input.validated()?;
        let mut tx = self.begin().await?;
        let before = lock_item(&mut tx, item_id).await?;
        let id = Some(Uuid::new_v4());
        let entries = [
            Entry {
                kind: MovementKind::TransferOut,
                delta: -t.quantity,
                location_id: t.from_location_id,
                note: &t.note,
                transfer_id: id,
                purchase_line_id: None,
            },
            Entry {
                kind: MovementKind::TransferIn,
                delta: t.quantity,
                location_id: t.to_location_id,
                note: &t.note,
                transfer_id: id,
                purchase_line_id: None,
            },
        ];
        let after = append(&mut tx, &before, &entries).await?;
        tx.commit().await?;
        Ok(after)
    }

    pub async fn get_inventory_item(&self, id: Uuid) -> Result<InventoryItem> {
        let row: Option<(String, String, Decimal, Decimal)> =
            sqlx::query_as("select name, unit, reorder_level, on_hand from homestead.inventory_items where id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        let (name, unit, reorder_level, on_hand) = row.ok_or(StoreError::NotFound("inventory item"))?;
        Ok(InventoryItem { id, name, unit, reorder_level, on_hand })
    }

    /// Where an item is, largest first.
    pub async fn item_balances(&self, item_id: Uuid) -> Result<Vec<Balance>> {
        let rows: Vec<(Option<Uuid>, Option<String>, Decimal)> = sqlx::query_as(
            "select b.location_id, l.name, b.on_hand from homestead.inventory_balances b
             left join homestead.locations l on l.id = b.location_id
             where b.item_id = $1 and b.on_hand <> 0 order by b.on_hand desc, l.name",
        )
        .bind(item_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(location_id, location_name, on_hand)| Balance { location_id, location_name, on_hand })
            .collect())
    }

    /// An item's ledger, newest first, each line with who recorded it.
    pub async fn item_movements(&self, item_id: Uuid, limit: i64) -> Result<Vec<Movement>> {
        let rows: Vec<MovementRow> = sqlx::query_as(
            "select m.id, m.item_id, m.kind, m.delta, m.location_id, l.name, m.reason, m.purchase_line_id,
                    m.transfer_id,
                    coalesce((select a.actor_label from homestead.audit_entries a
                              where a.resource_type = 'stock_movements' and a.resource_id = m.id::text
                                and a.action = 'insert' limit 1), 'unknown'),
                    m.at
             from homestead.stock_movements m left join homestead.locations l on l.id = m.location_id
             where m.item_id = $1 order by m.id desc limit $2",
        )
        .bind(item_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(
                |(
                    id,
                    item_id,
                    kind,
                    delta,
                    location_id,
                    location_name,
                    note,
                    purchase_line_id,
                    transfer_id,
                    by,
                    at,
                )| {
                    Ok(Movement {
                        id,
                        item_id,
                        kind: parse(&kind)?,
                        delta,
                        location_id,
                        location_name,
                        note,
                        purchase_line_id,
                        transfer_id,
                        recorded_by: by,
                        at,
                    })
                },
            )
            .collect()
    }

    // ── Vendors ────────────────────────────────────────────────────────────

    pub async fn create_vendor(&self, input: NewVendor) -> Result<Vendor> {
        let v = input.validated()?;
        let contact = serde_json::json!({ "email": v.email, "phone": v.phone, "website": v.website });
        let mut tx = self.begin().await?;
        let id: Uuid =
            sqlx::query_scalar("insert into homestead.vendors (name, contact, notes) values ($1, $2, $3) returning id")
                .bind(&v.name)
                .bind(contact)
                .bind(&v.notes)
                .fetch_one(&mut *tx)
                .await?;
        tx.commit().await?;
        Ok(Vendor { id, name: v.name, email: v.email, phone: v.phone, website: v.website, notes: v.notes })
    }

    pub async fn list_vendors(&self) -> Result<Vec<Vendor>> {
        let rows: Vec<(Uuid, String, serde_json::Value, String)> =
            sqlx::query_as("select id, name, contact, notes from homestead.vendors order by name")
                .fetch_all(&self.pool)
                .await?;
        let field = |c: &serde_json::Value, k: &str| c.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_owned();
        Ok(rows
            .into_iter()
            .map(|(id, name, c, notes)| Vendor {
                id,
                name,
                email: field(&c, "email"),
                phone: field(&c, "phone"),
                website: field(&c, "website"),
                notes,
            })
            .collect())
    }

    // ── Purchases ──────────────────────────────────────────────────────────

    pub async fn create_purchase(&self, input: NewPurchase) -> Result<Purchase> {
        let p = input.validated()?;
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.purchases
               (vendor_id, purchased_on, amount, currency, category, description, external_reference)
             values ($1, $2, $3, $4, $5, $6, $7) returning id",
        )
        .bind(p.vendor_id)
        .bind(p.purchased_on)
        .bind(p.amount)
        .bind(p.currency.as_str())
        .bind(&p.category)
        .bind(&p.description)
        .bind(&p.external_reference)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match StoreError::from(e) {
            StoreError::NotFound(_) => StoreError::NotFound("vendor"),
            other => other,
        })?;
        tx.commit().await?;
        self.get_purchase(id).await
    }

    pub async fn get_purchase(&self, id: Uuid) -> Result<Purchase> {
        let row: Option<PurchaseRow> = sqlx::query_as(concat!("select ", purchase_cols!(), " where p.id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        purchase(row.ok_or(StoreError::NotFound("purchase"))?)
    }

    pub async fn list_purchases(&self, limit: i64) -> Result<Vec<Purchase>> {
        let rows: Vec<PurchaseRow> = sqlx::query_as(concat!(
            "select ",
            purchase_cols!(),
            " order by p.purchased_on desc, p.created_at desc limit $1"
        ))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(purchase).collect()
    }

    pub async fn add_purchase_line(&self, purchase_id: Uuid, input: NewPurchaseLine) -> Result<PurchaseLine> {
        let l = input.validated()?;
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.purchase_lines (purchase_id, item_id, description, quantity, unit_cost)
             values ($1, $2, $3, $4, $5) returning id",
        )
        .bind(purchase_id)
        .bind(l.item_id)
        .bind(&l.description)
        .bind(l.quantity)
        .bind(l.unit_cost)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match StoreError::from(e) {
            StoreError::NotFound(_) => StoreError::NotFound("purchase or inventory item"),
            other => other,
        })?;
        tx.commit().await?;
        self.get_purchase_line(id).await
    }

    pub async fn get_purchase_line(&self, id: Uuid) -> Result<PurchaseLine> {
        let row: Option<LineRow> = sqlx::query_as(concat!("select ", line_cols!(), " where l.id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(line(row.ok_or(StoreError::NotFound("purchase line"))?))
    }

    pub async fn purchase_lines(&self, purchase_id: Uuid) -> Result<Vec<PurchaseLine>> {
        let rows: Vec<LineRow> =
            sqlx::query_as(concat!("select ", line_cols!(), " where l.purchase_id = $1 order by l.created_at, l.id"))
                .bind(purchase_id)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(line).collect())
    }

    /// Puts a purchase line into stock. Receiving the same line again
    /// changes nothing and returns it as it is, so a double-click or a
    /// retried API call can't count a delivery twice.
    pub async fn receive_purchase_line(&self, line_id: Uuid, location_id: Option<Uuid>) -> Result<PurchaseLine> {
        let mut tx = self.begin().await?;
        let row: Option<(Option<Uuid>, Decimal, Option<OffsetDateTime>)> = sqlx::query_as(
            "select item_id, quantity, received_at from homestead.purchase_lines where id = $1 for update",
        )
        .bind(line_id)
        .fetch_optional(&mut *tx)
        .await?;
        let (item_id, quantity, received_at) = row.ok_or(StoreError::NotFound("purchase line"))?;
        let Some(item_id) = item_id else {
            return Err(StoreError::Conflict(
                "this line isn't an inventory item, so there's nothing to receive".into(),
            ));
        };
        if received_at.is_none() {
            let before = lock_item(&mut tx, item_id).await?;
            sqlx::query(
                "update homestead.purchase_lines set received_at = now(), received_location_id = $2 where id = $1",
            )
            .bind(line_id)
            .bind(location_id)
            .execute(&mut *tx)
            .await?;
            let entry = Entry {
                kind: MovementKind::Receipt,
                delta: quantity,
                location_id,
                note: "",
                transfer_id: None,
                purchase_line_id: Some(line_id),
            };
            append(&mut tx, &before, &[entry]).await?;
        }
        tx.commit().await?;
        self.get_purchase_line(line_id).await
    }

    /// Where an item came from: every purchase line for it, newest first.
    pub async fn item_purchase_history(&self, item_id: Uuid) -> Result<Vec<(Purchase, PurchaseLine)>> {
        let lines: Vec<LineRow> = sqlx::query_as(concat!(
            "select ",
            line_cols!(),
            " join homestead.purchases p on p.id = l.purchase_id where l.item_id = $1
              order by p.purchased_on desc, l.created_at desc limit 50"
        ))
        .bind(item_id)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(lines.len());
        for l in lines.into_iter().map(line) {
            out.push((self.get_purchase(l.purchase_id).await?, l));
        }
        Ok(out)
    }
}

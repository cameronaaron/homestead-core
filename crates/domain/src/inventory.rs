//! Stock as a ledger, and where it came from.
//!
//! Every change to what we have is a [`MovementKind`] row in
//! `stock_movements`, and those rows are never edited. What's on hand (per
//! location, and in total) is kept by the database from that ledger, in the
//! same transaction, so the two can't disagree. Purchases record what was
//! bought from whom; receiving a purchase line is itself a movement.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::{ValidationError, non_blank};

string_enum!(
    /// Why stock changed. The sign of a movement follows from its kind.
    MovementKind, "kind" {
    Receipt => "receipt", Consumption => "consumption", Harvest => "harvest", Waste => "waste",
    Adjustment => "adjustment", TransferOut => "transfer_out", TransferIn => "transfer_in",
});

impl MovementKind {
    /// Kinds a person records directly (transfers come in pairs, receipts
    /// from purchases).
    pub const RECORDABLE: &'static [MovementKind] =
        &[MovementKind::Consumption, MovementKind::Waste, MovementKind::Harvest, MovementKind::Adjustment];

    /// +1 for stock coming in, -1 going out, `None` if either (a count).
    pub fn sign(self) -> Option<i8> {
        match self {
            MovementKind::Receipt | MovementKind::Harvest | MovementKind::TransferIn => Some(1),
            MovementKind::Consumption | MovementKind::Waste | MovementKind::TransferOut => Some(-1),
            MovementKind::Adjustment => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MovementKind::Receipt => "received",
            MovementKind::Consumption => "used",
            MovementKind::Harvest => "harvested / made",
            MovementKind::Waste => "wasted / spoiled",
            MovementKind::Adjustment => "count correction",
            MovementKind::TransferOut => "moved out",
            MovementKind::TransferIn => "moved in",
        }
    }
}

/// A movement someone records by hand.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewMovement {
    pub kind: MovementKind,
    /// Always positive, except for an adjustment, which is signed.
    pub quantity: Decimal,
    #[serde(default)]
    pub location_id: Option<Uuid>,
    #[serde(default)]
    pub note: String,
}

impl NewMovement {
    /// The validated movement and its signed delta.
    pub fn validated(self) -> Result<(Self, Decimal), ValidationError> {
        if matches!(self.kind, MovementKind::Receipt | MovementKind::TransferIn | MovementKind::TransferOut) {
            return Err(ValidationError::Malformed(
                "kind (receipts come from purchases; use a transfer to move stock)",
            ));
        }
        if self.quantity.is_zero() {
            return Err(ValidationError::NotPositive("quantity"));
        }
        let delta = match self.kind.sign() {
            Some(_) if self.quantity.is_sign_negative() => return Err(ValidationError::Negative("quantity")),
            Some(s) => self.quantity * Decimal::from(s),
            None => self.quantity,
        };
        if self.kind == MovementKind::Adjustment && self.note.trim().is_empty() {
            return Err(ValidationError::Blank("note (say why the count changed)"));
        }
        Ok((Self { note: self.note.trim().to_owned(), ..self }, delta))
    }
}

/// Moving stock between two places: one `transfer_out`, one `transfer_in`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Transfer {
    pub quantity: Decimal,
    /// `None` is stock not yet put anywhere in particular.
    #[serde(default)]
    pub from_location_id: Option<Uuid>,
    pub to_location_id: Option<Uuid>,
    #[serde(default)]
    pub note: String,
}

impl Transfer {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.quantity <= Decimal::ZERO {
            return Err(ValidationError::NotPositive("quantity"));
        }
        if self.from_location_id == self.to_location_id {
            return Err(ValidationError::Malformed("transfer (from and to are the same place)"));
        }
        Ok(Self { note: self.note.trim().to_owned(), ..self })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Movement {
    pub id: i64,
    pub item_id: Uuid,
    pub kind: MovementKind,
    pub delta: Decimal,
    pub location_id: Option<Uuid>,
    pub location_name: Option<String>,
    pub note: String,
    pub purchase_line_id: Option<Uuid>,
    pub transfer_id: Option<Uuid>,
    /// From the audit log: who recorded it.
    pub recorded_by: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// Stock of one item at one place (`None`: not put anywhere in particular).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Balance {
    pub location_id: Option<Uuid>,
    pub location_name: Option<String>,
    pub on_hand: Decimal,
}

// ── Vendors and purchases ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewVendor {
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub phone: String,
    #[serde(default)]
    pub website: String,
    #[serde(default)]
    pub notes: String,
}

impl NewVendor {
    pub fn validated(self) -> Result<Self, ValidationError> {
        let email = self.email.trim();
        let email = if email.is_empty() { String::new() } else { crate::contacts::normalise_email(email)? };
        Ok(Self {
            name: non_blank("vendor name", &self.name)?,
            email,
            phone: self.phone.trim().to_owned(),
            website: self.website.trim().to_owned(),
            notes: self.notes.trim().to_owned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Vendor {
    pub id: Uuid,
    pub name: String,
    pub email: String,
    pub phone: String,
    pub website: String,
    pub notes: String,
}

string_enum!(Currency, "currency" { Usd => "usd", Cad => "cad" });

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewPurchase {
    #[serde(default)]
    pub vendor_id: Option<Uuid>,
    pub purchased_on: Date,
    /// What was paid in total, from the receipt (tax and delivery included).
    pub amount: Decimal,
    #[serde(default = "usd")]
    pub currency: Currency,
    pub category: String,
    #[serde(default)]
    pub description: String,
    /// Invoice or order number.
    #[serde(default)]
    pub external_reference: Option<String>,
}

fn usd() -> Currency {
    Currency::Usd
}

impl NewPurchase {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.amount.is_sign_negative() {
            return Err(ValidationError::Negative("amount"));
        }
        Ok(Self {
            category: non_blank("category", &self.category)?.to_lowercase(),
            description: self.description.trim().to_owned(),
            external_reference: self.external_reference.map(|r| r.trim().to_owned()).filter(|r| !r.is_empty()),
            ..self
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Purchase {
    pub id: Uuid,
    pub vendor_id: Option<Uuid>,
    pub vendor_name: Option<String>,
    pub purchased_on: Date,
    pub amount: Decimal,
    pub currency: Currency,
    pub category: String,
    pub description: String,
    pub external_reference: Option<String>,
}

/// One thing on a purchase. A line for a tracked item can be received into
/// stock; a line without one (a repair, a delivery fee) is just a record.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewPurchaseLine {
    #[serde(default)]
    pub item_id: Option<Uuid>,
    #[serde(default)]
    pub description: String,
    pub quantity: Decimal,
    #[serde(default)]
    pub unit_cost: Option<Decimal>,
}

impl NewPurchaseLine {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.quantity <= Decimal::ZERO {
            return Err(ValidationError::NotPositive("quantity"));
        }
        if self.unit_cost.is_some_and(|c| c.is_sign_negative()) {
            return Err(ValidationError::Negative("unit cost"));
        }
        let description = self.description.trim().to_owned();
        if self.item_id.is_none() && description.is_empty() {
            return Err(ValidationError::Blank("description (or choose an inventory item)"));
        }
        Ok(Self { description, ..self })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PurchaseLine {
    pub id: Uuid,
    pub purchase_id: Uuid,
    pub item_id: Option<Uuid>,
    pub item_name: Option<String>,
    pub unit: Option<String>,
    pub description: String,
    pub quantity: Decimal,
    pub unit_cost: Option<Decimal>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub received_at: Option<OffsetDateTime>,
    pub received_location_id: Option<Uuid>,
}

/// How fast an item has been going: what was used or wasted, over how many
/// days of history (at most the window, at least one), so an item added
/// three days ago that used three bags is going at one a day, not three a
/// month.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Pace {
    pub used: Decimal,
    pub over_days: i64,
}

/// Days of history a pace looks back over.
pub const PACE_WINDOW_DAYS: i64 = 30;

/// Stock that runs out within this many days is worth a heads-up even
/// above its reorder level.
pub const RUNS_OUT_SOON_DAYS: i64 = 7;

impl Pace {
    /// Whole days `on_hand` lasts at this pace; `None` when nothing's been
    /// used (it lasts as long as it lasts).
    pub fn days_left(self, on_hand: Decimal) -> Option<i64> {
        if self.used <= Decimal::ZERO || self.over_days < 1 {
            return None;
        }
        let days = (on_hand.max(Decimal::ZERO) * Decimal::from(self.over_days) / self.used).floor();
        i64::try_from(days).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_lasts_as_long_as_its_recent_pace_says() {
        let pace = |used: i64, over_days| Pace { used: Decimal::from(used), over_days };
        // 10 bags over 30 days is a third a day: 5 bags last 15 days.
        assert_eq!(pace(10, 30).days_left(Decimal::from(5)), Some(15));
        // Partial days round down: better early than out.
        assert_eq!(pace(3, 2).days_left(Decimal::from(4)), Some(2));
        assert_eq!(pace(0, 30).days_left(Decimal::from(5)), None, "unused stock has no end date");
        assert_eq!(pace(10, 30).days_left(Decimal::ZERO), Some(0));
    }

    fn mv(kind: MovementKind, qty: i64, note: &str) -> NewMovement {
        NewMovement { kind, quantity: Decimal::from(qty), location_id: None, note: note.into() }
    }

    #[test]
    fn a_movement_takes_its_sign_from_its_kind() {
        assert_eq!(mv(MovementKind::Consumption, 3, "").validated().expect("ok").1, Decimal::from(-3));
        assert_eq!(mv(MovementKind::Waste, 1, "").validated().expect("ok").1, Decimal::from(-1));
        assert_eq!(mv(MovementKind::Harvest, 12, "").validated().expect("ok").1, Decimal::from(12));
        assert_eq!(mv(MovementKind::Adjustment, -2, "recount").validated().expect("ok").1, Decimal::from(-2));
        assert!(mv(MovementKind::Consumption, -3, "").validated().is_err(), "sign comes from the kind");
        assert!(mv(MovementKind::Consumption, 0, "").validated().is_err());
        assert!(mv(MovementKind::Adjustment, 2, " ").validated().is_err(), "corrections say why");
        for k in [MovementKind::Receipt, MovementKind::TransferIn, MovementKind::TransferOut] {
            assert!(mv(k, 1, "x").validated().is_err(), "{k} isn't recorded by hand");
        }
        for k in MovementKind::ALL {
            let expected = match k {
                MovementKind::Adjustment => None,
                _ => Some(()),
            };
            assert_eq!(k.sign().map(|_| ()), expected, "{k}");
        }
    }

    #[test]
    fn transfers_go_somewhere_else() {
        let t = |from: Option<Uuid>, to: Option<Uuid>, q: i64| Transfer {
            quantity: Decimal::from(q),
            from_location_id: from,
            to_location_id: to,
            note: String::new(),
        };
        let a = Some(Uuid::from_u128(1));
        assert!(t(None, a, 2).validated().is_ok());
        assert!(t(a, a, 2).validated().is_err());
        assert!(t(None, a, 0).validated().is_err());
    }

    #[test]
    fn purchases_and_lines_are_checked() {
        let p = NewPurchase {
            vendor_id: None,
            purchased_on: time::macros::date!(2026 - 09 - 10),
            amount: Decimal::from(40),
            currency: Currency::Usd,
            category: " Feed ".into(),
            description: String::new(),
            external_reference: Some("  ".into()),
        };
        let ok = p.clone().validated().expect("ok");
        assert_eq!((ok.category.as_str(), ok.external_reference), ("feed", None));
        assert!(NewPurchase { amount: Decimal::from(-1), ..p.clone() }.validated().is_err());
        assert!(NewPurchase { category: " ".into(), ..p }.validated().is_err());

        let line = |item: Option<Uuid>, desc: &str, qty: i64| NewPurchaseLine {
            item_id: item,
            description: desc.into(),
            quantity: Decimal::from(qty),
            unit_cost: None,
        };
        assert!(line(Some(Uuid::nil()), "", 2).validated().is_ok());
        assert!(line(None, "Delivery", 1).validated().is_ok());
        assert!(line(None, " ", 1).validated().is_err());
        assert!(line(Some(Uuid::nil()), "", 0).validated().is_err());
    }

    #[test]
    fn literals_match_the_migration() {
        let sql = include_str!("../../../migrations/0013_inventory_ledger.sql");
        crate::tests::assert_check_list(sql, "kind", MovementKind::ALL, MovementKind::as_str);
        let core = include_str!("../../../migrations/0003_accountability_and_operations.sql");
        crate::tests::assert_check_list(core, "currency", Currency::ALL, Currency::as_str);
    }
}

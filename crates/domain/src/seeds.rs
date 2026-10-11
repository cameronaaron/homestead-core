//! Seed lots: where a planting's seed came from.
//!
//! Enough to answer "which lot produced this bed, what else did that lot
//! go into, who sold it to us, when, and how much is left", and no more:
//! germination trials and viability testing are out of scope.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::Date;
use uuid::Uuid;

use crate::{ValidationError, non_blank};

string_enum!(SeedUnit, "unit" {
    Seeds => "seeds", Grams => "g", Ounces => "oz", Packets => "packet", Pounds => "lb",
});

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewSeedLot {
    pub crop: String,
    #[serde(default)]
    pub variety: String,
    /// Who we got it from, as written on the packet or invoice.
    #[serde(default)]
    pub supplier: String,
    /// The supplier's lot number, if printed.
    #[serde(default)]
    pub supplier_lot: String,
    /// When the supplier is also a vendor we buy from.
    #[serde(default)]
    pub vendor_id: Option<Uuid>,
    #[serde(default)]
    pub acquired_on: Option<Date>,
    #[serde(default)]
    pub packed_for_year: Option<i32>,
    #[serde(default)]
    pub organic: Option<bool>,
    /// How much came in, if anyone counted or weighed it.
    #[serde(default)]
    pub quantity: Option<Decimal>,
    #[serde(default)]
    pub unit: Option<SeedUnit>,
    #[serde(default)]
    pub notes: String,
}

impl NewSeedLot {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.quantity.is_some() != self.unit.is_some() {
            return Err(ValidationError::Malformed("quantity (give both an amount and its unit, or neither)"));
        }
        if self.quantity.is_some_and(|q| q <= Decimal::ZERO) {
            return Err(ValidationError::NotPositive("quantity"));
        }
        if self.packed_for_year.is_some_and(|y| !(1900..=3000).contains(&y)) {
            return Err(ValidationError::Malformed("packed-for year"));
        }
        Ok(Self {
            crop: non_blank("crop", &self.crop)?,
            variety: self.variety.trim().to_owned(),
            supplier: self.supplier.trim().to_owned(),
            supplier_lot: self.supplier_lot.trim().to_owned(),
            notes: self.notes.trim().to_owned(),
            ..self
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeedLot {
    pub id: Uuid,
    pub crop: String,
    pub variety: String,
    pub supplier: String,
    pub supplier_lot: String,
    pub vendor_id: Option<Uuid>,
    pub vendor_name: Option<String>,
    pub acquired_on: Option<Date>,
    pub packed_for_year: Option<i32>,
    pub organic: Option<bool>,
    pub quantity: Option<Decimal>,
    pub unit: Option<SeedUnit>,
    /// Summed from the plantings that used this lot.
    pub used: Decimal,
    pub plantings: i64,
    pub notes: String,
}

impl SeedLot {
    /// What's left, when the lot's size is known.
    pub fn remaining(&self) -> Option<Decimal> {
        self.quantity.map(|q| q - self.used)
    }

    pub fn label(&self) -> String {
        let mut s = self.crop.clone();
        if !self.variety.is_empty() {
            s.push_str(&format!(" '{}'", self.variety));
        }
        if !self.supplier.is_empty() {
            s.push_str(&format!(" · {}", self.supplier));
        }
        if !self.supplier_lot.is_empty() {
            s.push_str(&format!(" lot {}", self.supplier_lot));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lot() -> NewSeedLot {
        NewSeedLot {
            crop: " Tomato ".into(),
            variety: "Sungold".into(),
            supplier: "Baker Creek".into(),
            supplier_lot: "A-2291".into(),
            vendor_id: None,
            acquired_on: None,
            packed_for_year: Some(2026),
            organic: Some(true),
            quantity: Some(Decimal::from(30)),
            unit: Some(SeedUnit::Seeds),
            notes: String::new(),
        }
    }

    #[test]
    fn a_lots_size_needs_its_unit() {
        assert_eq!(lot().validated().expect("ok").crop, "Tomato");
        assert!(NewSeedLot { unit: None, ..lot() }.validated().is_err());
        assert!(NewSeedLot { quantity: None, unit: None, ..lot() }.validated().is_ok(), "size is optional");
        assert!(NewSeedLot { quantity: Some(Decimal::ZERO), ..lot() }.validated().is_err());
        assert!(NewSeedLot { packed_for_year: Some(26), ..lot() }.validated().is_err());
        assert!(NewSeedLot { crop: " ".into(), ..lot() }.validated().is_err());
    }

    #[test]
    fn remaining_is_what_came_in_less_what_was_sown() {
        let l = SeedLot {
            id: Uuid::nil(),
            crop: "Tomato".into(),
            variety: "Sungold".into(),
            supplier: "Baker Creek".into(),
            supplier_lot: "A-2291".into(),
            vendor_id: None,
            vendor_name: None,
            acquired_on: None,
            packed_for_year: None,
            organic: None,
            quantity: Some(Decimal::from(30)),
            unit: Some(SeedUnit::Seeds),
            used: Decimal::from(12),
            plantings: 2,
            notes: String::new(),
        };
        assert_eq!(l.remaining(), Some(Decimal::from(18)));
        assert_eq!(SeedLot { quantity: None, ..l.clone() }.remaining(), None);
        assert_eq!(l.label(), "Tomato 'Sungold' · Baker Creek lot A-2291");
    }

    #[test]
    fn literals_match_the_migration() {
        let sql = include_str!("../../../migrations/0015_seed_lots.sql");
        crate::tests::assert_check_list(sql, "unit", SeedUnit::ALL, SeedUnit::as_str);
    }
}

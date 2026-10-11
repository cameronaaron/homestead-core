//! The farm stand, and compost the neighbours bring us.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::Date;
use uuid::Uuid;

use crate::{ValidationError, non_blank};

string_enum!(SaleUnit, "unit" {
    Dozen => "dozen", Bunch => "bunch", Bag => "bag", Each => "each", Lb => "lb", Packet => "packet",
});

string_enum!(Payment, "payment" {
    Cash => "cash", Card => "card", Venmo => "venmo", Zelle => "zelle", Other => "other",
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StandSale {
    pub id: Uuid,
    pub sold_on: Date,
    pub item: String,
    pub quantity: Decimal,
    pub unit: SaleUnit,
    pub amount: Decimal,
    pub payment: Payment,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewStandSale {
    pub sold_on: Date,
    pub item: String,
    pub quantity: Decimal,
    pub unit: SaleUnit,
    pub amount: Decimal,
    #[serde(default = "cash")]
    pub payment: Payment,
    #[serde(default)]
    pub notes: String,
    /// The harvest this came out of, to count it as that harvest's sale.
    #[serde(default)]
    pub harvest_id: Option<Uuid>,
}

fn cash() -> Payment {
    Payment::Cash
}

impl SaleUnit {
    /// The same unit as a harvest measures it, when there is one: a bag or
    /// a packet has no harvest equivalent, so can't count against one.
    pub fn as_harvest_unit(self) -> Option<crate::HarvestUnit> {
        use crate::HarvestUnit as H;
        match self {
            SaleUnit::Dozen => Some(H::Dozen),
            SaleUnit::Bunch => Some(H::Bunch),
            SaleUnit::Each => Some(H::Each),
            SaleUnit::Lb => Some(H::Lb),
            SaleUnit::Bag | SaleUnit::Packet => None,
        }
    }
}

impl NewStandSale {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.quantity <= Decimal::ZERO {
            return Err(ValidationError::NotPositive("quantity"));
        }
        if self.amount < Decimal::ZERO {
            return Err(ValidationError::Negative("amount"));
        }
        // Item names group the totals, so "Eggs " and "eggs" are one item.
        let item = non_blank("item", &self.item)?;
        let mut chars = item.chars();
        let item = chars
            .next()
            .map(|c| c.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase())
            .unwrap_or_default();
        Ok(Self { item, notes: self.notes.trim().to_owned(), ..self })
    }
}

/// What one item sold for over a period.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemTotal {
    pub item: String,
    pub unit: SaleUnit,
    pub quantity: Decimal,
    pub amount: Decimal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompostDropoff {
    pub id: Uuid,
    pub received_on: Date,
    pub contributor: String,
    pub weight_lb: Option<Decimal>,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewCompostDropoff {
    pub received_on: Date,
    #[serde(default)]
    pub contributor: String,
    #[serde(default)]
    pub weight_lb: Option<Decimal>,
    #[serde(default)]
    pub notes: String,
}

impl NewCompostDropoff {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.weight_lb.is_some_and(|w| w <= Decimal::ZERO) {
            return Err(ValidationError::NotPositive("weight_lb"));
        }
        Ok(Self { contributor: self.contributor.trim().to_owned(), notes: self.notes.trim().to_owned(), ..self })
    }
}

// ── Egg carton labels ───────────────────────────────────────────────────────
//
// From CDFA's California Shell Egg Regulation Quick Reference Guide
// (https://www.cdfa.ca.gov/ahfss/mpes/pdfs/Quick_Reference_Guide.pdf, read
// 2026-09-29): each carton carries the packer's name, address and zip, the
// Julian packing date, a sell-by date, "Keep Refrigerated" or the safe
// handling statement, the UNABBREVIATED size and grade, and "Shell Egg Food
// Safety Compliant", those last three at least 1/4 inch tall. See
// docs/COMPLIANCE.md for the rest (registration, candling, 45 °F).

string_enum!(EggSize, "size" {
    Jumbo => "jumbo", ExtraLarge => "extra_large", Large => "large", Medium => "medium", Small => "small",
    Peewee => "peewee",
});

impl EggSize {
    /// Spelled out, as the label must be.
    pub fn label(self) -> &'static str {
        match self {
            EggSize::Jumbo => "Jumbo",
            EggSize::ExtraLarge => "Extra Large",
            EggSize::Large => "Large",
            EggSize::Medium => "Medium",
            EggSize::Small => "Small",
            EggSize::Peewee => "Peewee",
        }
    }
}

string_enum!(EggGrade, "grade" { AA => "aa", A => "a", B => "b" });

impl EggGrade {
    pub fn label(self) -> &'static str {
        match self {
            EggGrade::AA => "Grade AA",
            EggGrade::A => "Grade A",
            EggGrade::B => "Grade B",
        }
    }
}

/// FDA's required statement for shell eggs not treated to destroy
/// Salmonella (21 CFR 101.17(h)).
pub const SAFE_HANDLING: &str = "SAFE HANDLING INSTRUCTIONS: To prevent illness from bacteria: keep eggs refrigerated, \
     cook eggs until yolks are firm, and cook foods containing eggs thoroughly.";

/// CDFA's wording for eggs from a registered, compliant producer.
pub const SEFS_COMPLIANT: &str = "Shell Egg Food Safety Compliant";

/// The day of the year, as egg cartons print it (1 January is 001).
pub fn julian(d: Date) -> String {
    format!("{:03}", d.ordinal())
}

/// Sell-by is at most this long after packing unless changed. 30 days is
/// USDA's limit for eggs it grades; California's own limit is to confirm.
pub const DEFAULT_SELL_BY_DAYS: i64 = 30;

#[derive(Debug, Clone, PartialEq)]
pub struct EggLabel {
    pub packed_on: Date,
    pub sell_by: Date,
    pub size: EggSize,
    pub grade: EggGrade,
    /// Eggs in the carton.
    pub count: u8,
}

impl EggLabel {
    pub fn new(
        packed_on: Date,
        sell_by_days: i64,
        size: EggSize,
        grade: EggGrade,
        count: u8,
    ) -> Result<Self, ValidationError> {
        if !(1..=45).contains(&sell_by_days) {
            return Err(ValidationError::Malformed("sell by (1 to 45 days after packing)"));
        }
        if !(1..=30).contains(&count) {
            return Err(ValidationError::Malformed("eggs per carton (1 to 30)"));
        }
        let sell_by =
            packed_on.checked_add(time::Duration::days(sell_by_days)).ok_or(ValidationError::Malformed("sell by"))?;
        Ok(Self { packed_on, sell_by, size, grade, count })
    }

    /// "One dozen", "Half dozen", or "18 eggs".
    pub fn quantity(&self) -> String {
        match self.count {
            12 => "One dozen".into(),
            6 => "Half dozen".into(),
            1 => "1 egg".into(),
            n => format!("{n} eggs"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn egg_labels_carry_what_cdfa_asks_for() {
        assert_eq!(julian(date!(2026 - 01 - 01)), "001");
        assert_eq!(julian(date!(2026 - 09 - 29)), "272");
        assert_eq!(julian(date!(2024 - 12 - 31)), "366", "leap year");
        let l = EggLabel::new(date!(2026 - 09 - 29), DEFAULT_SELL_BY_DAYS, EggSize::ExtraLarge, EggGrade::AA, 12)
            .expect("label");
        assert_eq!(l.sell_by, date!(2026 - 10 - 29));
        assert_eq!((l.size.label(), l.grade.label(), l.quantity().as_str()), ("Extra Large", "Grade AA", "One dozen"));
        assert!(EggLabel::new(date!(2026 - 09 - 29), 0, EggSize::Large, EggGrade::A, 12).is_err());
        assert!(EggLabel::new(date!(2026 - 09 - 29), 30, EggSize::Large, EggGrade::A, 0).is_err());
        assert!(SAFE_HANDLING.contains("keep eggs refrigerated, cook eggs until yolks are firm"));
    }

    fn sale(item: &str, qty: i64, amount: i64) -> NewStandSale {
        NewStandSale {
            sold_on: date!(2026 - 09 - 26),
            item: item.into(),
            quantity: Decimal::from(qty),
            unit: SaleUnit::Dozen,
            amount: Decimal::from(amount),
            payment: Payment::Cash,
            notes: String::new(),
            harvest_id: None,
        }
    }

    #[test]
    fn only_units_a_harvest_uses_can_count_against_one() {
        assert_eq!(SaleUnit::Lb.as_harvest_unit(), Some(crate::HarvestUnit::Lb));
        assert_eq!(SaleUnit::Bag.as_harvest_unit(), None);
        for u in SaleUnit::ALL {
            if let Some(h) = u.as_harvest_unit() {
                assert_eq!(h.as_str(), u.as_str(), "same literal, same meaning");
            }
        }
    }

    #[test]
    fn enum_strings_match_migration() {
        let sql = include_str!("../../../migrations/0009_farm_stand.sql");
        crate::tests::assert_check_list(sql, "unit", SaleUnit::ALL, SaleUnit::as_str);
        crate::tests::assert_check_list(sql, "payment", Payment::ALL, Payment::as_str);
    }

    #[test]
    fn item_names_are_normalised_so_totals_group() {
        assert_eq!(sale("  eGGS ", 1, 7).validated().expect("ok").item, "Eggs");
        assert_eq!(sale("Black gold compost", 1, 3).validated().expect("ok").item, "Black gold compost");
    }

    #[test]
    fn quantity_positive_amount_not_negative() {
        assert_eq!(sale("Eggs", 0, 7).validated(), Err(ValidationError::NotPositive("quantity")));
        assert_eq!(sale("Eggs", 1, -1).validated(), Err(ValidationError::Negative("amount")));
        assert!(sale("Eggs", 1, 0).validated().is_ok(), "a gifted dozen still counts as eggs handled");
        assert_eq!(sale(" ", 1, 7).validated(), Err(ValidationError::Blank("item")));
    }

    #[test]
    fn compost_weight_is_optional_but_positive() {
        let c = |w: Option<i64>| NewCompostDropoff {
            received_on: date!(2026 - 09 - 26),
            contributor: String::new(),
            weight_lb: w.map(Decimal::from),
            notes: String::new(),
        };
        assert!(c(None).validated().is_ok());
        assert!(c(Some(0)).validated().is_err());
    }
}

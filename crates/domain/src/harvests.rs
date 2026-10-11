//! Where a harvest went, and produce given away.
//!
//! A harvest is split into dispositions in its own unit: they can't add up
//! to more than was picked, and a unit is never converted (a bunch is not a
//! weight). A donated part is the produce donation itself, so "what did we
//! donate" and "where did the tomatoes go" read the same record.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::Date;
use uuid::Uuid;

use crate::{HarvestUnit, ValidationError, non_blank};

string_enum!(DispositionKind, "kind" {
    Sale => "sale", Donation => "donation", Community => "community", Feed => "feed", Seed => "seed",
    Loss => "loss", Compost => "compost",
});

/// The four questions a harvest's dispositions answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fate {
    Sold,
    Donated,
    /// Eaten or used here: community use, animal feed, saved seed.
    Used,
    /// Lost, spoiled, or composted.
    Lost,
}

impl Fate {
    pub const ALL: [Fate; 4] = [Fate::Sold, Fate::Donated, Fate::Used, Fate::Lost];

    pub fn label(self) -> &'static str {
        match self {
            Fate::Sold => "sold",
            Fate::Donated => "donated",
            Fate::Used => "used here",
            Fate::Lost => "lost or composted",
        }
    }
}

impl DispositionKind {
    pub fn fate(self) -> Fate {
        match self {
            DispositionKind::Sale => Fate::Sold,
            DispositionKind::Donation => Fate::Donated,
            DispositionKind::Community | DispositionKind::Feed | DispositionKind::Seed => Fate::Used,
            DispositionKind::Loss | DispositionKind::Compost => Fate::Lost,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DispositionKind::Sale => "sold",
            DispositionKind::Donation => "donated",
            DispositionKind::Community => "community use",
            DispositionKind::Feed => "animal feed",
            DispositionKind::Seed => "seed saving",
            DispositionKind::Loss => "lost",
            DispositionKind::Compost => "composted",
        }
    }
}

/// Part of a harvest going somewhere, in the harvest's unit.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewDisposition {
    pub kind: DispositionKind,
    pub quantity: Decimal,
    #[serde(default)]
    pub notes: String,
    /// Who received it, for a donation.
    #[serde(default)]
    pub recipient: String,
    /// An operational estimate for a donation; never a tax value.
    #[serde(default)]
    pub operational_value: Option<Decimal>,
    /// When it was given, for a donation (defaults to the harvest day).
    #[serde(default)]
    pub donated_on: Option<Date>,
}

impl NewDisposition {
    pub fn of(kind: DispositionKind, quantity: Decimal) -> Self {
        Self {
            kind,
            quantity,
            notes: String::new(),
            recipient: String::new(),
            operational_value: None,
            donated_on: None,
        }
    }

    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.quantity <= Decimal::ZERO {
            return Err(ValidationError::NotPositive("quantity"));
        }
        let donation = self.kind == DispositionKind::Donation;
        let recipient = if donation { non_blank("recipient", &self.recipient)? } else { String::new() };
        if !donation
            && (!self.recipient.trim().is_empty() || self.operational_value.is_some() || self.donated_on.is_some())
        {
            return Err(ValidationError::Malformed("recipient, value and date (only a donation has them)"));
        }
        if self.operational_value.is_some_and(|v| v < Decimal::ZERO) {
            return Err(ValidationError::Negative("value"));
        }
        Ok(Self { recipient, notes: self.notes.trim().to_owned(), ..self })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Disposition {
    pub id: Uuid,
    pub harvest_id: Uuid,
    pub kind: DispositionKind,
    pub quantity: Decimal,
    pub unit: HarvestUnit,
    pub notes: String,
    /// The farm-stand sale this is, when it came from one.
    pub stand_sale_id: Option<Uuid>,
    /// The donation record this is, with its recipient.
    pub donation: Option<(Uuid, String)>,
}

/// A harvest's parts by fate, each with its share of the harvest.
#[derive(Debug, Clone, PartialEq)]
pub struct Shares {
    pub parts: Vec<(Fate, Decimal, Decimal)>,
    /// Picked but not yet recorded as going anywhere.
    pub unrecorded: Decimal,
}

/// Totals `parts` by fate as percentages of `total` (all in one unit).
pub fn shares(total: Decimal, parts: &[(DispositionKind, Decimal)]) -> Shares {
    let pct = |q: Decimal| if total.is_zero() { Decimal::ZERO } else { (q * Decimal::from(100) / total).round_dp(1) };
    let mut out = Vec::new();
    for fate in Fate::ALL {
        let q: Decimal = parts.iter().filter(|(k, _)| k.fate() == fate).map(|(_, q)| *q).sum();
        if !q.is_zero() {
            out.push((fate, q, pct(q)));
        }
    }
    let recorded: Decimal = parts.iter().map(|(_, q)| *q).sum();
    Shares { parts: out, unrecorded: (total - recorded).max(Decimal::ZERO) }
}

/// Produce given away that didn't come from a recorded harvest (or one
/// given in a unit its harvest wasn't weighed in).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewProduceDonation {
    pub donated_on: Date,
    pub recipient: String,
    pub product: String,
    pub quantity: Decimal,
    pub unit: HarvestUnit,
    #[serde(default)]
    pub operational_value: Option<Decimal>,
    #[serde(default)]
    pub notes: String,
}

impl NewProduceDonation {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.quantity <= Decimal::ZERO {
            return Err(ValidationError::NotPositive("quantity"));
        }
        if self.operational_value.is_some_and(|v| v < Decimal::ZERO) {
            return Err(ValidationError::Negative("value"));
        }
        Ok(Self {
            recipient: non_blank("recipient", &self.recipient)?,
            product: non_blank("product", &self.product)?,
            notes: self.notes.trim().to_owned(),
            ..self
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProduceDonation {
    pub id: Uuid,
    pub donated_on: Date,
    pub recipient: String,
    pub product: String,
    pub quantity: Decimal,
    pub unit: HarvestUnit,
    pub operational_value: Option<Decimal>,
    pub notes: String,
    /// The harvest it was part of, when it was.
    pub harvest_id: Option<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn d(n: i64) -> Decimal {
        Decimal::from(n)
    }

    #[test]
    fn a_donation_names_its_recipient_and_nothing_else_does() {
        let gift =
            NewDisposition { recipient: " Pantry ".into(), ..NewDisposition::of(DispositionKind::Donation, d(15)) };
        assert_eq!(gift.validated().expect("ok").recipient, "Pantry");
        assert_eq!(
            NewDisposition::of(DispositionKind::Donation, d(15)).validated(),
            Err(ValidationError::Blank("recipient"))
        );
        let sale = NewDisposition { recipient: "Pantry".into(), ..NewDisposition::of(DispositionKind::Sale, d(1)) };
        assert!(sale.validated().is_err());
        assert!(NewDisposition::of(DispositionKind::Compost, d(0)).validated().is_err());
        let valued = NewDisposition {
            recipient: "Pantry".into(),
            operational_value: Some(d(-1)),
            ..NewDisposition::of(DispositionKind::Donation, d(1))
        };
        assert!(valued.validated().is_err());
    }

    #[test]
    fn a_split_harvest_reads_as_shares() {
        use DispositionKind::*;
        // The worked example: 42 lb of tomatoes.
        let s = shares(d(42), &[(Sale, d(22)), (Donation, d(15)), (Community, d(3)), (Compost, d(2))]);
        assert_eq!(
            s.parts,
            [
                (Fate::Sold, d(22), Decimal::new(524, 1)),
                (Fate::Donated, d(15), Decimal::new(357, 1)),
                (Fate::Used, d(3), Decimal::new(71, 1)),
                (Fate::Lost, d(2), Decimal::new(48, 1)),
            ]
        );
        assert_eq!(s.unrecorded, d(0));
        let partial = shares(d(10), &[(Feed, d(2)), (Seed, d(1))]);
        assert_eq!((partial.parts.len(), partial.unrecorded), (1, d(7)));
    }

    #[test]
    fn standalone_donations_are_checked() {
        let g = NewProduceDonation {
            donated_on: date!(2026 - 09 - 01),
            recipient: "Pantry".into(),
            product: "Eggs".into(),
            quantity: d(3),
            unit: HarvestUnit::Dozen,
            operational_value: None,
            notes: String::new(),
        };
        assert!(g.clone().validated().is_ok());
        assert!(NewProduceDonation { product: " ".into(), ..g.clone() }.validated().is_err());
        assert!(NewProduceDonation { quantity: d(0), ..g }.validated().is_err());
    }

    #[test]
    fn literals_match_the_migration() {
        let sql = include_str!("../../../migrations/0021_farm_ops.sql");
        let after = &sql[sql.find("create table homestead.harvest_dispositions").expect("table")..];
        crate::tests::assert_check_list(after, "kind", DispositionKind::ALL, DispositionKind::as_str);
    }
}

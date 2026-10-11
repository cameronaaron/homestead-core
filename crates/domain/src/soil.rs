//! A growing area's soil: what tests found and what was added.
//!
//! Soil reports differ lab to lab (ppm, lb/acre, "low/medium/high"), so a
//! test keeps each value as the lab wrote it. Only pH is a number, because
//! it is always the same scale.

use std::collections::BTreeMap;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::Date;
use uuid::Uuid;

use crate::{ValidationError, non_blank};

string_enum!(AmendmentKind, "kind" {
    Compost => "compost", Fertilizer => "fertilizer", Lime => "lime", Sulfur => "sulfur", Mulch => "mulch",
    Other => "other",
});

/// Lab measurements beyond the usual ones: name → value as reported.
pub type Measurements = BTreeMap<String, String>;

/// Reads "Calcium = 1,200 ppm" or "CEC: 14" lines into measurements.
pub fn parse_measurements(text: &str) -> Result<Measurements, ValidationError> {
    let mut out = Measurements::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (name, value) = line
            .split_once('=')
            .or_else(|| line.split_once(':'))
            .ok_or(ValidationError::Malformed("measurement (one per line, like \"Calcium = 1200 ppm\")"))?;
        out.insert(name.trim().to_owned(), value.trim().to_owned());
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewSoilTest {
    pub location_id: Uuid,
    pub tested_on: Date,
    #[serde(default)]
    pub ph: Option<Decimal>,
    #[serde(default)]
    pub nitrogen: String,
    #[serde(default)]
    pub phosphorus: String,
    #[serde(default)]
    pub potassium: String,
    #[serde(default)]
    pub organic_matter: String,
    /// Who tested it: a lab's name, or "home kit".
    #[serde(default)]
    pub lab: String,
    #[serde(default)]
    pub measurements: Measurements,
    #[serde(default)]
    pub notes: String,
}

impl NewSoilTest {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.ph.is_some_and(|p| p < Decimal::ZERO || p > Decimal::from(14)) {
            return Err(ValidationError::Malformed("pH (0–14)"));
        }
        if self.measurements.len() > 40 {
            return Err(ValidationError::Malformed("measurements (at most 40)"));
        }
        let mut measurements = Measurements::new();
        for (name, value) in &self.measurements {
            let name = non_blank("measurement name", name)?;
            if name.chars().count() > 60 {
                return Err(ValidationError::Malformed("measurement name (at most 60 characters)"));
            }
            measurements.insert(name, non_blank("measurement value", value)?);
        }
        let t = |s: &str| s.trim().to_owned();
        let out = Self {
            nitrogen: t(&self.nitrogen),
            phosphorus: t(&self.phosphorus),
            potassium: t(&self.potassium),
            organic_matter: t(&self.organic_matter),
            lab: t(&self.lab),
            notes: t(&self.notes),
            measurements,
            ..self
        };
        let found = out.ph.is_some()
            || ![&out.nitrogen, &out.phosphorus, &out.potassium, &out.organic_matter].iter().all(|s| s.is_empty())
            || !out.measurements.is_empty();
        if !found {
            return Err(ValidationError::Blank("result (record at least one reading)"));
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SoilTest {
    pub id: Uuid,
    pub location_id: Uuid,
    pub tested_on: Date,
    pub ph: Option<Decimal>,
    pub nitrogen: String,
    pub phosphorus: String,
    pub potassium: String,
    pub organic_matter: String,
    pub lab: String,
    pub measurements: Measurements,
    pub notes: String,
}

impl SoilTest {
    /// "pH 6.4 · N 12 ppm · P high", for a timeline line.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(p) = self.ph {
            parts.push(format!("pH {}", p.normalize()));
        }
        for (label, v) in
            [("N", &self.nitrogen), ("P", &self.phosphorus), ("K", &self.potassium), ("OM", &self.organic_matter)]
        {
            if !v.is_empty() {
                parts.push(format!("{label} {v}"));
            }
        }
        parts.extend(self.measurements.iter().map(|(k, v)| format!("{k} {v}")));
        parts.join(" · ")
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewAmendment {
    pub location_id: Uuid,
    pub applied_on: Date,
    pub kind: AmendmentKind,
    /// What exactly: "worm castings", "12-0-0 feather meal". Defaults to the kind.
    #[serde(default)]
    pub material: String,
    #[serde(default)]
    pub amount: Option<Decimal>,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub cost: Option<Decimal>,
    #[serde(default)]
    pub notes: String,
}

impl NewAmendment {
    pub fn validated(self) -> Result<Self, ValidationError> {
        let unit = self.unit.trim().to_owned();
        if self.amount.is_some() == unit.is_empty() {
            return Err(ValidationError::Malformed("amount (give both an amount and its unit, or neither)"));
        }
        if self.amount.is_some_and(|a| a <= Decimal::ZERO) {
            return Err(ValidationError::NotPositive("amount"));
        }
        if self.cost.is_some_and(|c| c < Decimal::ZERO) {
            return Err(ValidationError::Negative("cost"));
        }
        let material = match self.material.trim() {
            "" => self.kind.as_str().to_owned(),
            m => m.to_owned(),
        };
        Ok(Self { material, unit, reason: self.reason.trim().to_owned(), notes: self.notes.trim().to_owned(), ..self })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Amendment {
    pub id: Uuid,
    pub location_id: Uuid,
    pub applied_on: Date,
    pub kind: AmendmentKind,
    pub material: String,
    pub amount: Option<Decimal>,
    pub unit: String,
    pub reason: String,
    pub cost: Option<Decimal>,
    pub notes: String,
}

/// One line of a place's soil history.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SoilEntry {
    Test(SoilTest),
    Amendment(Amendment),
}

impl SoilEntry {
    pub fn on(&self) -> Date {
        match self {
            SoilEntry::Test(t) => t.tested_on,
            SoilEntry::Amendment(a) => a.applied_on,
        }
    }
}

/// Tests and amendments as one history, newest first, each with whatever
/// the caller tags it with (the place it was in). On the same day the
/// amendment reads above the test, because a test is taken before amending.
pub fn soil_history<P>(tests: Vec<(SoilTest, P)>, amendments: Vec<(Amendment, P)>) -> Vec<(SoilEntry, P)> {
    let mut all: Vec<(SoilEntry, P)> = tests
        .into_iter()
        .map(|(t, p)| (SoilEntry::Test(t), p))
        .chain(amendments.into_iter().map(|(a, p)| (SoilEntry::Amendment(a), p)))
        .collect();
    all.sort_by_key(|(e, _)| std::cmp::Reverse((e.on(), matches!(e, SoilEntry::Amendment(_)))));
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn test_at(on: Date) -> NewSoilTest {
        NewSoilTest {
            location_id: Uuid::nil(),
            tested_on: on,
            ph: Some(Decimal::new(64, 1)),
            nitrogen: String::new(),
            phosphorus: " high ".into(),
            potassium: String::new(),
            organic_matter: String::new(),
            lab: "UC Davis".into(),
            measurements: Measurements::new(),
            notes: String::new(),
        }
    }

    #[test]
    fn a_test_keeps_values_as_the_lab_wrote_them() {
        let m = parse_measurements("Calcium = 1,200 ppm\n\nCEC: 14 meq/100g\n").expect("parsed");
        assert_eq!(m.get("Calcium").map(String::as_str), Some("1,200 ppm"));
        assert_eq!(m.get("CEC").map(String::as_str), Some("14 meq/100g"));
        assert!(parse_measurements("just a number").is_err());
        let t = NewSoilTest { measurements: m, ..test_at(date!(2026 - 03 - 01)) }.validated().expect("ok");
        assert_eq!(t.phosphorus, "high");
    }

    #[test]
    fn a_test_needs_a_reading_and_a_sane_ph() {
        let empty = NewSoilTest { ph: None, phosphorus: String::new(), ..test_at(date!(2026 - 03 - 01)) };
        assert!(empty.clone().validated().is_err());
        assert!(NewSoilTest { organic_matter: "4.1%".into(), ..empty.clone() }.validated().is_ok());
        assert!(NewSoilTest { ph: Some(Decimal::from(15)), ..test_at(date!(2026 - 03 - 01)) }.validated().is_err());
        let blank_value = Measurements::from([("Ca".to_owned(), " ".to_owned())]);
        assert!(NewSoilTest { measurements: blank_value, ..empty }.validated().is_err());
    }

    #[test]
    fn an_amendment_has_both_amount_and_unit_or_neither() {
        let a = NewAmendment {
            location_id: Uuid::nil(),
            applied_on: date!(2026 - 03 - 02),
            kind: AmendmentKind::Lime,
            material: " ".into(),
            amount: Some(Decimal::from(5)),
            unit: "lb".into(),
            reason: "pH 5.8".into(),
            cost: None,
            notes: String::new(),
        };
        assert_eq!(a.clone().validated().expect("ok").material, "lime", "defaults to its kind");
        assert!(NewAmendment { unit: String::new(), ..a.clone() }.validated().is_err());
        assert!(NewAmendment { amount: None, unit: String::new(), ..a.clone() }.validated().is_ok());
        assert!(NewAmendment { cost: Some(Decimal::NEGATIVE_ONE), ..a }.validated().is_err());
    }

    #[test]
    fn history_is_newest_first_with_a_same_day_amendment_above_its_test() {
        let t = |on| SoilTest {
            id: Uuid::nil(),
            location_id: Uuid::nil(),
            tested_on: on,
            ph: None,
            nitrogen: String::new(),
            phosphorus: String::new(),
            potassium: String::new(),
            organic_matter: String::new(),
            lab: String::new(),
            measurements: Measurements::new(),
            notes: String::new(),
        };
        let a = |on| Amendment {
            id: Uuid::nil(),
            location_id: Uuid::nil(),
            applied_on: on,
            kind: AmendmentKind::Compost,
            material: "compost".into(),
            amount: None,
            unit: String::new(),
            reason: String::new(),
            cost: None,
            notes: String::new(),
        };
        let h = soil_history(
            vec![(t(date!(2026 - 01 - 10)), ()), (t(date!(2026 - 03 - 01)), ())],
            vec![(a(date!(2026 - 03 - 01)), ()), (a(date!(2026 - 02 - 01)), ())],
        );
        let kinds: Vec<_> = h.iter().map(|(e, _)| (e.on(), matches!(e, SoilEntry::Test(_)))).collect();
        assert_eq!(
            kinds,
            [
                (date!(2026 - 03 - 01), false),
                (date!(2026 - 03 - 01), true),
                (date!(2026 - 02 - 01), false),
                (date!(2026 - 01 - 10), true)
            ]
        );
    }

    #[test]
    fn literals_match_the_migration() {
        let sql = include_str!("../../../migrations/0029_soil_and_crop_health.sql");
        crate::tests::assert_check_list(sql, "kind", AmendmentKind::ALL, AmendmentKind::as_str);
    }
}

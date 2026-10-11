//! Readings: numbers taken off something on the farm (compost pile
//! temperature, a bed's soil moisture, what the flock ate) without a new
//! table for each. A reading is one look at one thing, and can carry
//! several values ("temperature 142 °F, moisture 55 %").
//!
//! The typed records (harvests, soil tests, egg days, irrigation) stay the
//! way to record what they cover; this is for measurements nobody built a
//! page for yet. Values keep the unit they were taken in and are never
//! converted, so a trend only ever compares like with like.

use std::collections::BTreeMap;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::ValidationError;
use crate::auth::Capability;

/// What was measured. Exactly one; a planting's reading shows on its
/// place's page too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum ReadingSubject {
    Location(Uuid),
    Planting(Uuid),
    Animal(Uuid),
    Equipment(Uuid),
}

impl ReadingSubject {
    /// Who may see readings of it: whoever may see the thing itself.
    pub fn read_cap(self) -> Capability {
        match self {
            ReadingSubject::Location(_) | ReadingSubject::Planting(_) => Capability::GardenRead,
            ReadingSubject::Animal(_) => Capability::AnimalsRead,
            ReadingSubject::Equipment(_) => Capability::EquipmentRead,
        }
    }

    /// Who may take one: whoever may record the thing's own work.
    pub fn write_cap(self) -> Capability {
        match self {
            ReadingSubject::Location(_) | ReadingSubject::Planting(_) => Capability::GardenWrite,
            ReadingSubject::Animal(_) => Capability::AnimalsWrite,
            ReadingSubject::Equipment(_) => Capability::EquipmentWrite,
        }
    }

    /// `(location, planting, animal, equipment)`, one of them set.
    pub fn ids(self) -> (Option<Uuid>, Option<Uuid>, Option<Uuid>, Option<Uuid>) {
        match self {
            ReadingSubject::Location(id) => (Some(id), None, None, None),
            ReadingSubject::Planting(id) => (None, Some(id), None, None),
            ReadingSubject::Animal(id) => (None, None, Some(id), None),
            ReadingSubject::Equipment(id) => (None, None, None, Some(id)),
        }
    }

    pub fn from_ids(
        location: Option<Uuid>,
        planting: Option<Uuid>,
        animal: Option<Uuid>,
        equipment: Option<Uuid>,
    ) -> Option<Self> {
        match (location, planting, animal, equipment) {
            (Some(id), None, None, None) => Some(ReadingSubject::Location(id)),
            (None, Some(id), None, None) => Some(ReadingSubject::Planting(id)),
            (None, None, Some(id), None) => Some(ReadingSubject::Animal(id)),
            (None, None, None, Some(id)) => Some(ReadingSubject::Equipment(id)),
            _ => None,
        }
    }

    /// Its form value, `location:<id>`.
    pub fn key(self) -> String {
        let (kind, id) = match self {
            ReadingSubject::Location(id) => ("location", id),
            ReadingSubject::Planting(id) => ("planting", id),
            ReadingSubject::Animal(id) => ("animal", id),
            ReadingSubject::Equipment(id) => ("equipment", id),
        };
        format!("{kind}:{id}")
    }
}

impl std::str::FromStr for ReadingSubject {
    type Err = ValidationError;
    fn from_str(s: &str) -> Result<Self, ValidationError> {
        let bad = || ValidationError::Malformed("what was measured");
        let (kind, id) = s.trim().split_once(':').ok_or_else(bad)?;
        let id: Uuid = id.parse().map_err(|_| bad())?;
        match kind {
            "location" => Ok(ReadingSubject::Location(id)),
            "planting" => Ok(ReadingSubject::Planting(id)),
            "animal" => Ok(ReadingSubject::Animal(id)),
            "equipment" => Ok(ReadingSubject::Equipment(id)),
            _ => Err(bad()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadingValue {
    /// "temperature"; "value" when the reading has only the one number.
    pub name: String,
    pub value: Decimal,
    /// As taken ("°F", "%", "lb"); may be empty for a count.
    #[serde(default)]
    pub unit: String,
}

pub const MAX_VALUES: usize = 20;
/// The column is `numeric(14, 4)`.
const MAX_ABS: i64 = 9_999_999_999;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewReading {
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    /// "compost temperature": free text, so a new measurement needs no
    /// code. Stored lower-case with single spaces, so it groups.
    pub kind: String,
    pub subject: ReadingSubject,
    pub values: Vec<ReadingValue>,
    #[serde(default)]
    pub notes: String,
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl NewReading {
    pub fn validated(self) -> Result<Self, ValidationError> {
        let kind = squash(&self.kind).to_lowercase();
        if kind.is_empty() {
            return Err(ValidationError::Blank("what kind of reading"));
        }
        if kind.chars().count() > 60 {
            return Err(ValidationError::Malformed("kind (60 characters at most)"));
        }
        if self.values.is_empty() {
            return Err(ValidationError::Blank("values"));
        }
        if self.values.len() > MAX_VALUES {
            return Err(ValidationError::Malformed("values (20 at most)"));
        }
        let mut seen = std::collections::HashSet::new();
        let mut values = Vec::with_capacity(self.values.len());
        for v in self.values {
            let name = squash(&v.name);
            let unit = squash(&v.unit);
            if name.is_empty() {
                return Err(ValidationError::Blank("value name"));
            }
            if name.chars().count() > 60 || unit.chars().count() > 20 {
                return Err(ValidationError::Malformed("value (names 60 characters, units 20 at most)"));
            }
            if !seen.insert(name.to_lowercase()) {
                return Err(ValidationError::Malformed("values (a name appears twice)"));
            }
            // Round here rather than let the column round silently.
            let value = v.value.round_dp(4);
            if value.abs() > Decimal::from(MAX_ABS) {
                return Err(ValidationError::Malformed("value (too large)"));
            }
            values.push(ReadingValue { name, value, unit });
        }
        let notes = self.notes.trim().to_owned();
        if notes.chars().count() > 500 {
            return Err(ValidationError::Malformed("notes (500 characters at most)"));
        }
        Ok(Self { kind, values, notes, ..self })
    }
}

/// Reads what a person typed, one value per line: `temperature = 142 °F`,
/// `moisture: 55%`, or just `142 °F` for a one-number reading.
pub fn parse_values(text: &str) -> Result<Vec<ReadingValue>, ValidationError> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|line| {
            let (name, rest) = match line.split_once(['=', ':']) {
                Some((n, r)) => (n.trim(), r.trim()),
                None => ("value", line),
            };
            let split = rest
                .char_indices()
                .find(|(i, c)| !(c.is_ascii_digit() || *c == '.' || *c == ',' || (*i == 0 && (*c == '-' || *c == '+'))))
                .map_or(rest.len(), |(i, _)| i);
            let number = rest[..split].replace(',', "");
            let value = number
                .parse::<Decimal>()
                .map_err(|_| ValidationError::Malformed("value (expected a number, like temperature = 142 °F)"))?;
            Ok(ReadingValue { name: name.to_owned(), value, unit: rest[split..].trim().to_owned() })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reading {
    pub id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    pub kind: String,
    pub subject: ReadingSubject,
    /// "Pile 2", "Tomato in Bed A", "Hens".
    pub subject_name: String,
    pub values: Vec<ReadingValue>,
    pub notes: String,
}

impl Reading {
    /// "temperature 142 °F · moisture 55 %"; a lone "value" is just "142 °F".
    pub fn summary(&self) -> String {
        self.values
            .iter()
            .map(|v| {
                let n = v.value.normalize();
                let amount = if v.unit.is_empty() { n.to_string() } else { format!("{n} {}", v.unit) };
                if v.name == "value" { amount } else { format!("{} {amount}", v.name) }
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// One measured quantity over time, at one thing, in one unit.
#[derive(Debug, Clone, PartialEq)]
pub struct Trend {
    pub kind: String,
    pub subject_name: String,
    pub name: String,
    pub unit: String,
    /// Oldest first.
    pub points: Vec<(OffsetDateTime, Decimal)>,
}

impl Trend {
    pub fn latest(&self) -> Decimal {
        self.points.last().expect("a trend has a point").1
    }
    pub fn min(&self) -> Decimal {
        self.points.iter().map(|p| p.1).min().expect("a point")
    }
    pub fn max(&self) -> Decimal {
        self.points.iter().map(|p| p.1).max().expect("a point")
    }
}

/// Groups readings into trends by kind, thing, value name and unit, most
/// recently read first. A value taken in °C and another in °F are two
/// trends, never averaged together.
pub fn trends(readings: &[Reading]) -> Vec<Trend> {
    let mut by: BTreeMap<(String, ReadingSubject, String, String), Trend> = BTreeMap::new();
    for r in readings {
        for v in &r.values {
            let key = (r.kind.clone(), r.subject, v.name.to_lowercase(), v.unit.clone());
            by.entry(key)
                .or_insert_with(|| Trend {
                    kind: r.kind.clone(),
                    subject_name: r.subject_name.clone(),
                    name: v.name.clone(),
                    unit: v.unit.clone(),
                    points: Vec::new(),
                })
                .points
                .push((r.observed_at, v.value));
        }
    }
    let mut out: Vec<Trend> = by
        .into_values()
        .map(|mut t| {
            t.points.sort_by_key(|p| p.0);
            t
        })
        .collect();
    out.sort_by(|a, b| b.points.last().map(|p| p.0).cmp(&a.points.last().map(|p| p.0)));
    out
}

impl PartialOrd for ReadingSubject {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ReadingSubject {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key().cmp(&other.key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn dec(s: &str) -> Decimal {
        s.parse().expect("decimal")
    }

    #[test]
    fn values_are_read_as_people_write_them() {
        let v = parse_values("temperature = 142 °F\nmoisture: 55%\n\n  -3.5 in  \ncount=1,200").expect("parse");
        let got: Vec<_> = v.iter().map(|v| (v.name.as_str(), v.value, v.unit.as_str())).collect();
        assert_eq!(
            got,
            [
                ("temperature", dec("142"), "°F"),
                ("moisture", dec("55"), "%"),
                ("value", dec("-3.5"), "in"),
                ("count", dec("1200"), ""),
            ]
        );
        assert!(parse_values("temperature = hot").is_err());
        assert!(parse_values("temperature = °F 142").is_err(), "the number comes first");
        assert!(parse_values("= 5").map(|v| v[0].name.is_empty()).unwrap_or(false), "caught by validation instead");
    }

    fn reading(values: Vec<ReadingValue>) -> NewReading {
        NewReading {
            observed_at: datetime!(2026-09-29 08:00 UTC),
            kind: "  Compost   Temperature ".into(),
            subject: ReadingSubject::Location(Uuid::nil()),
            values,
            notes: String::new(),
        }
    }

    #[test]
    fn a_reading_groups_by_a_tidy_kind_and_refuses_ambiguity() {
        let ok = reading(parse_values("temperature = 142.123456 °F").expect("v")).validated().expect("ok");
        assert_eq!(ok.kind, "compost temperature", "one spelling, so readings group");
        assert_eq!(ok.values[0].value, dec("142.1235"), "rounded to the column's four places, not silently by it");
        assert!(reading(vec![]).validated().is_err());
        assert!(reading(parse_values("t = 1\nT = 2").expect("v")).validated().is_err(), "a name twice");
        assert!(reading(parse_values("= 5").expect("v")).validated().is_err(), "a blank name");
        assert!(reading(parse_values("x = 99999999999").expect("v")).validated().is_err(), "past numeric(14,4)");
        let many = (0..21).map(|i| format!("v{i} = 1")).collect::<Vec<_>>().join("\n");
        assert!(reading(parse_values(&many).expect("v")).validated().is_err());
        assert!(NewReading { kind: " ".into(), ..reading(parse_values("1").expect("v")) }.validated().is_err());
    }

    #[test]
    fn subjects_round_trip_and_carry_their_capabilities() {
        let id = Uuid::new_v4();
        for s in [
            ReadingSubject::Location(id),
            ReadingSubject::Planting(id),
            ReadingSubject::Animal(id),
            ReadingSubject::Equipment(id),
        ] {
            assert_eq!(s.key().parse::<ReadingSubject>(), Ok(s));
            let (l, p, a, e) = s.ids();
            assert_eq!(ReadingSubject::from_ids(l, p, a, e), Some(s));
        }
        assert_eq!(ReadingSubject::from_ids(Some(id), None, Some(id), None), None, "exactly one");
        assert!("barn:1".parse::<ReadingSubject>().is_err());
        assert_eq!(ReadingSubject::Animal(id).write_cap(), Capability::AnimalsWrite);
        assert_eq!(ReadingSubject::Planting(id).read_cap(), Capability::GardenRead);
        assert_eq!(ReadingSubject::Equipment(id).read_cap(), Capability::EquipmentRead);
    }

    #[test]
    fn trends_keep_units_apart_and_run_oldest_first() {
        let pile = ReadingSubject::Location(Uuid::nil());
        let r = |at, v: &str| Reading {
            id: Uuid::new_v4(),
            observed_at: at,
            kind: "compost temperature".into(),
            subject: pile,
            subject_name: "Pile 2".into(),
            values: parse_values(v).expect("v"),
            notes: String::new(),
        };
        let readings = [
            r(datetime!(2026-09-03 08:00 UTC), "temperature = 150 °F"),
            r(datetime!(2026-09-01 08:00 UTC), "temperature = 120 °F"),
            r(datetime!(2026-09-02 08:00 UTC), "Temperature = 60 °C"),
            r(datetime!(2026-09-04 08:00 UTC), "temperature = 141 °F"),
        ];
        let t = trends(&readings);
        assert_eq!(t.len(), 2, "°C and °F are separate trends");
        let f = &t[0];
        assert_eq!((f.unit.as_str(), f.points.len()), ("°F", 3), "the most recently read first");
        assert_eq!((f.latest(), f.min(), f.max()), (dec("141"), dec("120"), dec("150")));
        assert!(f.points.windows(2).all(|w| w[0].0 < w[1].0), "oldest first");
        assert_eq!(readings[0].summary(), "temperature 150 °F");
        assert_eq!(r(datetime!(2026-09-01 0:00 UTC), "3 lb\nwater = 2 gal").summary(), "3 lb · water 2 gal");
    }

    #[test]
    fn check_limits_match_migration() {
        let sql = include_str!("../../../migrations/0043_readings.sql");
        assert!(
            sql.contains("numeric(14, 4)")
                && sql.contains("num_nonnulls(location_id, planting_id, animal_id, equipment_id) = 1")
        );
    }
}

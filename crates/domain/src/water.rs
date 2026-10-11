//! Weather observations and irrigation.
//!
//! People record these by hand today; sensors or a controller may later.
//! Every record names its `source` (`manual` for people), so a sensor feed
//! can be retried idempotently and never overwrites what someone wrote.
//! Stored in SI units (°C, mm, km/h, litres); the UI speaks °F, inches and
//! gallons.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::ValidationError;

pub const MANUAL: &str = "manual";

/// Litres in a US gallon.
pub fn gallons_to_litres(g: Decimal) -> Decimal {
    (g * Decimal::new(3_785_411_784, 9)).round_dp(3)
}

pub fn litres_to_gallons(l: Decimal) -> Decimal {
    (l / Decimal::new(3_785_411_784, 9)).round_dp(1)
}

pub fn fahrenheit_to_celsius(f: Decimal) -> Decimal {
    ((f - Decimal::from(32)) * Decimal::from(5) / Decimal::from(9)).round_dp(2)
}

pub fn celsius_to_fahrenheit(c: Decimal) -> Decimal {
    (c * Decimal::from(9) / Decimal::from(5) + Decimal::from(32)).round_dp(1)
}

pub fn inches_to_mm(i: Decimal) -> Decimal {
    (i * Decimal::new(254, 1)).round_dp(2)
}

pub fn mm_to_inches(mm: Decimal) -> Decimal {
    (mm / Decimal::new(254, 1)).round_dp(2)
}

pub fn mph_to_kph(m: Decimal) -> Decimal {
    (m * Decimal::new(1_609_344, 6)).round_dp(2)
}

/// A machine source name: lower-case `kind:id` like `station:backyard`.
fn check_source(source: &str) -> Result<String, ValidationError> {
    let s = source.trim().to_lowercase();
    let ok = s == MANUAL
        || (s.contains(':')
            && s.len() <= 80
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'-' | b'_' | b'.')));
    if ok { Ok(s) } else { Err(ValidationError::Malformed("source (manual, or kind:id like station:backyard)")) }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewWeather {
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    #[serde(default = "manual")]
    pub source: String,
    #[serde(default)]
    pub location_id: Option<Uuid>,
    #[serde(default)]
    pub temperature_c: Option<Decimal>,
    #[serde(default)]
    pub precipitation_mm: Option<Decimal>,
    #[serde(default)]
    pub wind_kph: Option<Decimal>,
    #[serde(default)]
    pub humidity_pct: Option<Decimal>,
    #[serde(default)]
    pub conditions: String,
    /// The sensor's own payload, kept as received.
    #[serde(default)]
    pub raw: Option<serde_json::Value>,
    #[serde(default)]
    pub soil_temperature_c: Option<Decimal>,
    #[serde(default)]
    pub frost: bool,
}

fn manual() -> String {
    MANUAL.into()
}

impl NewWeather {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.precipitation_mm.is_some_and(|p| p.is_sign_negative()) {
            return Err(ValidationError::Negative("rain"));
        }
        if self.wind_kph.is_some_and(|w| w.is_sign_negative()) {
            return Err(ValidationError::Negative("wind"));
        }
        if self.humidity_pct.is_some_and(|h| h.is_sign_negative() || h > Decimal::from(100)) {
            return Err(ValidationError::Malformed("humidity (0–100%)"));
        }
        if self.temperature_c.is_some_and(|t| t < Decimal::from(-60) || t > Decimal::from(60)) {
            return Err(ValidationError::Malformed("temperature (outside −60…60 °C)"));
        }
        let conditions = self.conditions.trim().to_owned();
        let measured =
            [self.temperature_c, self.precipitation_mm, self.wind_kph, self.humidity_pct, self.soil_temperature_c];
        if measured.iter().all(Option::is_none) && conditions.is_empty() && !self.frost {
            return Err(ValidationError::Blank("observation (record at least one reading or the conditions)"));
        }
        Ok(Self { source: check_source(&self.source)?, conditions, ..self })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Weather {
    pub id: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    pub source: String,
    pub location_id: Option<Uuid>,
    pub location_name: Option<String>,
    pub temperature_c: Option<Decimal>,
    pub precipitation_mm: Option<Decimal>,
    pub wind_kph: Option<Decimal>,
    pub humidity_pct: Option<Decimal>,
    pub conditions: String,
    #[serde(default)]
    pub soil_temperature_c: Option<Decimal>,
    #[serde(default)]
    pub frost: bool,
}

/// At or above this, a reading is flagged as extreme heat (95 °F). A display
/// threshold for people, not a rule anything acts on.
pub const HEAT_C: i64 = 35;

impl Weather {
    pub fn is_extreme_heat(&self) -> bool {
        self.temperature_c.is_some_and(|t| t >= Decimal::from(HEAT_C))
    }
}

string_enum!(IrrigationProblem, "problem" {
    Leak => "leak", Blocked => "blocked", BrokenLine => "broken_line", LowPressure => "low_pressure",
    Controller => "controller", Other => "other",
});

impl IrrigationProblem {
    pub fn label(self) -> &'static str {
        match self {
            IrrigationProblem::Leak => "leak",
            IrrigationProblem::Blocked => "blocked emitter",
            IrrigationProblem::BrokenLine => "broken line",
            IrrigationProblem::LowPressure => "low pressure",
            IrrigationProblem::Controller => "controller trouble",
            IrrigationProblem::Other => "problem",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewZone {
    pub name: String,
    #[serde(default)]
    pub location_ids: Vec<Uuid>,
    /// The id a controller knows this zone by, if one runs it.
    #[serde(default)]
    pub controller_ref: String,
    #[serde(default)]
    pub notes: String,
}

impl NewZone {
    pub fn validated(self) -> Result<Self, ValidationError> {
        let mut location_ids = self.location_ids;
        location_ids.sort();
        location_ids.dedup();
        Ok(Self {
            name: crate::non_blank("name", &self.name)?,
            location_ids,
            controller_ref: self.controller_ref.trim().to_owned(),
            notes: self.notes.trim().to_owned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Zone {
    pub id: Uuid,
    pub name: String,
    pub controller_ref: String,
    pub notes: String,
    pub active: bool,
    /// The places it serves, by id and name.
    pub places: Vec<(Uuid, String)>,
}

string_enum!(IrrigationMethod, "method" {
    Drip => "drip", Sprinkler => "sprinkler", Hand => "hand", Soaker => "soaker", Flood => "flood", Other => "other",
});

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewIrrigation {
    /// Where. Taken from the planting when one is given.
    #[serde(default)]
    pub location_id: Option<Uuid>,
    #[serde(default)]
    pub planting_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub ended_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub water_liters: Option<Decimal>,
    pub method: IrrigationMethod,
    #[serde(default)]
    pub notes: String,
    #[serde(default = "manual")]
    pub source: String,
    /// A controller's own id for the run, so a retried report is recorded once.
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub zone_id: Option<Uuid>,
    /// Something wrong found while watering; opens a task to fix it.
    #[serde(default)]
    pub problem: Option<IrrigationProblem>,
}

impl NewIrrigation {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.location_id.is_none() && self.planting_id.is_none() && self.zone_id.is_none() {
            return Err(ValidationError::Blank("where (a place, a planting or a zone)"));
        }
        if self.problem == Some(IrrigationProblem::Other) && self.notes.trim().is_empty() {
            return Err(ValidationError::Blank("notes (say what the problem is)"));
        }
        if self.ended_at.is_some_and(|e| e < self.started_at) {
            return Err(ValidationError::Malformed("end (it's before the start)"));
        }
        if self.ended_at.is_some_and(|e| e - self.started_at > time::Duration::hours(48)) {
            return Err(ValidationError::Malformed("end (a run over 48 hours is probably a typo)"));
        }
        if self.water_liters.is_some_and(|w| w.is_sign_negative()) {
            return Err(ValidationError::Negative("water"));
        }
        let source = check_source(&self.source)?;
        let external_id = self.external_id.map(|x| x.trim().to_owned()).filter(|x| !x.is_empty());
        if source == MANUAL && external_id.is_some() {
            return Err(ValidationError::Malformed("external id (only machine sources have one)"));
        }
        Ok(Self { source, external_id, notes: self.notes.trim().to_owned(), ..self })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Irrigation {
    pub id: Uuid,
    /// Absent for a zone's run, which waters every place the zone serves.
    pub location_id: Option<Uuid>,
    /// The place, or the zone's name for a zone's run.
    pub location_name: String,
    pub zone_id: Option<Uuid>,
    pub problem: Option<IrrigationProblem>,
    pub planting_id: Option<Uuid>,
    pub planting_crop: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub ended_at: Option<OffsetDateTime>,
    pub water_liters: Option<Decimal>,
    pub method: IrrigationMethod,
    pub notes: String,
    pub source: String,
}

impl Irrigation {
    pub fn minutes(&self) -> Option<i64> {
        self.ended_at.map(|e| (e - self.started_at).whole_minutes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn d(s: &str) -> Decimal {
        s.parse().expect("decimal")
    }

    #[test]
    fn units_convert_both_ways() {
        assert_eq!(gallons_to_litres(d("10")), d("37.854"));
        assert_eq!(litres_to_gallons(d("37.854")), d("10.0"));
        assert_eq!(fahrenheit_to_celsius(d("212")), d("100"));
        assert_eq!(fahrenheit_to_celsius(d("32")), d("0"));
        assert_eq!(celsius_to_fahrenheit(d("37")), d("98.6"));
        assert_eq!(inches_to_mm(d("1")), d("25.4"));
        assert_eq!(mm_to_inches(d("12.7")), d("0.5"));
        assert_eq!(mph_to_kph(d("10")), d("16.09"));
    }

    fn obs() -> NewWeather {
        NewWeather {
            observed_at: datetime!(2026-09-28 14:00 UTC),
            source: "Manual".into(),
            location_id: None,
            temperature_c: Some(d("31.5")),
            precipitation_mm: None,
            wind_kph: None,
            humidity_pct: None,
            conditions: String::new(),
            raw: None,
            soil_temperature_c: None,
            frost: false,
        }
    }

    #[test]
    fn observations_need_a_reading_and_a_sane_source() {
        assert_eq!(obs().validated().expect("ok").source, "manual");
        assert!(NewWeather { temperature_c: None, ..obs() }.validated().is_err());
        assert!(NewWeather { temperature_c: None, conditions: "fog".into(), ..obs() }.validated().is_ok());
        assert!(NewWeather { precipitation_mm: Some(d("-1")), ..obs() }.validated().is_err());
        assert!(NewWeather { humidity_pct: Some(d("101")), ..obs() }.validated().is_err());
        assert!(NewWeather { temperature_c: Some(d("95")), ..obs() }.validated().is_err(), "°F typed as °C");
        assert!(NewWeather { source: "station:backyard".into(), ..obs() }.validated().is_ok());
        assert!(NewWeather { source: "my station".into(), ..obs() }.validated().is_err());
    }

    fn run() -> NewIrrigation {
        NewIrrigation {
            location_id: Some(Uuid::nil()),
            planting_id: None,
            started_at: datetime!(2026-09-28 06:00 UTC),
            ended_at: Some(datetime!(2026-09-28 06:45 UTC)),
            water_liters: Some(d("120")),
            method: IrrigationMethod::Drip,
            notes: String::new(),
            source: MANUAL.into(),
            external_id: None,
            zone_id: None,
            problem: None,
        }
    }

    #[test]
    fn irrigation_runs_are_checked() {
        assert!(run().validated().is_ok());
        assert!(NewIrrigation { location_id: None, ..run() }.validated().is_err());
        assert!(NewIrrigation { location_id: None, planting_id: Some(Uuid::nil()), ..run() }.validated().is_ok());
        assert!(NewIrrigation { ended_at: Some(datetime!(2026-09-28 05:00 UTC)), ..run() }.validated().is_err());
        assert!(NewIrrigation { ended_at: Some(datetime!(2026-10-01 06:00 UTC)), ..run() }.validated().is_err());
        assert!(NewIrrigation { external_id: Some("r-1".into()), ..run() }.validated().is_err());
        let zone_run = NewIrrigation { location_id: None, zone_id: Some(Uuid::nil()), ..run() };
        assert!(zone_run.validated().is_ok(), "a zone alone says where");
        let vague = NewIrrigation { problem: Some(IrrigationProblem::Other), ..run() };
        assert!(vague.validated().is_err(), "\"other\" needs a note");
        let leak = NewIrrigation { problem: Some(IrrigationProblem::Leak), ..run() };
        assert!(leak.validated().is_ok());
        assert!(
            NewIrrigation { source: "controller:zone3".into(), external_id: Some("r-1".into()), ..run() }
                .validated()
                .is_ok()
        );
    }

    #[test]
    fn literals_match_the_migration() {
        let sql = include_str!("../../../migrations/0016_weather_irrigation.sql");
        crate::tests::assert_check_list(sql, "method", IrrigationMethod::ALL, IrrigationMethod::as_str);
        let ops = include_str!("../../../migrations/0021_farm_ops.sql");
        crate::tests::assert_check_list(ops, "problem", IrrigationProblem::ALL, IrrigationProblem::as_str);
    }
}

//! Crop health: pests, disease and other trouble seen in the garden, what
//! was done, and how it ended. People diagnose; this only remembers.

use serde::{Deserialize, Serialize};
use time::Date;
use uuid::Uuid;

use crate::{ValidationError, non_blank};

string_enum!(ObservationCategory, "category" {
    Pest => "pest", Disease => "disease", Nutrient => "nutrient", Water => "water", Weather => "weather",
    Other => "other",
});

string_enum!(Severity, "severity" { Low => "low", Moderate => "moderate", High => "high" });

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewObservation {
    pub observed_on: Date,
    /// Where; taken from the planting when one is named.
    #[serde(default)]
    pub location_id: Option<Uuid>,
    #[serde(default)]
    pub planting_id: Option<Uuid>,
    pub category: ObservationCategory,
    pub observation: String,
    #[serde(default = "low")]
    pub severity: Severity,
    /// How much was affected: "3 plants", "half the bed".
    #[serde(default)]
    pub affected: String,
    #[serde(default)]
    pub action_taken: String,
    /// Makes a follow-up task due that day.
    #[serde(default)]
    pub follow_up_on: Option<Date>,
}

fn low() -> Severity {
    Severity::Low
}

impl NewObservation {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.location_id.is_none() && self.planting_id.is_none() {
            return Err(ValidationError::Blank("where (a place or a planting)"));
        }
        if self.follow_up_on.is_some_and(|f| f < self.observed_on) {
            return Err(ValidationError::Malformed("follow-up date (it's before the observation)"));
        }
        Ok(Self {
            observation: non_blank("observation", &self.observation)?,
            affected: self.affected.trim().to_owned(),
            action_taken: self.action_taken.trim().to_owned(),
            ..self
        })
    }
}

/// How an observation ended.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Resolution {
    pub resolved_on: Date,
    pub outcome: String,
}

impl Resolution {
    pub fn validated(self) -> Result<Self, ValidationError> {
        Ok(Self { outcome: non_blank("outcome", &self.outcome)?, ..self })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Observation {
    pub id: Uuid,
    pub observed_on: Date,
    pub location_id: Option<Uuid>,
    pub place: Option<String>,
    pub planting_id: Option<Uuid>,
    pub crop: Option<String>,
    pub category: ObservationCategory,
    pub observation: String,
    pub severity: Severity,
    pub affected: String,
    pub action_taken: String,
    pub follow_up_on: Option<Date>,
    pub outcome: String,
    pub resolved_on: Option<Date>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn seen() -> NewObservation {
        NewObservation {
            observed_on: date!(2026 - 06 - 01),
            location_id: Some(Uuid::nil()),
            planting_id: None,
            category: ObservationCategory::Pest,
            observation: " Aphids on kale ".into(),
            severity: Severity::Moderate,
            affected: "6 plants".into(),
            action_taken: "Sprayed water".into(),
            follow_up_on: Some(date!(2026 - 06 - 04)),
        }
    }

    #[test]
    fn an_observation_is_somewhere_and_follows_up_later() {
        assert_eq!(seen().validated().expect("ok").observation, "Aphids on kale");
        assert!(NewObservation { location_id: None, ..seen() }.validated().is_err());
        assert!(NewObservation { location_id: None, planting_id: Some(Uuid::nil()), ..seen() }.validated().is_ok());
        assert!(NewObservation { follow_up_on: Some(date!(2026 - 05 - 31)), ..seen() }.validated().is_err());
        assert!(NewObservation { observation: " ".into(), ..seen() }.validated().is_err());
        let r = Resolution { resolved_on: date!(2026 - 06 - 10), outcome: " ".into() };
        assert!(r.validated().is_err(), "closing says how it ended");
    }

    #[test]
    fn literals_match_the_migration() {
        let sql = include_str!("../../../migrations/0021_farm_ops.sql");
        let after = &sql[sql.find("create table homestead.crop_observations").expect("table")..];
        crate::tests::assert_check_list(after, "category", ObservationCategory::ALL, ObservationCategory::as_str);
        crate::tests::assert_check_list(after, "severity", Severity::ALL, Severity::as_str);
    }
}

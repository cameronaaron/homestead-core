//! Incident and safety reports.
//!
//! Enough to learn from what happened and prove it was handled: when,
//! where, what kind, who was involved, what was done then and after. Not a
//! medical record: the only health detail kept is how far treatment went
//! (none, first aid, or medical care), never diagnoses or injuries.
//!
//! Anyone who works here can file a report; reading reports takes
//! `incidents.read`, and a reporter can read back only their own.

use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::{ValidationError, non_blank};

string_enum!(IncidentKind, "kind" {
    Injury => "injury", NearMiss => "near_miss", AnimalBite => "animal_bite", PropertyDamage => "property_damage",
    FoodSafety => "food_safety", Hazard => "hazard", Other => "other",
});

impl IncidentKind {
    pub fn label(self) -> &'static str {
        match self {
            IncidentKind::Injury => "injury",
            IncidentKind::NearMiss => "near miss",
            IncidentKind::AnimalBite => "animal bite or scratch",
            IncidentKind::PropertyDamage => "property damage",
            IncidentKind::FoodSafety => "food-safety issue",
            IncidentKind::Hazard => "safety hazard",
            IncidentKind::Other => "other incident",
        }
    }

    /// Kinds where someone may have been hurt.
    pub fn involves_harm(self) -> bool {
        matches!(self, IncidentKind::Injury | IncidentKind::AnimalBite)
    }
}

string_enum!(
    /// How far treatment went. Deliberately coarse.
    Treatment, "treatment" { None => "none", FirstAid => "first_aid", MedicalCare => "medical_care" }
);

string_enum!(IncidentStatus, "status" { Open => "open", Closed => "closed" });

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewIncident {
    #[serde(with = "time::serde::rfc3339")]
    pub occurred_at: OffsetDateTime,
    #[serde(default)]
    pub location_id: Option<Uuid>,
    /// More precise than the location: "by the north coop door".
    #[serde(default)]
    pub location_detail: String,
    pub kind: IncidentKind,
    pub description: String,
    #[serde(default)]
    pub immediate_action: String,
    /// Names or roles; "a visitor" is fine.
    #[serde(default)]
    pub people_involved: String,
    #[serde(default)]
    pub witnesses: String,
    #[serde(default)]
    pub treatment: Option<Treatment>,
}

impl NewIncident {
    pub fn validated(self, now: OffsetDateTime) -> Result<Self, ValidationError> {
        if self.occurred_at > now + time::Duration::minutes(5) {
            return Err(ValidationError::Malformed("when (it's in the future)"));
        }
        if self.treatment.is_some() && !self.kind.involves_harm() {
            return Err(ValidationError::Malformed("treatment (only for injuries and bites)"));
        }
        let description = non_blank("what happened", &self.description)?;
        if description.chars().count() > 4000 {
            return Err(ValidationError::Malformed("what happened (keep it under 4000 characters)"));
        }
        Ok(Self {
            description,
            location_detail: self.location_detail.trim().to_owned(),
            immediate_action: self.immediate_action.trim().to_owned(),
            people_involved: self.people_involved.trim().to_owned(),
            witnesses: self.witnesses.trim().to_owned(),
            ..self
        })
    }
}

/// What's being done so it doesn't happen again.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FollowUp {
    pub corrective_action: String,
    #[serde(default)]
    pub follow_up_by: Option<Date>,
}

impl FollowUp {
    pub fn validated(self) -> Result<Self, ValidationError> {
        Ok(Self { corrective_action: non_blank("corrective action", &self.corrective_action)?, ..self })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Incident {
    pub id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub occurred_at: OffsetDateTime,
    pub location_id: Option<Uuid>,
    pub location_name: Option<String>,
    pub location_detail: String,
    pub kind: IncidentKind,
    pub description: String,
    pub immediate_action: String,
    pub people_involved: String,
    pub witnesses: String,
    pub treatment: Option<Treatment>,
    pub corrective_action: String,
    pub follow_up_by: Option<Date>,
    pub status: IncidentStatus,
    pub closure_notes: String,
    pub reported_by: Option<Uuid>,
    pub reported_by_name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub reported_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub closed_at: Option<OffsetDateTime>,
    pub closed_by_name: Option<String>,
}

impl Incident {
    /// A short reference safe to say out loud or post: no details.
    pub fn reference(&self) -> String {
        format!("INC-{}", self.id.simple().to_string()[..6].to_uppercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn report() -> NewIncident {
        NewIncident {
            occurred_at: datetime!(2026-09-28 16:00 UTC),
            location_id: None,
            location_detail: " north coop door ".into(),
            kind: IncidentKind::AnimalBite,
            description: " Rooster spurred a volunteer's hand while collecting eggs. ".into(),
            immediate_action: "Washed, bandaged".into(),
            people_involved: "Val (volunteer)".into(),
            witnesses: String::new(),
            treatment: Some(Treatment::FirstAid),
        }
    }

    #[test]
    fn reports_are_checked_and_stay_non_medical() {
        let now = datetime!(2026-09-28 17:00 UTC);
        let ok = report().validated(now).expect("ok");
        assert_eq!(ok.location_detail, "north coop door");
        assert!(NewIncident { description: " ".into(), ..report() }.validated(now).is_err());
        assert!(NewIncident { occurred_at: datetime!(2026-09-29 17:00 UTC), ..report() }.validated(now).is_err());
        let hazard = NewIncident { kind: IncidentKind::Hazard, ..report() };
        assert!(hazard.clone().validated(now).is_err(), "no treatment on a hazard");
        assert!(NewIncident { treatment: None, ..hazard }.validated(now).is_ok());
        assert!(NewIncident { description: "x".repeat(4001), ..report() }.validated(now).is_err());
    }

    #[test]
    fn literals_match_the_migration() {
        let sql = include_str!("../../../migrations/0018_incidents.sql");
        crate::tests::assert_check_list(sql, "kind", IncidentKind::ALL, IncidentKind::as_str);
        crate::tests::assert_check_list(sql, "treatment", Treatment::ALL, Treatment::as_str);
        crate::tests::assert_check_list(sql, "status", IncidentStatus::ALL, IncidentStatus::as_str);
    }
}

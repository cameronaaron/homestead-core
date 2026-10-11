//! Community events and volunteer hours.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::Date;
use uuid::Uuid;

use crate::{ValidationError, non_blank};

string_enum!(EventKind, "kind" {
    Workshop => "workshop", WorkDay => "work_day", Potluck => "potluck", AdoptionEvent => "adoption_event",
    Market => "market", Talk => "talk", Meeting => "meeting", Other => "other",
});

string_enum!(EventStatus, "status" { Scheduled => "scheduled", Cancelled => "cancelled" });
string_enum!(Availability, "availability" {
    Attending => "attending", Maybe => "maybe", Unavailable => "unavailable",
});

// The working groups the team chose (Discord roles and Linear labels of the
// same names), plus the cat club, whose volunteers are their own crew.
string_enum!(WorkingGroup, "working_group" {
    GrantsBusiness => "grants_business", FarmLabor => "farm_labor", SocialMedia => "social_media",
    CatClub => "cat_club", Other => "other",
});

impl WorkingGroup {
    pub fn label(self) -> &'static str {
        match self {
            WorkingGroup::GrantsBusiness => "Grants & Business",
            WorkingGroup::FarmLabor => "Farm & Labor",
            WorkingGroup::SocialMedia => "Social Media",
            WorkingGroup::CatClub => "Kiki's Cat Club",
            WorkingGroup::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityEvent {
    pub id: Uuid,
    pub title: String,
    pub kind: EventKind,
    pub held_on: Date,
    pub time_note: String,
    pub place: String,
    pub description: String,
    pub attendance: Option<i32>,
    pub status: EventStatus,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewCommunityEvent {
    pub title: String,
    pub kind: EventKind,
    pub held_on: Date,
    #[serde(default)]
    pub time_note: String,
    #[serde(default)]
    pub place: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EventEdit {
    pub title: String,
    pub kind: EventKind,
    pub held_on: Date,
    #[serde(default)]
    pub time_note: String,
    #[serde(default)]
    pub place: String,
    #[serde(default)]
    pub description: String,
}

impl EventEdit {
    pub fn validated(self) -> Result<Self, ValidationError> {
        let e = NewCommunityEvent {
            title: self.title,
            kind: self.kind,
            held_on: self.held_on,
            time_note: self.time_note,
            place: self.place,
            description: self.description,
        }
        .validated()?;
        Ok(Self {
            title: e.title,
            kind: e.kind,
            held_on: e.held_on,
            time_note: e.time_note,
            place: e.place,
            description: e.description,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventRsvp {
    pub volunteer_id: Uuid,
    pub volunteer_name: String,
    pub availability: Availability,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventInventory {
    pub item_id: Uuid,
    pub name: String,
    pub unit: String,
    pub quantity: Decimal,
    pub on_hand: Decimal,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventEquipment {
    pub equipment_id: Uuid,
    pub name: String,
    pub available: bool,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventPlan {
    pub event: CommunityEvent,
    pub rsvps: Vec<EventRsvp>,
    pub tasks: Vec<crate::Task>,
    pub inventory: Vec<EventInventory>,
    pub equipment: Vec<EventEquipment>,
}

impl NewCommunityEvent {
    pub fn validated(self) -> Result<Self, ValidationError> {
        Ok(Self {
            title: non_blank("title", &self.title)?,
            time_note: self.time_note.trim().to_owned(),
            place: self.place.trim().to_owned(),
            description: self.description.trim().to_owned(),
            ..self
        })
    }
}

/// Longest name a volunteer can go by, as the gate and the database allow.
pub const MAX_VOLUNTEER_NAME: usize = 80;

/// Someone who gives their time. Shifts and gate visits point here, so
/// one person's hours add up however their name was typed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Volunteer {
    pub id: Uuid,
    pub name: String,
    pub email: Option<String>,
    pub phone: String,
    pub active: bool,
    /// Other names that find this person (from merges), lower-cased.
    pub other_names: Vec<String>,
}

/// A volunteer's details as a steward edits them.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct VolunteerEdit {
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub phone: String,
    pub active: bool,
}

impl VolunteerEdit {
    pub fn validated(self) -> Result<Self, ValidationError> {
        let email = match self.email.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(e) => Some(crate::contacts::normalise_email(e)?),
        };
        let phone = self.phone.trim().to_owned();
        if phone.chars().count() > 40 {
            return Err(ValidationError::Malformed("phone (40 characters at most)"));
        }
        Ok(Self { name: volunteer_name(&self.name)?, email, phone, ..self })
    }
}

/// A volunteer's name: trimmed, not blank, and short enough to store.
pub fn volunteer_name(s: &str) -> Result<String, ValidationError> {
    let t = non_blank("person_name", s)?;
    if t.chars().count() > MAX_VOLUNTEER_NAME {
        return Err(ValidationError::Malformed("name (80 characters at most)"));
    }
    Ok(t)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolunteerShift {
    pub id: Uuid,
    /// Who, stably; `person_name` is how the name was written that day.
    pub volunteer_id: Uuid,
    pub person_name: String,
    pub worked_on: Date,
    pub hours: Decimal,
    pub working_group: WorkingGroup,
    pub event_id: Option<Uuid>,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewVolunteerShift {
    pub person_name: String,
    pub worked_on: Date,
    pub hours: Decimal,
    pub working_group: WorkingGroup,
    #[serde(default)]
    pub event_id: Option<Uuid>,
    #[serde(default)]
    pub notes: String,
}

pub const MAX_SHIFT_HOURS: Decimal = Decimal::from_parts(24, 0, 0, false, 0);

impl NewVolunteerShift {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.hours <= Decimal::ZERO {
            return Err(ValidationError::NotPositive("hours"));
        }
        if self.hours > MAX_SHIFT_HOURS {
            return Err(ValidationError::Malformed("hours (more than a day)"));
        }
        Ok(Self { person_name: volunteer_name(&self.person_name)?, notes: self.notes.trim().to_owned(), ..self })
    }
}

/// Volunteer hours for one working group over a period.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HoursByGroup {
    pub working_group: WorkingGroup,
    pub hours: Decimal,
    pub people: i64,
}

/// A sign-in at the gate. Open while `out_at` is `None`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Visit {
    pub id: Uuid,
    pub volunteer_id: Uuid,
    pub person_name: String,
    pub working_group: WorkingGroup,
    #[serde(with = "time::serde::rfc3339")]
    pub in_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub out_at: Option<time::OffsetDateTime>,
    pub shift_id: Option<Uuid>,
}

/// Longer than this between checking in and out is a forgotten check-out,
/// not a shift: the visit closes but no hours are recorded for it.
pub const MAX_GATE_SHIFT: time::Duration = time::Duration::hours(12);

/// The hours a gate visit counts for: rounded to the nearest quarter hour,
/// at least a quarter, and `None` when it's too long to be real (see
/// [`MAX_GATE_SHIFT`]).
pub fn gate_hours(stayed: time::Duration) -> Option<Decimal> {
    if stayed > MAX_GATE_SHIFT || stayed.is_negative() {
        return None;
    }
    let quarters = (stayed.whole_seconds() + 450) / 900; // nearest 15 minutes
    Some(Decimal::new(quarters.max(1) * 25, 2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn gate_visits_count_to_the_nearest_quarter_hour() {
        use time::Duration;
        let h = |d: Duration| gate_hours(d).map(|x| x.to_string());
        assert_eq!(h(Duration::minutes(2)).as_deref(), Some("0.25"), "a visit counts for at least a quarter");
        assert_eq!(h(Duration::minutes(52)).as_deref(), Some("0.75"));
        assert_eq!(h(Duration::minutes(53)).as_deref(), Some("1.00"), "7.5 minutes rounds up");
        assert_eq!(h(Duration::minutes(3 * 60 + 10)).as_deref(), Some("3.25"));
        assert_eq!(h(Duration::hours(12)).as_deref(), Some("12.00"));
        assert_eq!(h(Duration::hours(12) + Duration::SECOND), None, "a forgotten check-out is not a shift");
        assert_eq!(h(-Duration::MINUTE), None);
        let sql = include_str!("../../../migrations/0035_gate_visits.sql");
        crate::tests::assert_check_list(sql, "working_group", WorkingGroup::ALL, WorkingGroup::as_str);
    }

    #[test]
    fn enum_strings_match_migration() {
        let sql = include_str!("../../../migrations/0008_community.sql");
        crate::tests::assert_check_list(sql, "kind", EventKind::ALL, EventKind::as_str);
        crate::tests::assert_check_list(sql, "working_group", WorkingGroup::ALL, WorkingGroup::as_str);
    }

    /// The labels are the names used for the Discord roles and Linear labels.
    #[test]
    fn working_group_labels_match_discord_and_linear() {
        let discord = include_str!("../../../discord/server.toml");
        let linear = include_str!("../../../linear/workspace.toml");
        for g in [WorkingGroup::GrantsBusiness, WorkingGroup::FarmLabor, WorkingGroup::SocialMedia] {
            let quoted = format!("name = \"{}\"", g.label());
            assert!(discord.contains(&quoted), "Discord has no role {quoted}");
            assert!(linear.contains(&quoted), "Linear has no label {quoted}");
        }
    }

    #[test]
    fn shifts_are_between_zero_and_a_day() {
        let s = |h: i64| NewVolunteerShift {
            person_name: "Ricardo".into(),
            worked_on: date!(2026 - 09 - 26),
            hours: Decimal::from(h),
            working_group: WorkingGroup::FarmLabor,
            event_id: None,
            notes: String::new(),
        };
        assert_eq!(s(0).validated(), Err(ValidationError::NotPositive("hours")));
        assert!(s(25).validated().is_err());
        assert!(s(24).validated().is_ok());
        let mut blank = s(3);
        blank.person_name = " ".into();
        assert_eq!(blank.validated(), Err(ValidationError::Blank("person_name")));
        let mut long = s(3);
        long.person_name = "x".repeat(MAX_VOLUNTEER_NAME + 1);
        assert!(long.validated().is_err(), "longer than a volunteer name can be");
    }

    #[test]
    fn volunteer_details_are_trimmed_and_checked() {
        let e = |email: Option<&str>, phone: &str| VolunteerEdit {
            name: "  Sam R. ".into(),
            email: email.map(str::to_owned),
            phone: phone.into(),
            active: true,
        };
        let ok = e(Some(" Sam@Example.org "), " 555-0100 ").validated().expect("ok");
        assert_eq!(
            (ok.name.as_str(), ok.email.as_deref(), ok.phone.as_str()),
            ("Sam R.", Some("sam@example.org"), "555-0100")
        );
        assert_eq!(e(Some("  "), "").validated().expect("blank email").email, None);
        assert!(e(Some("nope"), "").validated().is_err());
        assert!(e(None, &"5".repeat(41)).validated().is_err());
        assert_eq!(
            VolunteerEdit { name: " ".into(), ..e(None, "") }.validated(),
            Err(ValidationError::Blank("person_name"))
        );
    }
}

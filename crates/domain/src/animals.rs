//! Animals: the flock, any goats, and Kiki's Cat Club's cats.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::Date;
use uuid::Uuid;

use crate::{ValidationError, non_blank};

string_enum!(Species, "species" {
    Chicken => "chicken", Duck => "duck", Goat => "goat", Cat => "cat", Other => "other",
});

string_enum!(Sex, "sex" {
    Female => "female", Male => "male", Unknown => "unknown",
});

string_enum!(AnimalStatus, "status" {
    Resident => "resident", InCare => "in_care", Fostered => "fostered", Adoptable => "adoptable",
    Adopted => "adopted", ReturnedToColony => "returned_to_colony", Transferred => "transferred",
    Deceased => "deceased",
});

string_enum!(AnimalOrigin, "origin" {
    BornHere => "born_here", Purchased => "purchased", Rescued => "rescued", Surrendered => "surrendered",
    Stray => "stray", Trapped => "trapped", TransferredIn => "transferred_in", Other => "other",
});

string_enum!(CareKind, "kind" {
    Vaccine => "vaccine", SpayNeuter => "spay_neuter", Deworm => "deworm", Flea => "flea",
    VetVisit => "vet_visit", Medication => "medication", Microchip => "microchip",     Weight => "weight", Feeding => "feeding", HealthNote => "health_note", Mortality => "mortality",
    FlockCount => "flock_count", Hoof => "hoof", Enclosure => "enclosure", Other => "other",
});

string_enum!(PlacementKind, "kind" {
    Foster => "foster", Adoption => "adoption", ReturnToColony => "return_to_colony", Transfer => "transfer",
});

string_enum!(
    /// Whose contact details come back when a vet or shelter scans the chip.
    ChipRegistrant, "chip_registered_to" {
    Nobody => "nobody", Us => "us", Adopter => "adopter", Other => "other",
});

/// A microchip number as a scanner or a vet's paperwork gives it, in the
/// one form it's stored and looked up by: separators gone, upper case, no
/// "AVID" prefix. ISO chips (134.2 kHz, the standard now) are 15 digits;
/// older 125 kHz chips (AVID, FDX-A) are 9 or 10 characters. Blank stays
/// blank (no chip).
pub fn normalise_chip(s: &str) -> Result<String, ValidationError> {
    let c: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '-' | '.' | '*' | '_'))
        .collect::<String>()
        .to_ascii_uppercase();
    let c = c.strip_prefix("AVID").unwrap_or(&c).to_owned();
    let iso = c.len() == 15 && c.bytes().all(|b| b.is_ascii_digit());
    let older = (9..=10).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_hexdigit());
    if c.is_empty() || iso || older {
        Ok(c)
    } else {
        Err(ValidationError::Malformed("microchip (15 digits, or 9 or 10 characters for an older chip)"))
    }
}

/// How an animal is identified, set on its page.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Identity {
    #[serde(default)]
    pub microchip: String,
    #[serde(default)]
    pub chip_registry: String,
    #[serde(default = "nobody")]
    pub chip_registered_to: ChipRegistrant,
    #[serde(default)]
    pub band: String,
}

fn nobody() -> ChipRegistrant {
    ChipRegistrant::Nobody
}

impl Identity {
    pub fn validated(self) -> Result<Self, ValidationError> {
        let microchip = normalise_chip(&self.microchip)?;
        if microchip.is_empty() && self.chip_registered_to != ChipRegistrant::Nobody {
            return Err(ValidationError::Malformed("registered to (there's no chip number)"));
        }
        let chip_registry = self.chip_registry.trim().to_owned();
        let band = self.band.trim().to_owned();
        if chip_registry.chars().count() > 80 || band.chars().count() > 40 {
            return Err(ValidationError::Malformed("registry or band (too long)"));
        }
        Ok(Self { microchip, chip_registry, band, ..self })
    }
}

impl Species {
    /// Flocks may be counted as one record; cats and goats never are,
    /// because care and placement happen to an individual.
    pub fn may_be_counted_as_a_group(self) -> bool {
        matches!(self, Species::Chicken | Species::Duck | Species::Other)
    }
}

impl AnimalStatus {
    /// Still ours to look after (as opposed to placed, gone or dead).
    pub fn in_our_care(self) -> bool {
        matches!(self, AnimalStatus::Resident | AnimalStatus::InCare | AnimalStatus::Adoptable)
    }
}

impl PlacementKind {
    /// The status an animal takes on when this placement starts.
    pub fn resulting_status(self) -> AnimalStatus {
        match self {
            PlacementKind::Foster => AnimalStatus::Fostered,
            PlacementKind::Adoption => AnimalStatus::Adopted,
            PlacementKind::ReturnToColony => AnimalStatus::ReturnedToColony,
            PlacementKind::Transfer => AnimalStatus::Transferred,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Animal {
    pub id: Uuid,
    pub species: Species,
    pub name: String,
    pub head_count: i32,
    pub sex: Sex,
    pub status: AnimalStatus,
    pub origin: AnimalOrigin,
    pub arrived_on: Date,
    pub born_on: Option<Date>,
    pub microchip: String,
    pub notes: String,
    #[serde(default)]
    pub chip_registry: String,
    #[serde(default = "nobody")]
    pub chip_registered_to: ChipRegistrant,
    #[serde(default)]
    pub band: String,
    /// The coop, pen or area it lives in.
    #[serde(default)]
    pub home_location_id: Option<Uuid>,
    /// The community cat colony it came from (TNR).
    #[serde(default)]
    pub colony_id: Option<Uuid>,
    #[serde(default)]
    pub ear_tipped: bool,
    /// What the public adoption page says about it.
    #[serde(default)]
    pub adoption_blurb: String,
}

/// Care that comes round again: every `every_days` from `next_due_on`,
/// until `ends_on` if there is one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CareSchedule {
    pub id: Uuid,
    pub animal_id: Uuid,
    pub animal_name: String,
    /// Where the animal is: its status, so a fostered cat's dose reads as
    /// "with the foster".
    pub animal_status: AnimalStatus,
    pub kind: CareKind,
    pub details: String,
    pub every_days: i32,
    pub next_due_on: Date,
    pub ends_on: Option<Date>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewCareSchedule {
    pub kind: CareKind,
    #[serde(default)]
    pub details: String,
    pub every_days: i32,
    pub first_due_on: Date,
    #[serde(default)]
    pub ends_on: Option<Date>,
}

impl NewCareSchedule {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if !(1..=366).contains(&self.every_days) {
            return Err(ValidationError::Malformed("every (1 to 366 days)"));
        }
        if self.ends_on.is_some_and(|e| e < self.first_due_on) {
            return Err(ValidationError::Malformed("ends on (before the first time it's due)"));
        }
        let details = self.details.trim().to_owned();
        if details.chars().count() > 200 {
            return Err(ValidationError::Malformed("details (200 characters at most)"));
        }
        Ok(Self { details, ..self })
    }
}

/// When a schedule is next due after care given on `given_on`, or `None`
/// when that runs past its end (the course is done).
pub fn next_due(given_on: Date, every_days: i32, ends_on: Option<Date>) -> Option<Date> {
    let next = given_on.checked_add(time::Duration::days(i64::from(every_days)))?;
    match ends_on {
        Some(end) if next > end => None,
        _ => Some(next),
    }
}

/// A community cat colony: where a group of outdoor cats lives and who
/// feeds them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Colony {
    pub id: Uuid,
    pub name: String,
    /// Coarse on purpose: a block or a landmark, not an address.
    pub area: String,
    pub caretaker_name: String,
    /// Shown only to people who can change animals.
    pub caretaker_contact: String,
    pub estimated_size: Option<i32>,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewColony {
    pub name: String,
    #[serde(default)]
    pub area: String,
    #[serde(default)]
    pub caretaker_name: String,
    #[serde(default)]
    pub caretaker_contact: String,
    #[serde(default)]
    pub estimated_size: Option<i32>,
    #[serde(default)]
    pub notes: String,
}

impl NewColony {
    pub fn validated(self) -> Result<Self, ValidationError> {
        let limit = |field, s: &str, max: usize| {
            let t = s.trim().to_owned();
            if t.chars().count() > max { Err(ValidationError::Malformed(field)) } else { Ok(t) }
        };
        if self.estimated_size.is_some_and(|n| n < 0) {
            return Err(ValidationError::Negative("estimated size"));
        }
        let name = non_blank("name", &self.name)?;
        Ok(Self {
            name: limit("name (80 characters at most)", &name, 80)?,
            area: limit("area (200 characters at most)", &self.area, 200)?,
            caretaker_name: limit("caretaker (80 characters at most)", &self.caretaker_name, 80)?,
            caretaker_contact: limit("caretaker contact (120 characters at most)", &self.caretaker_contact, 120)?,
            notes: limit("notes (2000 characters at most)", &self.notes, 2000)?,
            ..self
        })
    }
}

/// How far TNR has got in a colony.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ColonyProgress {
    pub colony: Colony,
    /// Cats from this colony recorded here.
    pub recorded: i64,
    /// Of those, ear-tipped or with a spay/neuter on record.
    pub fixed: i64,
}

impl ColonyProgress {
    /// Cats believed still unfixed: the estimate less those fixed, when
    /// there's an estimate.
    pub fn left_to_fix(&self) -> Option<i64> {
        self.colony.estimated_size.map(|n| (i64::from(n) - self.fixed).max(0))
    }
}

/// A place animals live, against what it holds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Housing {
    pub location_id: Uuid,
    pub name: String,
    pub capacity: Option<i32>,
    /// Animals living there and still in our care, flocks by head.
    pub head: i64,
}

impl Housing {
    pub fn over_capacity(&self) -> bool {
        self.capacity.is_some_and(|c| self.head > i64::from(c))
    }

    /// Room left, or how many too many (negative).
    pub fn room(&self) -> Option<i64> {
        self.capacity.map(|c| i64::from(c) - self.head)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CareEntry {
    pub id: Uuid,
    pub animal_id: Uuid,
    pub occurred_on: Date,
    pub kind: CareKind,
    pub details: String,
    pub provider: String,
    pub cost: Option<Decimal>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub id: Uuid,
    pub animal_id: Uuid,
    pub kind: PlacementKind,
    pub person_name: String,
    pub started_on: Date,
    pub fee: Option<Decimal>,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewAnimal {
    pub species: Species,
    pub name: String,
    #[serde(default = "one")]
    pub head_count: i32,
    #[serde(default = "unknown_sex")]
    pub sex: Sex,
    pub status: AnimalStatus,
    pub origin: AnimalOrigin,
    pub arrived_on: Date,
    #[serde(default)]
    pub born_on: Option<Date>,
    #[serde(default)]
    pub microchip: String,
    #[serde(default)]
    pub notes: String,
}

fn one() -> i32 {
    1
}

fn unknown_sex() -> Sex {
    Sex::Unknown
}

impl NewAnimal {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.head_count < 1 {
            return Err(ValidationError::NotPositive("head_count"));
        }
        if self.head_count > 1 && !self.species.may_be_counted_as_a_group() {
            return Err(ValidationError::Malformed("head_count (cats and goats are recorded one at a time)"));
        }
        if let Some(born) = self.born_on
            && born > self.arrived_on
            && self.origin != AnimalOrigin::BornHere
        {
            return Err(ValidationError::Malformed("born_on (after the arrival date)"));
        }
        Ok(Self {
            name: non_blank("name", &self.name)?,
            microchip: normalise_chip(&self.microchip)?,
            notes: self.notes.trim().to_owned(),
            ..self
        })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewCareEntry {
    pub occurred_on: Date,
    pub kind: CareKind,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub cost: Option<Decimal>,
}

impl NewCareEntry {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.cost.is_some_and(|c| c < Decimal::ZERO) {
            return Err(ValidationError::Negative("cost"));
        }
        if self.kind == CareKind::Other && self.details.trim().is_empty() {
            return Err(ValidationError::Blank("details"));
        }
        Ok(Self { details: self.details.trim().to_owned(), provider: self.provider.trim().to_owned(), ..self })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewPlacement {
    pub kind: PlacementKind,
    #[serde(default)]
    pub person_name: String,
    pub started_on: Date,
    #[serde(default)]
    pub fee: Option<Decimal>,
    #[serde(default)]
    pub notes: String,
}

impl NewPlacement {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.fee.is_some() && self.kind != PlacementKind::Adoption {
            return Err(ValidationError::Malformed("fee (only adoptions carry a fee)"));
        }
        if self.fee.is_some_and(|f| f < Decimal::ZERO) {
            return Err(ValidationError::Negative("fee"));
        }
        let person_name = match self.kind {
            PlacementKind::ReturnToColony => self.person_name.trim().to_owned(),
            _ => non_blank("person_name", &self.person_name)?,
        };
        Ok(Self { person_name, notes: self.notes.trim().to_owned(), ..self })
    }
}

/// Kiki's Cat Club in numbers, for the website, grant reports and PetSmart
/// Charities. Counted from placements, so an adoption that is later
/// returned still counts as the adoption event it was.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CatClubNumbers {
    pub adoptions: i64,
    pub returned_to_colony: i64,
    pub spay_neuters: i64,
    pub in_our_care: i64,
    pub fostered_now: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn animal(species: Species, head_count: i32) -> NewAnimal {
        NewAnimal {
            species,
            name: " Kiki ".into(),
            head_count,
            sex: Sex::Female,
            status: AnimalStatus::InCare,
            origin: AnimalOrigin::Trapped,
            arrived_on: date!(2026 - 09 - 01),
            born_on: None,
            microchip: String::new(),
            notes: String::new(),
        }
    }

    #[test]
    fn enum_strings_match_migration() {
        let sql = include_str!("../../../migrations/0006_animals.sql");
        crate::tests::assert_check_list(sql, "species", Species::ALL, Species::as_str);
        crate::tests::assert_check_list(sql, "sex", Sex::ALL, Sex::as_str);
        crate::tests::assert_check_list(sql, "status", AnimalStatus::ALL, AnimalStatus::as_str);
        crate::tests::assert_check_list(sql, "origin", AnimalOrigin::ALL, AnimalOrigin::as_str);
        let care = include_str!("../../../migrations/0021_farm_ops.sql");
        let care = &care[care.find("animal_care_kind_check").expect("care kinds")..];
        crate::tests::assert_check_list(care, "kind", CareKind::ALL, CareKind::as_str);
        let placements = &sql[sql.find("create table homestead.animal_placements").expect("placements")..];
        crate::tests::assert_check_list(placements, "kind", PlacementKind::ALL, PlacementKind::as_str);
    }

    #[test]
    fn chips_are_stored_in_one_form_whatever_the_scanner_typed() {
        assert_eq!(normalise_chip(" 985-112 003 456 789 ").as_deref(), Ok("985112003456789"));
        assert_eq!(normalise_chip("AVID*012*345*678").as_deref(), Ok("012345678"));
        assert_eq!(normalise_chip("0a12-3456bc").as_deref(), Ok("0A123456BC"), "FDX-A is hex");
        assert_eq!(normalise_chip("  ").as_deref(), Ok(""), "no chip");
        for bad in ["98511200345678", "9851120034567890", "985112003456a89", "hello", "12345678"] {
            assert!(normalise_chip(bad).is_err(), "{bad}");
        }
        let mut a = animal(Species::Cat, 1);
        a.microchip = "985 112 003 456 789".into();
        assert_eq!(a.validated().expect("ok").microchip, "985112003456789");
        let sql = include_str!("../../../migrations/0036_animal_identity.sql");
        crate::tests::assert_check_list(sql, "chip_registered_to", ChipRegistrant::ALL, ChipRegistrant::as_str);
    }

    #[test]
    fn a_colony_is_named_bounded_and_counts_down_to_all_fixed() {
        let c = NewColony {
            name: " Laurel Canyon lot ".into(),
            area: "behind the laundromat".into(),
            caretaker_name: "Marisol".into(),
            caretaker_contact: "555-0100".into(),
            estimated_size: Some(9),
            notes: String::new(),
        };
        assert_eq!(c.clone().validated().expect("ok").name, "Laurel Canyon lot");
        assert!(NewColony { name: " ".into(), ..c.clone() }.validated().is_err());
        assert!(NewColony { estimated_size: Some(-1), ..c.clone() }.validated().is_err());
        assert!(NewColony { area: "x".repeat(201), ..c.clone() }.validated().is_err());
        let v = c.validated().expect("ok");
        let colony = Colony {
            id: Uuid::nil(),
            name: v.name,
            area: v.area,
            caretaker_name: v.caretaker_name,
            caretaker_contact: v.caretaker_contact,
            estimated_size: v.estimated_size,
            notes: v.notes,
        };
        let p = |fixed| ColonyProgress { colony: colony.clone(), recorded: fixed, fixed };
        assert_eq!(p(4).left_to_fix(), Some(5));
        assert_eq!(p(12).left_to_fix(), Some(0), "never below zero when the estimate was low");
    }

    #[test]
    fn a_course_of_care_moves_on_from_each_dose_and_ends() {
        let end = Some(date!(2026 - 10 - 10));
        assert_eq!(next_due(date!(2026 - 10 - 01), 1, end), Some(date!(2026 - 10 - 02)));
        assert_eq!(next_due(date!(2026 - 10 - 10), 1, end), None, "the last dose ends the course");
        assert_eq!(next_due(date!(2026 - 10 - 01), 21, None), Some(date!(2026 - 10 - 22)), "a booster");
        let s = |every, ends_on| NewCareSchedule {
            kind: CareKind::Medication,
            details: " Clavamox 0.5 ml ".into(),
            every_days: every,
            first_due_on: date!(2026 - 10 - 01),
            ends_on,
        };
        assert_eq!(s(1, end).validated().expect("ok").details, "Clavamox 0.5 ml");
        assert!(s(0, None).validated().is_err());
        assert!(s(1, Some(date!(2026 - 09 - 30))).validated().is_err());
        let sql = include_str!("../../../migrations/0039_care_schedules.sql");
        crate::tests::assert_check_list(sql, "kind", CareKind::ALL, CareKind::as_str);
    }

    #[test]
    fn housing_is_over_capacity_only_past_its_limit() {
        let h = |capacity, head| Housing { location_id: Uuid::nil(), name: "Coop".into(), capacity, head };
        assert!(h(Some(18), 26).over_capacity());
        assert_eq!(h(Some(18), 26).room(), Some(-8));
        assert!(!h(Some(18), 18).over_capacity(), "full is not over");
        assert!(!h(None, 99).over_capacity(), "no capacity set, no judgement");
    }

    #[test]
    fn a_registration_needs_a_chip() {
        let id = |chip: &str, to| Identity {
            microchip: chip.into(),
            chip_registry: " Found.org ".into(),
            chip_registered_to: to,
            band: " blue ".into(),
        };
        assert!(id("", ChipRegistrant::Us).validated().is_err());
        let ok = id("985112003456789", ChipRegistrant::Us).validated().expect("ok");
        assert_eq!((ok.chip_registry.as_str(), ok.band.as_str()), ("Found.org", "blue"));
        assert!(id("", ChipRegistrant::Nobody).validated().is_ok(), "a band alone is fine");
    }

    #[test]
    fn only_flocks_may_be_counted_as_a_group() {
        assert!(animal(Species::Chicken, 26).validated().is_ok());
        for s in [Species::Cat, Species::Goat] {
            assert!(matches!(animal(s, 2).validated(), Err(ValidationError::Malformed(_))), "{s}");
            assert!(animal(s, 1).validated().is_ok(), "{s}");
        }
        assert_eq!(animal(Species::Chicken, 0).validated(), Err(ValidationError::NotPositive("head_count")));
        assert_eq!(animal(Species::Cat, 1).validated().expect("ok").name, "Kiki", "name trimmed");
    }

    #[test]
    fn birth_after_arrival_only_for_animals_born_here() {
        let mut a = animal(Species::Goat, 1);
        a.born_on = Some(date!(2026 - 09 - 02));
        assert!(a.clone().validated().is_err());
        a.origin = AnimalOrigin::BornHere;
        assert!(a.validated().is_ok());
    }

    #[test]
    fn placements_need_a_person_except_back_to_the_colony() {
        let p = |kind, person: &str, fee| NewPlacement {
            kind,
            person_name: person.into(),
            started_on: date!(2026 - 09 - 27),
            fee,
            notes: String::new(),
        };
        assert_eq!(p(PlacementKind::Adoption, " ", None).validated(), Err(ValidationError::Blank("person_name")));
        assert!(p(PlacementKind::ReturnToColony, "", None).validated().is_ok());
        assert!(p(PlacementKind::Adoption, "Randy's friend", Some(Decimal::from(75))).validated().is_ok());
        assert!(p(PlacementKind::Foster, "Ava", Some(Decimal::from(75))).validated().is_err(), "fosters pay nothing");
        assert!(p(PlacementKind::Adoption, "A", Some(Decimal::NEGATIVE_ONE)).validated().is_err());
    }

    #[test]
    fn each_placement_moves_the_animal_out_of_our_care() {
        for k in PlacementKind::ALL {
            assert!(!k.resulting_status().in_our_care(), "{k}");
        }
    }

    #[test]
    fn care_cost_cannot_be_negative_and_other_needs_details() {
        let c = |kind, details: &str, cost| NewCareEntry {
            occurred_on: date!(2026 - 09 - 10),
            kind,
            details: details.into(),
            provider: String::new(),
            cost,
        };
        assert!(c(CareKind::Vaccine, "", Some(Decimal::from(-1))).validated().is_err());
        assert_eq!(c(CareKind::Other, " ", None).validated(), Err(ValidationError::Blank("details")));
        assert!(c(CareKind::SpayNeuter, "", None).validated().is_ok());
    }
}

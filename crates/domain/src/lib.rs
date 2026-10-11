//! Sun Brewed Homestead farm-operations domain.
//!
//! Pure types and rules: no database, no network. Every enum's string form
//! here is the same literal the database CHECK constraints accept, and
//! `tests::enum_strings_match_migration` holds the two in lockstep.

pub mod event;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

pub use event::Event;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("{0} must not be blank")]
    Blank(&'static str),
    #[error("{field} has unknown value {value:?}")]
    UnknownValue { field: &'static str, value: String },
    #[error("{0} must be greater than zero")]
    NotPositive(&'static str),
    #[error("{0} must not be negative")]
    Negative(&'static str),
    #[error("expected harvest date is before the sowing date")]
    HarvestBeforeSowing,
    #[error("{0} is not valid")]
    Malformed(&'static str),
}

/// Declares a closed string enum whose `as_str` forms are the DB literals.
macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident, $field:literal { $($variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }
        }

        impl std::str::FromStr for $name {
            type Err = ValidationError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($s => Ok($name::$variant),)+
                    other => Err(ValidationError::UnknownValue { field: $field, value: other.to_owned() }),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

pub mod adoptions;
pub mod animals;
pub mod attachments;
pub mod auth;
pub mod community;
pub mod contacts;
pub mod crops;
pub mod deadlines;
pub mod digest;
pub mod equipment;
pub mod gifts;
pub mod harvests;
pub mod health;
pub mod incidents;
pub mod inventory;
pub mod map;
pub mod movements;
pub mod onboarding;
pub mod readings;
pub mod recurring;
pub mod reports;
pub mod search;
pub mod seeds;
pub mod soil;
pub mod stand;
pub mod tags;
pub mod water;

string_enum!(LocationKind, "kind" {
    Site => "site", Garden => "garden", Field => "field", Bed => "bed", Row => "row",
    Orchard => "orchard", Greenhouse => "greenhouse", AnimalArea => "animal_area",
    Pen => "pen", Coop => "coop", Compost => "compost", Stand => "stand",
    Storage => "storage", Other => "other",
});

string_enum!(PlantingStatus, "status" {
    Planned => "planned", Growing => "growing", Harvesting => "harvesting",
    Finished => "finished", Failed => "failed",
});

impl PlantingStatus {
    /// Finished or failed: no longer in the ground.
    pub fn is_over(self) -> bool {
        matches!(self, PlantingStatus::Finished | PlantingStatus::Failed)
    }
}

string_enum!(SowMethod, "method" {
    Direct => "direct", Transplant => "transplant",
});

string_enum!(HarvestUnit, "unit" {
    Lb => "lb", Oz => "oz", Kg => "kg", G => "g", Each => "each", Bunch => "bunch", Dozen => "dozen",
});

string_enum!(TaskPriority, "priority" {
    Low => "low", Normal => "normal", High => "high", Urgent => "urgent",
});

string_enum!(TaskStatus, "status" {
    Open => "open", Done => "done", Cancelled => "cancelled",
});

// ── Records (what the store returns) ────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub id: Uuid,
    pub name: String,
    pub kind: LocationKind,
    pub notes: String,
    #[serde(default)]
    pub parent_id: Option<Uuid>,
    #[serde(default = "default_true")]
    pub active: bool,
    #[serde(default)]
    pub area_sqft: Option<Decimal>,
    #[serde(default)]
    pub soil_notes: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

fn default_true() -> bool {
    true
}

fn direct_sow() -> SowMethod {
    SowMethod::Direct
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Planting {
    pub id: Uuid,
    pub location_id: Uuid,
    pub crop: String,
    pub variety: String,
    pub sown_on: Date,
    pub expected_harvest_on: Option<Date>,
    pub status: PlantingStatus,
    pub notes: String,
    /// The seed lot it was sown from, if recorded. Absent from events
    /// recorded before seed lots were tracked.
    #[serde(default)]
    pub seed_lot_id: Option<Uuid>,
    #[serde(default)]
    pub crop_plan_id: Option<Uuid>,
    #[serde(default)]
    pub family: String,
    #[serde(default = "direct_sow")]
    pub method: SowMethod,
    #[serde(default)]
    pub expected_yield: Option<Decimal>,
    #[serde(default)]
    pub expected_yield_unit: Option<HarvestUnit>,
    /// When it left the bed; only a finished or failed planting has one.
    #[serde(default)]
    pub ended_on: Option<Date>,
    #[serde(default)]
    pub transplant_on: Option<Date>,
    /// The end of its expected harvest window, which starts on `expected_harvest_on`.
    #[serde(default)]
    pub expected_harvest_until: Option<Date>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Harvest {
    pub id: Uuid,
    pub planting_id: Uuid,
    pub harvested_on: Date,
    pub quantity: Decimal,
    pub unit: HarvestUnit,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub title: String,
    pub details: String,
    pub location_id: Option<Uuid>,
    pub due_on: Option<Date>,
    pub priority: TaskPriority,
    pub status: TaskStatus,
    pub mirror_to_linear: bool,
    pub linear_issue_id: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub completed_at: Option<OffsetDateTime>,
    /// The recurring template that made this task, if any. Absent from
    /// events recorded before recurring tasks existed.
    #[serde(default)]
    pub template_id: Option<Uuid>,
    /// The person doing it (an `actors` id); `None` while anyone can take
    /// it. Absent from events recorded before tasks had assignees.
    #[serde(default)]
    pub assigned_to: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InventoryItem {
    pub id: Uuid,
    pub name: String,
    pub unit: String,
    pub reorder_level: Decimal,
    pub on_hand: Decimal,
}

impl InventoryItem {
    /// At or below the reorder level, and the level is actually set.
    pub fn is_low(&self) -> bool {
        self.reorder_level > Decimal::ZERO && self.on_hand <= self.reorder_level
    }
}

// ── Inputs (validated before they reach the store) ──────────────────────────

fn non_blank(field: &'static str, s: &str) -> Result<String, ValidationError> {
    let t = s.trim();
    if t.is_empty() { Err(ValidationError::Blank(field)) } else { Ok(t.to_owned()) }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewLocation {
    pub name: String,
    pub kind: LocationKind,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub parent_id: Option<Uuid>,
    #[serde(default = "default_true")]
    pub active: bool,
    #[serde(default)]
    pub area_sqft: Option<Decimal>,
    #[serde(default)]
    pub soil_notes: String,
}

impl NewLocation {
    pub fn named(name: impl Into<String>, kind: LocationKind) -> Self {
        Self {
            name: name.into(),
            kind,
            notes: String::new(),
            parent_id: None,
            active: true,
            area_sqft: None,
            soil_notes: String::new(),
        }
    }

    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.area_sqft.is_some_and(|a| a <= Decimal::ZERO) {
            return Err(ValidationError::NotPositive("area"));
        }
        Ok(Self {
            name: non_blank("name", &self.name)?,
            notes: self.notes.trim().to_owned(),
            soil_notes: self.soil_notes.trim().to_owned(),
            ..self
        })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewPlanting {
    pub location_id: Uuid,
    pub crop: String,
    #[serde(default)]
    pub variety: String,
    pub sown_on: Date,
    #[serde(default)]
    pub expected_harvest_on: Option<Date>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub seed_lot_id: Option<Uuid>,
    /// How much of the lot went in, in the lot's own unit.
    #[serde(default)]
    pub seed_quantity_used: Option<Decimal>,
    #[serde(default)]
    pub crop_plan_id: Option<Uuid>,
    #[serde(default)]
    pub family: String,
    #[serde(default = "direct_sow")]
    pub method: SowMethod,
    #[serde(default)]
    pub expected_yield: Option<Decimal>,
    #[serde(default)]
    pub expected_yield_unit: Option<HarvestUnit>,
    #[serde(default)]
    pub status: Option<PlantingStatus>,
}

impl NewPlanting {
    /// A direct sowing with no seed lot and no expected yield.
    pub fn at(location_id: Uuid, crop: impl Into<String>, sown_on: Date) -> Self {
        Self {
            location_id,
            crop: crop.into(),
            variety: String::new(),
            sown_on,
            expected_harvest_on: None,
            notes: String::new(),
            seed_lot_id: None,
            seed_quantity_used: None,
            crop_plan_id: None,
            family: String::new(),
            method: SowMethod::Direct,
            expected_yield: None,
            expected_yield_unit: None,
            status: None,
        }
    }

    pub fn validated(self) -> Result<Self, ValidationError> {
        if let Some(h) = self.expected_harvest_on
            && h < self.sown_on
        {
            return Err(ValidationError::HarvestBeforeSowing);
        }
        if self.seed_quantity_used.is_some() && self.seed_lot_id.is_none() {
            return Err(ValidationError::Blank("seed lot (seed used is counted against a lot)"));
        }
        if self.seed_quantity_used.is_some_and(|q| q <= Decimal::ZERO) {
            return Err(ValidationError::NotPositive("seed used"));
        }
        if self.expected_yield.is_some() != self.expected_yield_unit.is_some() {
            return Err(ValidationError::Malformed("expected yield (give both an amount and its unit, or neither)"));
        }
        if self.expected_yield.is_some_and(|q| q <= Decimal::ZERO) {
            return Err(ValidationError::NotPositive("expected yield"));
        }
        Ok(Self {
            crop: non_blank("crop", &self.crop)?,
            variety: self.variety.trim().to_owned(),
            notes: self.notes.trim().to_owned(),
            family: self.family.trim().to_owned(),
            ..self
        })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewHarvest {
    pub planting_id: Uuid,
    pub harvested_on: Date,
    pub quantity: Decimal,
    pub unit: HarvestUnit,
    #[serde(default)]
    pub notes: String,
}

impl NewHarvest {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.quantity <= Decimal::ZERO {
            return Err(ValidationError::NotPositive("quantity"));
        }
        Ok(Self { notes: self.notes.trim().to_owned(), ..self })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewTask {
    pub title: String,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub location_id: Option<Uuid>,
    #[serde(default)]
    pub due_on: Option<Date>,
    #[serde(default = "default_priority")]
    pub priority: TaskPriority,
    #[serde(default)]
    pub mirror_to_linear: bool,
    /// Who it's for; `None` leaves it for anyone to take.
    #[serde(default)]
    pub assigned_to: Option<Uuid>,
}

fn default_priority() -> TaskPriority {
    TaskPriority::Normal
}

impl NewTask {
    /// A normal-priority task with just a title.
    pub fn titled(title: &str) -> Self {
        NewTask {
            title: title.to_owned(),
            details: String::new(),
            location_id: None,
            due_on: None,
            priority: TaskPriority::Normal,
            mirror_to_linear: false,
            assigned_to: None,
        }
    }

    pub fn validated(self) -> Result<Self, ValidationError> {
        Ok(Self { title: non_blank("title", &self.title)?, details: self.details.trim().to_owned(), ..self })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewInventoryItem {
    pub name: String,
    pub unit: String,
    #[serde(default)]
    pub reorder_level: Decimal,
}

impl NewInventoryItem {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.reorder_level < Decimal::ZERO {
            return Err(ValidationError::Negative("reorder_level"));
        }
        Ok(Self { name: non_blank("name", &self.name)?, unit: non_blank("unit", &self.unit)?, ..self })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StockAdjustment {
    /// Positive to receive stock, negative to consume it.
    pub delta: Decimal,
    pub reason: String,
}

impl StockAdjustment {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.delta.is_zero() {
            return Err(ValidationError::NotPositive("|delta|"));
        }
        Ok(Self { reason: non_blank("reason", &self.reason)?, ..self })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use time::macros::date;

    /// A growing direct sowing with nothing else recorded.
    pub(crate) fn planting(crop: &str, sown_on: Date) -> Planting {
        Planting {
            id: Uuid::nil(),
            location_id: Uuid::nil(),
            crop: crop.into(),
            variety: String::new(),
            sown_on,
            expected_harvest_on: None,
            status: PlantingStatus::Growing,
            notes: String::new(),
            seed_lot_id: None,
            crop_plan_id: None,
            family: String::new(),
            method: SowMethod::Direct,
            expected_yield: None,
            expected_yield_unit: None,
            ended_on: None,
            transplant_on: None,
            expected_harvest_until: None,
        }
    }

    /// The DB CHECK constraints and these enums must accept exactly the same
    /// literals, or a value valid here is rejected on insert (or vice versa).
    /// Asserts the first `{column} in (…)` CHECK list in `sql` holds exactly
    /// the enum's literals.
    pub(crate) fn assert_check_list<T: Copy>(sql: &str, column: &str, all: &[T], s: fn(T) -> &'static str) {
        let start = sql.find(&format!("{column} in")).unwrap_or_else(|| panic!("no CHECK for {column}"));
        let rest = &sql[start..];
        let list = &rest[rest.find('(').expect("(") + 1..rest.find(')').expect(")")];
        let mut db: Vec<&str> = list.split(',').map(|x| x.trim().trim_matches('\'')).collect();
        let mut ours: Vec<&str> = all.iter().map(|v| s(*v)).collect();
        db.sort();
        ours.sort();
        assert_eq!(db, ours, "{column} literals diverge between enum and migration");
    }

    #[test]
    fn enum_strings_match_migration() {
        let sql = include_str!("../../../migrations/0001_core.sql");
        let places = include_str!("../../../migrations/0020_places.sql");
        assert_check_list(places, "kind", LocationKind::ALL, LocationKind::as_str);
        assert_check_list(sql, "unit", HarvestUnit::ALL, HarvestUnit::as_str);
        assert_check_list(sql, "priority", TaskPriority::ALL, TaskPriority::as_str);
        // `status` appears for plantings first, then tasks, then deliveries.
        let after_plantings = &sql[sql.find("create table homestead.plantings").expect("plantings")..];
        assert_check_list(after_plantings, "status", PlantingStatus::ALL, PlantingStatus::as_str);
        let after_tasks = &sql[sql.find("create table homestead.tasks").expect("tasks")..];
        assert_check_list(after_tasks, "status", TaskStatus::ALL, TaskStatus::as_str);
    }

    #[test]
    fn enum_round_trips_through_str_and_serde() {
        for k in LocationKind::ALL {
            assert_eq!(k.as_str().parse::<LocationKind>(), Ok(*k));
            assert_eq!(serde_json::to_value(k).expect("ser"), serde_json::json!(k.as_str()));
        }
        assert!(matches!("barn".parse::<LocationKind>(), Err(ValidationError::UnknownValue { .. })));
    }

    #[test]
    fn blank_names_rejected_and_whitespace_trimmed() {
        let bad = NewLocation { name: "  \t".into(), ..NewLocation::named("", LocationKind::Bed) };
        assert_eq!(bad.validated(), Err(ValidationError::Blank("name")));
        let ok =
            NewLocation { name: "  Bed 3 ".into(), notes: " x ".into(), ..NewLocation::named("", LocationKind::Bed) }
                .validated()
                .expect("valid");
        assert_eq!((ok.name.as_str(), ok.notes.as_str()), ("Bed 3", "x"));
        let area = NewLocation { area_sqft: Some(Decimal::ZERO), ..NewLocation::named("Bed", LocationKind::Bed) };
        assert_eq!(area.validated(), Err(ValidationError::NotPositive("area")));
    }

    #[test]
    fn planting_harvest_date_cannot_precede_sowing() {
        let p = |h| NewPlanting {
            expected_harvest_on: Some(h),
            ..NewPlanting::at(Uuid::nil(), "Tomato", date!(2026 - 03 - 10))
        };
        assert_eq!(p(date!(2026 - 03 - 09)).validated(), Err(ValidationError::HarvestBeforeSowing));
        assert!(p(date!(2026 - 03 - 10)).validated().is_ok(), "same-day is allowed");
        let seed = |lot: Option<Uuid>, used: i64| NewPlanting {
            seed_lot_id: lot,
            seed_quantity_used: Some(Decimal::from(used)),
            ..p(date!(2026 - 03 - 10))
        };
        assert!(seed(Some(Uuid::nil()), 12).validated().is_ok());
        assert!(seed(None, 12).validated().is_err(), "seed used needs its lot");
        assert!(seed(Some(Uuid::nil()), 0).validated().is_err());
    }

    #[test]
    fn harvest_quantity_must_be_positive() {
        let h = |q| NewHarvest {
            planting_id: Uuid::nil(),
            harvested_on: date!(2026 - 06 - 01),
            quantity: q,
            unit: HarvestUnit::Lb,
            notes: String::new(),
        };
        assert_eq!(h(Decimal::ZERO).validated(), Err(ValidationError::NotPositive("quantity")));
        assert_eq!(h(Decimal::NEGATIVE_ONE).validated(), Err(ValidationError::NotPositive("quantity")));
        assert!(h(Decimal::new(25, 1)).validated().is_ok());
    }

    #[test]
    fn low_stock_requires_a_reorder_level() {
        let item = |on_hand: i64, level: i64| InventoryItem {
            id: Uuid::nil(),
            name: "Feed".into(),
            unit: "bag".into(),
            reorder_level: Decimal::from(level),
            on_hand: Decimal::from(on_hand),
        };
        assert!(item(2, 2).is_low(), "at the level counts as low");
        assert!(!item(3, 2).is_low());
        assert!(!item(0, 0).is_low(), "no reorder level configured means never 'low'");
    }

    #[test]
    fn zero_stock_adjustment_rejected() {
        let adj = StockAdjustment { delta: Decimal::ZERO, reason: "count".into() };
        assert!(adj.validated().is_err());
    }
}

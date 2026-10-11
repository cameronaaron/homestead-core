//! Tools and machines, and the work done on them.
//!
//! When something is next due for service is read off its latest
//! maintenance record, not kept as a second copy on the equipment.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::Date;
use uuid::Uuid;

use crate::{ValidationError, non_blank};

string_enum!(EquipmentStatus, "status" {
    InService => "in_service", NeedsRepair => "needs_repair", OutOfService => "out_of_service", Retired => "retired",
});

string_enum!(Condition, "condition" { New => "new", Good => "good", Fair => "fair", Poor => "poor" });

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewEquipment {
    pub name: String,
    pub category: String,
    #[serde(default)]
    pub manufacturer: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub serial_number: String,
    #[serde(default)]
    pub purchased_on: Option<Date>,
    #[serde(default)]
    pub purchase_cost: Option<Decimal>,
    /// The purchase it came from, when that's recorded.
    #[serde(default)]
    pub purchase_id: Option<Uuid>,
    #[serde(default)]
    pub location_id: Option<Uuid>,
    #[serde(default = "good")]
    pub condition: Condition,
    #[serde(default)]
    pub notes: String,
}

fn good() -> Condition {
    Condition::Good
}

impl NewEquipment {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.purchase_cost.is_some_and(|c| c.is_sign_negative()) {
            return Err(ValidationError::Negative("purchase cost"));
        }
        Ok(Self {
            name: non_blank("name", &self.name)?,
            category: non_blank("category", &self.category)?.to_lowercase(),
            manufacturer: self.manufacturer.trim().to_owned(),
            model: self.model.trim().to_owned(),
            serial_number: self.serial_number.trim().to_owned(),
            notes: self.notes.trim().to_owned(),
            ..self
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Equipment {
    pub id: Uuid,
    pub name: String,
    pub category: String,
    pub manufacturer: String,
    pub model: String,
    pub serial_number: String,
    pub purchased_on: Option<Date>,
    pub purchase_cost: Option<Decimal>,
    pub purchase_id: Option<Uuid>,
    pub location_id: Option<Uuid>,
    pub location_name: Option<String>,
    pub condition: Condition,
    pub status: EquipmentStatus,
    pub notes: String,
    /// From the most recent maintenance record.
    pub last_serviced_on: Option<Date>,
    pub next_service_on: Option<Date>,
}

impl Equipment {
    pub fn service_due(&self, today: Date) -> bool {
        self.status != EquipmentStatus::Retired && self.next_service_on.is_some_and(|d| d <= today)
    }
}

/// Where it is, what shape it's in, whether it can be used.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EquipmentState {
    pub status: EquipmentStatus,
    pub condition: Condition,
    #[serde(default)]
    pub location_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewMaintenance {
    pub performed_on: Date,
    /// What was done.
    pub work: String,
    #[serde(default)]
    pub cost: Option<Decimal>,
    /// Who did it: a person's name, or leave blank and name a vendor.
    #[serde(default)]
    pub performed_by: String,
    #[serde(default)]
    pub vendor_id: Option<Uuid>,
    #[serde(default)]
    pub next_service_on: Option<Date>,
    #[serde(default)]
    pub notes: String,
    /// Also make a task due on `next_service_on`.
    #[serde(default)]
    pub remind: bool,
}

impl NewMaintenance {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.cost.is_some_and(|c| c.is_sign_negative()) {
            return Err(ValidationError::Negative("cost"));
        }
        if self.next_service_on.is_some_and(|n| n <= self.performed_on) {
            return Err(ValidationError::Malformed("next service (it must be after this one)"));
        }
        if self.remind && self.next_service_on.is_none() {
            return Err(ValidationError::Blank("next service date (needed for a reminder)"));
        }
        Ok(Self {
            work: non_blank("work done", &self.work)?,
            performed_by: self.performed_by.trim().to_owned(),
            notes: self.notes.trim().to_owned(),
            ..self
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Maintenance {
    pub id: Uuid,
    pub equipment_id: Uuid,
    pub performed_on: Date,
    pub work: String,
    pub cost: Option<Decimal>,
    pub performed_by: String,
    pub vendor_id: Option<Uuid>,
    pub vendor_name: Option<String>,
    pub next_service_on: Option<Date>,
    pub notes: String,
    pub task_id: Option<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn service() -> NewMaintenance {
        NewMaintenance {
            performed_on: date!(2026 - 09 - 01),
            work: "Changed oil, new spark plug".into(),
            cost: Some(Decimal::from(35)),
            performed_by: "Sam".into(),
            vendor_id: None,
            next_service_on: Some(date!(2027 - 03 - 01)),
            notes: String::new(),
            remind: true,
        }
    }

    #[test]
    fn maintenance_is_checked() {
        assert!(service().validated().is_ok());
        assert!(NewMaintenance { next_service_on: Some(date!(2026 - 09 - 01)), ..service() }.validated().is_err());
        assert!(NewMaintenance { next_service_on: None, ..service() }.validated().is_err(), "a reminder needs a date");
        assert!(NewMaintenance { next_service_on: None, remind: false, ..service() }.validated().is_ok());
        assert!(NewMaintenance { work: " ".into(), ..service() }.validated().is_err());
        assert!(NewMaintenance { cost: Some(Decimal::from(-1)), ..service() }.validated().is_err());
    }

    #[test]
    fn equipment_is_checked_and_service_due_ignores_retired_things() {
        let e = NewEquipment {
            name: " Tiller ".into(),
            category: "Power tools".into(),
            manufacturer: String::new(),
            model: String::new(),
            serial_number: String::new(),
            purchased_on: None,
            purchase_cost: None,
            purchase_id: None,
            location_id: None,
            condition: Condition::Good,
            notes: String::new(),
        };
        let ok = e.clone().validated().expect("ok");
        assert_eq!((ok.name.as_str(), ok.category.as_str()), ("Tiller", "power tools"));
        assert!(NewEquipment { purchase_cost: Some(Decimal::from(-5)), ..e.clone() }.validated().is_err());
        assert!(NewEquipment { category: "".into(), ..e }.validated().is_err());

        let thing = Equipment {
            id: Uuid::nil(),
            name: "Tiller".into(),
            category: "power tools".into(),
            manufacturer: String::new(),
            model: String::new(),
            serial_number: String::new(),
            purchased_on: None,
            purchase_cost: None,
            purchase_id: None,
            location_id: None,
            location_name: None,
            condition: Condition::Good,
            status: EquipmentStatus::InService,
            notes: String::new(),
            last_serviced_on: None,
            next_service_on: Some(date!(2026 - 09 - 28)),
        };
        assert!(thing.service_due(date!(2026 - 09 - 28)));
        assert!(!thing.service_due(date!(2026 - 09 - 27)));
        assert!(!Equipment { status: EquipmentStatus::Retired, ..thing }.service_due(date!(2027 - 01 - 01)));
    }

    #[test]
    fn literals_match_the_migration() {
        let sql = include_str!("../../../migrations/0017_equipment.sql");
        crate::tests::assert_check_list(sql, "status", EquipmentStatus::ALL, EquipmentStatus::as_str);
        crate::tests::assert_check_list(sql, "condition", Condition::ALL, Condition::as_str);
    }
}

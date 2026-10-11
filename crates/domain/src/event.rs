//! The integration event catalog.
//!
//! Every cross-system integration hangs off these events. Each is a
//! self-contained snapshot taken inside the transaction that caused it, so a
//! sink can act on it without reading the database back (by then the row may
//! have changed again).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::animals::Species;
use crate::{Harvest, InventoryItem, Planting, Task};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum Event {
    #[serde(rename = "task.created")]
    TaskCreated { task: Task },
    #[serde(rename = "task.completed")]
    TaskCompleted { task: Task },
    #[serde(rename = "planting.created")]
    PlantingCreated { planting: Planting, location_name: String },
    #[serde(rename = "harvest.logged")]
    HarvestLogged { harvest: Harvest, crop: String, location_name: String },
    /// Emitted when a stock movement takes an item from above its reorder
    /// level to at-or-below it — once per crossing, not on every movement.
    #[serde(rename = "inventory.low_stock")]
    LowStock { item: InventoryItem, previous_on_hand: Decimal },
    /// A legal release came back signed. Carries the signer's name only:
    /// their email stays in `release_requests`, out of chat and trackers.
    #[serde(rename = "release.signed")]
    ReleaseSigned { release_key: String, title: String, signer_name: String },
    #[serde(rename = "release.declined")]
    ReleaseDeclined { release_key: String, title: String, signer_name: String },
    /// Someone who joined through Discord signed the welcome agreement and is
    /// to hold `role`. Carries their Discord id (to grant it) and name only.
    #[serde(rename = "member.onboarded")]
    MemberOnboarded { discord_user_id: String, name: String, role: crate::onboarding::OnboardingRole },
    /// An animal went to its forever home. Names the animal only: the
    /// adopter stays in `animal_placements`, out of chat.
    #[serde(rename = "animal.adopted")]
    AnimalAdopted { animal_name: String, species: Species },
    /// Someone applied to adopt through the public page. Names the animal
    /// only (or none, for "any"): the applicant stays in
    /// `adoption_applications`, out of chat.
    #[serde(rename = "adoption.applied")]
    AdoptionApplied { animal_name: Option<String> },
    /// A safety incident was filed. Carries its kind and a reference only:
    /// who, what and where stay in `incidents`, out of chat.
    #[serde(rename = "incident.reported")]
    IncidentReported { reference: String, kind: crate::incidents::IncidentKind },
    /// Open tasks whose due date reached a new reminder stage, grouped by
    /// the Linear project they belong to ("" for none). Carries titles,
    /// dates and assignees only, like `task.created`.
    #[serde(rename = "task.deadlines")]
    TaskDeadlines { project: String, items: Vec<crate::deadlines::DueItem> },
    /// The morning summary: overdue, due today, high priority, low stock
    /// (see [`crate::digest`]). Titles, dates and assignees only.
    #[serde(rename = "digest.daily")]
    DailyDigest { digest: crate::digest::Digest },
}

impl Event {
    pub const KINDS: &'static [&'static str] = &[
        "task.created",
        "task.completed",
        "planting.created",
        "harvest.logged",
        "inventory.low_stock",
        "release.signed",
        "release.declined",
        "member.onboarded",
        "animal.adopted",
        "adoption.applied",
        "incident.reported",
        "task.deadlines",
        "digest.daily",
    ];

    /// What someone must already be allowed to see before this event is
    /// shown or pinged to them. Unknown kinds notify nobody.
    pub fn audience(kind: &str) -> Option<crate::auth::Capability> {
        use crate::auth::Capability as C;
        match kind {
            "task.created" | "task.completed" | "task.deadlines" => Some(C::TasksRead),
            // It lists low stock too; everyone who reads tasks reads stock.
            "digest.daily" => Some(C::TasksRead),
            "planting.created" | "harvest.logged" => Some(C::GardenRead),
            "inventory.low_stock" => Some(C::InventoryRead),
            "release.signed" | "release.declined" | "member.onboarded" => Some(C::ReleasesRead),
            "animal.adopted" => Some(C::AnimalsRead),
            "adoption.applied" => Some(C::AnimalsWrite),
            "incident.reported" => Some(C::IncidentsRead),
            _ => None,
        }
    }

    /// The label on the notification settings page.
    pub fn notice(kind: &str) -> &'static str {
        match kind {
            "task.created" => "A task is added",
            "task.completed" => "A task is finished",
            "planting.created" => "Something is planted",
            "harvest.logged" => "A harvest is logged",
            "inventory.low_stock" => "Stock falls to its reorder level",
            "release.signed" => "A release is signed",
            "release.declined" => "A release is declined",
            "member.onboarded" => "Someone signs the welcome agreement and joins",
            "animal.adopted" => "An animal is adopted",
            "adoption.applied" => "Someone applies to adopt",
            "incident.reported" => "A safety incident is reported",
            "task.deadlines" => "A deadline is two weeks, a week or a day away, or overdue",
            "digest.daily" => "The morning summary of what's overdue, due today and low",
            _ => "Other",
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Event::TaskCreated { .. } => "task.created",
            Event::TaskCompleted { .. } => "task.completed",
            Event::PlantingCreated { .. } => "planting.created",
            Event::HarvestLogged { .. } => "harvest.logged",
            Event::LowStock { .. } => "inventory.low_stock",
            Event::ReleaseSigned { .. } => "release.signed",
            Event::ReleaseDeclined { .. } => "release.declined",
            Event::MemberOnboarded { .. } => "member.onboarded",
            Event::AnimalAdopted { .. } => "animal.adopted",
            Event::AdoptionApplied { .. } => "adoption.applied",
            Event::IncidentReported { .. } => "incident.reported",
            Event::TaskDeadlines { .. } => "task.deadlines",
            Event::DailyDigest { .. } => "digest.daily",
        }
    }

    /// The `data` half of the serde representation; stored in `events.payload`
    /// alongside `kind` in its own column.
    pub fn payload(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).expect("Event serialises: all fields are plain data");
        v.get_mut("data").map(serde_json::Value::take).unwrap_or_default()
    }

    pub fn from_parts(kind: &str, payload: serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(serde_json::json!({ "kind": kind, "data": payload }))
    }

    /// One human line, shared by chat-style sinks and the ops page.
    pub fn summary(&self) -> String {
        match self {
            Event::TaskCreated { task } => {
                let due = task.due_on.map(|d| format!(" (due {d})")).unwrap_or_default();
                format!("New {} task: {}{due}", task.priority, task.title)
            }
            Event::TaskCompleted { task } => format!("Done: {}", task.title),
            Event::PlantingCreated { planting, location_name } => {
                let variety =
                    if planting.variety.is_empty() { String::new() } else { format!(" '{}'", planting.variety) };
                format!("Planted {}{variety} in {location_name} on {}", planting.crop, planting.sown_on)
            }
            Event::HarvestLogged { harvest, crop, location_name } => {
                format!("Harvested {} {} of {crop} from {location_name}", harvest.quantity.normalize(), harvest.unit)
            }
            Event::LowStock { item, .. } => format!(
                "Low stock: {} at {} {} (reorder at {})",
                item.name,
                item.on_hand.normalize(),
                item.unit,
                item.reorder_level.normalize()
            ),
            Event::ReleaseSigned { title, signer_name, .. } => format!("{signer_name} signed the {title}"),
            Event::ReleaseDeclined { title, signer_name, .. } => format!("{signer_name} declined the {title}"),
            Event::MemberOnboarded { name, role, .. } => {
                format!("{name} signed the welcome agreement and joined as {role}")
            }
            Event::AnimalAdopted { animal_name, species } => format!("{animal_name} the {species} was adopted!"),
            Event::AdoptionApplied { animal_name: Some(name) } => format!("New adoption application for {name}"),
            Event::AdoptionApplied { animal_name: None } => "New adoption application".to_owned(),
            Event::IncidentReported { reference, kind } => {
                format!("A {} was reported ({reference}). Details are in Homestead → Incidents.", kind.label())
            }
            Event::TaskDeadlines { project, items } => crate::deadlines::summary(project, items),
            Event::DailyDigest { digest } => digest.render(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TaskPriority, TaskStatus};
    use uuid::Uuid;

    fn task() -> Task {
        Task {
            id: Uuid::nil(),
            title: "Turn compost".into(),
            details: String::new(),
            location_id: None,
            due_on: Some(time::macros::date!(2026 - 10 - 01)),
            priority: TaskPriority::High,
            status: TaskStatus::Open,
            mirror_to_linear: false,
            linear_issue_id: None,
            completed_at: None,
            template_id: None,
            assigned_to: None,
        }
    }

    #[test]
    fn kind_column_and_payload_round_trip() {
        let e = Event::TaskCreated { task: task() };
        let back = Event::from_parts(e.kind(), e.payload()).expect("round trip");
        assert_eq!(back, e);
        assert!(e.payload().get("kind").is_none(), "payload must not duplicate the kind column");
    }

    #[test]
    fn kinds_list_matches_serde_tags() {
        let item = InventoryItem {
            id: Uuid::nil(),
            name: "Layer feed".into(),
            unit: "bag".into(),
            reorder_level: Decimal::from(2),
            on_hand: Decimal::from(1),
        };
        let signed = |k: &str| (k.to_owned(), "Photo Release".to_owned(), "Jane Doe".to_owned());
        let (k1, t1, n1) = signed("photo-release");
        let (k2, t2, n2) = signed("photo-release");
        let samples = [
            Event::TaskCreated { task: task() },
            Event::TaskCompleted { task: task() },
            Event::LowStock { item, previous_on_hand: Decimal::from(3) },
            Event::ReleaseSigned { release_key: k1, title: t1, signer_name: n1 },
            Event::ReleaseDeclined { release_key: k2, title: t2, signer_name: n2 },
            Event::AnimalAdopted { animal_name: "Mochi".into(), species: crate::animals::Species::Cat },
            Event::MemberOnboarded {
                discord_user_id: "1554027507932598345".into(),
                name: "Jane Doe".into(),
                role: crate::onboarding::OnboardingRole::Volunteer,
            },
            Event::TaskDeadlines {
                project: "Grant writing".into(),
                items: vec![crate::deadlines::DueItem {
                    title: "Submit the application".into(),
                    due_on: time::macros::date!(2026 - 11 - 12),
                    days_left: 7,
                    issue: Some("SUN-46".into()),
                    url: None,
                    assignee: String::new(),
                }],
            },
        ];
        for e in &samples {
            assert_eq!(&Event::from_parts(e.kind(), e.payload()).expect("round trip"), e);
            let tag = serde_json::to_value(e).expect("ser")["kind"].as_str().expect("tag").to_owned();
            assert_eq!(tag, e.kind());
            assert!(Event::KINDS.contains(&e.kind()));
        }
    }

    #[test]
    fn adoption_announcement_names_the_animal_not_the_adopter() {
        let e = Event::AnimalAdopted { animal_name: "Mochi".into(), species: crate::animals::Species::Cat };
        assert_eq!(e.summary(), "Mochi the cat was adopted!");
        let keys: Vec<String> = e.payload().as_object().expect("object").keys().cloned().collect();
        assert_eq!(keys, ["animal_name", "species"], "no adopter field may ride on the event");
    }

    /// Dates cross every integration boundary (API, event payloads); they must
    /// be ISO-8601 strings, not the `time` crate's compact tuple encoding.
    #[test]
    fn dates_serialise_as_iso_strings() {
        let v = serde_json::to_value(task()).expect("ser");
        assert_eq!(v["due_on"], "2026-10-01");
        let back: Task = serde_json::from_value(v).expect("de");
        assert_eq!(back.due_on, task().due_on);
    }

    #[test]
    fn summary_is_readable() {
        assert_eq!(Event::TaskCreated { task: task() }.summary(), "New high task: Turn compost (due 2026-10-01)");
    }
}

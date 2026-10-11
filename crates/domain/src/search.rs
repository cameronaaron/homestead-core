//! Finding anything by name, across the app, without seeing more than
//! you're allowed to.
//!
//! Each kind of record is searched only if the caller could open that kind
//! of record anyway, so a result can never reveal something they can't
//! read. Incidents are not searchable at all.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ValidationError;
use crate::auth::{Capability, CapabilitySet};

string_enum!(SearchKind, "kind" {
    Animal => "animal", Task => "task", Item => "inventory_item", Planting => "planting", SeedLot => "seed_lot",
    Location => "location", Contact => "contact", Gift => "gift", Event => "event", Equipment => "equipment",
    Vendor => "vendor", Purchase => "purchase",
});

impl SearchKind {
    /// What it takes to see this kind in results: what it takes to open it.
    pub fn read_cap(self) -> Capability {
        match self {
            SearchKind::Animal => Capability::AnimalsRead,
            SearchKind::Task => Capability::TasksRead,
            SearchKind::Item => Capability::InventoryRead,
            SearchKind::Planting | SearchKind::SeedLot | SearchKind::Location => Capability::GardenRead,
            SearchKind::Contact => Capability::ContactsRead,
            SearchKind::Gift => Capability::GiftsRead,
            SearchKind::Event => Capability::CommunityRead,
            SearchKind::Equipment => Capability::EquipmentRead,
            SearchKind::Vendor | SearchKind::Purchase => Capability::PurchasesRead,
        }
    }

    /// The kinds `caps` may search.
    pub fn visible(caps: CapabilitySet) -> Vec<SearchKind> {
        SearchKind::ALL.iter().copied().filter(|k| caps.has(k.read_cap())).collect()
    }

    /// Where a result opens.
    pub fn url(self, id: Uuid) -> String {
        match self {
            SearchKind::Animal => format!("/animals/{id}"),
            SearchKind::Task => "/tasks".into(),
            SearchKind::Item => format!("/inventory/{id}"),
            SearchKind::Planting | SearchKind::Location => "/garden".into(),
            SearchKind::SeedLot => format!("/seeds/{id}"),
            SearchKind::Contact => "/contacts".into(),
            SearchKind::Gift => format!("/gifts/{id}/letter"),
            SearchKind::Event => "/community".into(),
            SearchKind::Equipment => format!("/equipment/{id}"),
            SearchKind::Vendor => "/purchases".into(),
            SearchKind::Purchase => format!("/purchases/{id}"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SearchKind::Animal => "Animal",
            SearchKind::Task => "Task",
            SearchKind::Item => "Inventory",
            SearchKind::Planting => "Planting",
            SearchKind::SeedLot => "Seed lot",
            SearchKind::Location => "Place",
            SearchKind::Contact => "Contact",
            SearchKind::Gift => "Gift",
            SearchKind::Event => "Event",
            SearchKind::Equipment => "Equipment",
            SearchKind::Vendor => "Vendor",
            SearchKind::Purchase => "Purchase",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchHit {
    pub kind: SearchKind,
    pub id: Uuid,
    pub title: String,
    pub subtitle: String,
    pub rank: f32,
    pub url: String,
}

/// Turns what someone typed into a prefix query for Postgres's `simple`
/// text search: every word must match the start of a word in the record.
/// Only letters, digits and `@ . _ -` survive, so the input can never be
/// read as query syntax. `None` if nothing searchable is left.
pub fn prefix_query(input: &str) -> Option<String> {
    let words: Vec<String> = input
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, '@' | '.' | '_' | '-'))
                .collect::<String>()
                .trim_matches(|c: char| matches!(c, '.' | '-' | '_'))
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .take(8)
        .collect();
    if words.is_empty() {
        return None;
    }
    Some(words.iter().map(|w| format!("'{w}':*")).collect::<Vec<_>>().join(" & "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;

    #[test]
    fn input_becomes_a_safe_prefix_query() {
        assert_eq!(prefix_query("Moch"), Some("'moch':*".into()));
        assert_eq!(prefix_query("  layer  FEED "), Some("'layer':* & 'feed':*".into()));
        assert_eq!(prefix_query("ana@example.org"), Some("'ana@example.org':*".into()));
        // Query operators and quotes are dropped, not interpreted.
        assert_eq!(prefix_query("a' | !b & (c):*"), Some("'a':* & 'b':* & 'c':*".into()));
        assert_eq!(prefix_query("' & | ! ( )"), None);
        assert_eq!(prefix_query(""), None);
        assert_eq!(prefix_query(&"w ".repeat(50)).map(|q| q.matches('&').count()), Some(7), "at most 8 words");
    }

    #[test]
    fn people_only_search_what_they_can_open() {
        let volunteer = SearchKind::visible(Role::Volunteer.capabilities());
        assert!(volunteer.contains(&SearchKind::Animal) && !volunteer.contains(&SearchKind::Gift));
        assert!(!volunteer.contains(&SearchKind::Contact) && !volunteer.contains(&SearchKind::Purchase));
        let board = SearchKind::visible(Role::Board.capabilities());
        assert!(board.contains(&SearchKind::Gift) && board.contains(&SearchKind::Contact));
        assert_eq!(SearchKind::visible(CapabilitySet::all()).len(), SearchKind::ALL.len());
        assert!(SearchKind::visible(CapabilitySet::EMPTY).is_empty());
    }
}

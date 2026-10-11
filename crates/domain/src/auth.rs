//! Who may do what: capabilities, and the roles that bundle them.
//!
//! Handlers never ask "is this a steward?"; they ask for a [`Capability`].
//! Roles exist only to hand people a sensible set of capabilities, and the
//! mapping lives here, in one table, so it can be read and tested whole.

use serde::{Deserialize, Serialize};

use crate::ValidationError;

string_enum!(
    /// One thing a person or service may do. The string form is what the
    /// audit log, the API and the docs show.
    Capability, "capability" {
    TasksRead => "tasks.read", TasksWrite => "tasks.write",
    GardenRead => "garden.read", GardenWrite => "garden.write",
    AnimalsRead => "animals.read", AnimalsWrite => "animals.write", AnimalsMedical => "animals.medical",
    InventoryRead => "inventory.read", InventoryAdjust => "inventory.adjust",
    PurchasesRead => "purchases.read", PurchasesWrite => "purchases.write",
    EquipmentRead => "equipment.read", EquipmentWrite => "equipment.write",
    GiftsRead => "gifts.read", GiftsWrite => "gifts.write",
    ContactsRead => "contacts.read", ContactsWrite => "contacts.write", ContactsExport => "contacts.export",
    CommunityRead => "community.read", CommunityWrite => "community.write",
    StandRead => "stand.read", StandWrite => "stand.write",
    ReleasesRead => "releases.read", ReleasesSend => "releases.send",
    GrantsRead => "grants.read", GrantsWrite => "grants.write", GrantsApprove => "grants.approve", GrantsSend => "grants.send",
    IncidentsReport => "incidents.report", IncidentsRead => "incidents.read", IncidentsWrite => "incidents.write",
    ReportsRead => "reports.read",
    AuditRead => "audit.read",
    SystemsRead => "systems.read", SystemsAdmin => "systems.admin",
});

string_enum!(
    /// A bundle of capabilities. The literals match `actor_roles.role`;
    /// `service` is the role of machine credentials calling the API.
    Role, "role" {
    Steward => "steward", Board => "board", FarmLead => "farm_lead", Crew => "crew", Volunteer => "volunteer",
    Service => "service",
});

string_enum!(ActorKind, "kind" { Human => "human", Service => "service" });

impl Capability {
    fn bit(self) -> u64 {
        // `ALL` is in declaration order; the index is stable within a build,
        // and a CapabilitySet is never persisted, so reordering is safe.
        let i = Capability::ALL.iter().position(|c| *c == self).expect("every capability is in ALL");
        1 << i
    }
}

/// A set of capabilities; cheap to copy and to test membership in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CapabilitySet(u64);

impl CapabilitySet {
    pub const EMPTY: CapabilitySet = CapabilitySet(0);

    pub fn of(caps: &[Capability]) -> Self {
        caps.iter().fold(Self::EMPTY, |s, c| s.with(*c))
    }

    pub fn all() -> Self {
        Self::of(Capability::ALL)
    }

    #[must_use]
    pub fn with(self, c: Capability) -> Self {
        Self(self.0 | c.bit())
    }

    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn has(self, c: Capability) -> bool {
        self.0 & c.bit() != 0
    }

    pub fn iter(self) -> impl Iterator<Item = Capability> {
        Capability::ALL.iter().copied().filter(move |c| self.has(*c))
    }

    /// Everything the given roles grant together.
    pub fn for_roles(roles: &[Role]) -> Self {
        roles.iter().fold(Self::EMPTY, |s, r| s.union(r.capabilities()))
    }
}

impl Role {
    /// The whole authorization model in one place.
    pub fn capabilities(self) -> CapabilitySet {
        use Capability::*;
        match self {
            // Runs the organization: everything, including people and systems.
            Role::Steward => CapabilitySet::all(),
            // Oversight: reads the books and the safety record, approves
            // grant campaigns, but doesn't run the farm or send mail.
            Role::Board => CapabilitySet::of(&[
                TasksRead,
                GardenRead,
                AnimalsRead,
                InventoryRead,
                PurchasesRead,
                EquipmentRead,
                GiftsRead,
                ContactsRead,
                CommunityRead,
                StandRead,
                ReleasesRead,
                GrantsRead,
                GrantsApprove,
                IncidentsReport,
                IncidentsRead,
                ReportsRead,
                AuditRead,
            ]),
            // Runs day-to-day operations, follows up on incidents.
            Role::FarmLead => CapabilitySet::of(&[
                TasksRead,
                TasksWrite,
                GardenRead,
                GardenWrite,
                AnimalsRead,
                AnimalsWrite,
                AnimalsMedical,
                InventoryRead,
                InventoryAdjust,
                PurchasesRead,
                PurchasesWrite,
                EquipmentRead,
                EquipmentWrite,
                CommunityRead,
                CommunityWrite,
                StandRead,
                StandWrite,
                ReleasesRead,
                ReleasesSend,
                IncidentsReport,
                IncidentsRead,
                IncidentsWrite,
                ReportsRead,
            ]),
            // Regular hands: does and records the work, gives medication.
            Role::Crew => CapabilitySet::of(&[
                TasksRead,
                TasksWrite,
                GardenRead,
                GardenWrite,
                AnimalsRead,
                AnimalsWrite,
                AnimalsMedical,
                InventoryRead,
                InventoryAdjust,
                EquipmentRead,
                EquipmentWrite,
                CommunityRead,
                StandRead,
                StandWrite,
                IncidentsReport,
            ]),
            // Occasional help: sees the work, ticks tasks off, reports hazards.
            Role::Volunteer => {
                CapabilitySet::of(&[TasksRead, TasksWrite, GardenRead, AnimalsRead, InventoryRead, IncidentsReport])
            }
            // API callers get the farm-operations surface the old shared
            // token had, not people's data, money or the safety record.
            Role::Service => CapabilitySet::of(&[
                TasksRead,
                TasksWrite,
                GardenRead,
                GardenWrite,
                InventoryRead,
                InventoryAdjust,
                PurchasesRead,
                PurchasesWrite,
                EquipmentRead,
                AuditRead,
                ReportsRead,
                SystemsRead,
            ]),
        }
    }

    /// The label people see.
    pub fn title(self) -> &'static str {
        match self {
            Role::Steward => "Steward",
            Role::Board => "Board",
            Role::FarmLead => "Farm Lead",
            Role::Crew => "Crew",
            Role::Volunteer => "Volunteer",
            Role::Service => "Service",
        }
    }
}

/// What it takes to see a table's rows in the audit log: exactly what it
/// takes to see the rows themselves, since an audit entry carries the whole
/// row. Attachments are judged by their owner (see `attachments`). A table
/// missing here is visible only with `systems.admin`, and a test fails
/// for any audited table missing from this list.
pub const AUDIT_VISIBILITY: &[(&str, Capability)] = &[
    ("locations", Capability::GardenRead),
    ("location_shapes", Capability::GardenRead),
    // Pages show a reading to whoever may read its subject (garden, animals
    // or equipment). The audit log rules per table, so it takes the one of
    // those an audit reader might lack: service accounts read the garden and
    // equipment but not animals.
    ("readings", Capability::AnimalsRead),
    // Rows name an animal or a machine; animals.read is the one of the two
    // an audit reader (a service account) may lack.
    ("movements", Capability::AnimalsRead),
    ("reading_values", Capability::AnimalsRead),
    // Who joined through Discord or the /join link, and which agreement
    // they signed.
    ("onboardings", Capability::ReleasesRead),
    ("joins", Capability::ReleasesRead),
    // A sensor (and so its rules) watches a location, an animal or a
    // machine, like a reading: the same rule, for the same reason.
    ("sensors", Capability::AnimalsRead),
    ("sensor_rules", Capability::AnimalsRead),
    ("plantings", Capability::GardenRead),
    ("harvests", Capability::GardenRead),
    ("seed_lots", Capability::GardenRead),
    ("weather_records", Capability::GardenRead),
    ("irrigation_records", Capability::GardenRead),
    ("tasks", Capability::TasksRead),
    ("recurring_task_templates", Capability::TasksRead),
    ("linear_issues", Capability::TasksRead),
    ("inventory_items", Capability::InventoryRead),
    ("low_stock_issues", Capability::InventoryRead),
    ("stock_movements", Capability::InventoryRead),
    ("vendors", Capability::PurchasesRead),
    ("purchases", Capability::PurchasesRead),
    ("purchase_lines", Capability::PurchasesRead),
    ("equipment", Capability::EquipmentRead),
    ("equipment_maintenance", Capability::EquipmentRead),
    ("tags", Capability::EquipmentRead),
    ("equipment_loans", Capability::EquipmentRead),
    ("animals", Capability::AnimalsRead),
    ("animal_placements", Capability::AnimalsRead),
    ("animal_care", Capability::AnimalsMedical),
    ("care_schedules", Capability::AnimalsMedical),
    // Caretakers' contact details: only people who can change animals.
    ("colonies", Capability::AnimalsWrite),
    // Applicants' email and phone: whoever handles adoptions.
    ("adoption_applications", Capability::AnimalsWrite),
    ("gifts", Capability::GiftsRead),
    ("zeffy_webhook_events", Capability::GiftsRead),
    ("contacts", Capability::ContactsRead),
    ("community_events", Capability::CommunityRead),
    ("event_rsvps", Capability::CommunityRead),
    ("event_tasks", Capability::CommunityRead),
    ("event_inventory", Capability::CommunityRead),
    ("event_equipment", Capability::CommunityRead),
    ("volunteer_shifts", Capability::CommunityRead),
    ("volunteer_visits", Capability::CommunityRead),
    ("volunteer_names", Capability::CommunityRead),
    // Volunteers' email and phone: whoever manages volunteers.
    ("volunteers", Capability::CommunityWrite),
    ("stand_sales", Capability::StandRead),
    ("compost_dropoffs", Capability::StandRead),
    ("grant_templates", Capability::GrantsRead),
    ("grant_recipients", Capability::GrantsRead),
    ("grant_campaigns", Capability::GrantsRead),
    ("grant_messages", Capability::GrantsRead),
    ("incidents", Capability::IncidentsRead),
    ("crop_plans", Capability::GardenRead),
    ("soil_tests", Capability::GardenRead),
    ("soil_amendments", Capability::GardenRead),
    ("crop_observations", Capability::GardenRead),
    ("irrigation_zones", Capability::GardenRead),
    ("irrigation_zone_locations", Capability::GardenRead),
    ("harvest_dispositions", Capability::GardenRead),
    ("produce_donations", Capability::GardenRead),
    ("egg_days", Capability::AnimalsRead),
    ("compost_batches", Capability::StandRead),
    ("compost_turns", Capability::StandRead),
    ("checklist_templates", Capability::TasksRead),
    ("checklist_runs", Capability::TasksRead),
    ("losses", Capability::GardenRead),
    ("obligations", Capability::TasksRead),
    ("emergency_plans", Capability::TasksRead),
    ("cost_links", Capability::PurchasesRead),
    ("restore_tests", Capability::SystemsRead),
    ("actors", Capability::SystemsAdmin),
    ("actor_roles", Capability::SystemsAdmin),
    ("notification_preferences", Capability::SystemsAdmin),
    ("field_defaults", Capability::SystemsAdmin),
    ("api_tokens", Capability::SystemsAdmin),
    // Snapshots of deleted files of every kind (incident photos included):
    // stewards only, since the owner-based rule for attachments can't see
    // inside the snapshot.
    ("attachment_purges", Capability::SystemsAdmin),
];

/// The tables whose audit entries `caps` may see.
pub fn visible_audit_tables(caps: CapabilitySet) -> Vec<&'static str> {
    AUDIT_VISIBILITY.iter().filter(|(_, c)| caps.has(*c)).map(|(t, _)| *t).collect()
}

/// A person or service, as stored.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActorRecord {
    pub id: uuid::Uuid,
    pub display_name: String,
    pub email: Option<String>,
    pub kind: ActorKind,
    pub roles: Vec<Role>,
    /// Discord and similar identities that act as this person, e.g.
    /// `discord:123456789`.
    pub external_subject: Option<String>,
    pub has_password: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub disabled_at: Option<time::OffsetDateTime>,
    /// Set when they leave the first-login page. Null until then.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub welcomed_at: Option<time::OffsetDateTime>,
}

impl ActorRecord {
    pub fn capabilities(&self) -> CapabilitySet {
        if self.disabled_at.is_some() { CapabilitySet::EMPTY } else { CapabilitySet::for_roles(&self.roles) }
    }
}

/// A new person or service account.
#[derive(Debug, Clone, Deserialize)]
pub struct NewActor {
    pub display_name: String,
    /// Required for people (it's how they sign in); none for services.
    #[serde(default)]
    pub email: Option<String>,
    pub kind: ActorKind,
    pub roles: Vec<Role>,
}

impl NewActor {
    pub fn validated(mut self) -> Result<Self, ValidationError> {
        self.display_name = self.display_name.trim().to_owned();
        if self.display_name.is_empty() {
            return Err(ValidationError::Blank("name"));
        }
        self.email = self.email.filter(|e| !e.trim().is_empty());
        self.email = match (self.kind, self.email.take()) {
            (ActorKind::Human, None) => return Err(ValidationError::Blank("email")),
            (ActorKind::Human, Some(e)) => Some(crate::contacts::normalise_email(&e)?),
            (ActorKind::Service, Some(_)) => return Err(ValidationError::Malformed("email (services have none)")),
            (ActorKind::Service, None) => None,
        };
        if self.roles.is_empty() {
            return Err(ValidationError::Blank("roles"));
        }
        // A service holds exactly the service role, and people never do: a
        // person with API-only powers, or a script with a Steward's, is a
        // misconfiguration rather than a choice.
        let service_role = self.roles.contains(&Role::Service);
        if (self.kind == ActorKind::Service) != service_role || (service_role && self.roles.len() != 1) {
            return Err(ValidationError::Malformed("roles (services have only the service role)"));
        }
        self.roles.sort_by_key(|r| Role::ALL.iter().position(|x| x == r));
        self.roles.dedup();
        Ok(self)
    }
}

/// Shortest password accepted. Length beats composition rules.
pub const MIN_PASSWORD_LEN: usize = 12;

pub fn check_password(pw: &str) -> Result<(), ValidationError> {
    if pw.chars().count() < MIN_PASSWORD_LEN {
        return Err(ValidationError::Malformed("password (use at least 12 characters)"));
    }
    if pw.len() > 1024 {
        return Err(ValidationError::Malformed("password (too long)"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Capability::*;
    use super::*;

    #[test]
    fn capability_strings_are_domain_dot_verb() {
        for c in Capability::ALL {
            let (domain, verb) = c.as_str().split_once('.').expect("domain.verb");
            assert!(!domain.is_empty() && !verb.is_empty() && !verb.contains('.'), "{c}");
            assert_eq!(c.as_str().parse::<Capability>(), Ok(*c));
        }
        assert!(Capability::ALL.len() <= 64, "CapabilitySet is a u64");
    }

    #[test]
    fn a_set_holds_exactly_what_was_put_in() {
        let s = CapabilitySet::of(&[TasksRead, AuditRead]);
        assert!(s.has(TasksRead) && s.has(AuditRead) && !s.has(TasksWrite));
        assert_eq!(s.iter().collect::<Vec<_>>(), vec![TasksRead, AuditRead]);
        assert!(!CapabilitySet::EMPTY.has(TasksRead));
        assert!(Capability::ALL.iter().all(|c| CapabilitySet::all().has(*c)));
    }

    #[test]
    fn only_the_steward_administers_systems_and_exports_contacts() {
        for r in Role::ALL {
            let caps = r.capabilities();
            assert_eq!(caps.has(SystemsAdmin), *r == Role::Steward, "{r}");
            assert_eq!(caps.has(ContactsExport), *r == Role::Steward, "{r}");
        }
    }

    /// Incidents hold people's names and injuries: everyone can report
    /// one, but only oversight and the farm lead can read them back.
    #[test]
    fn incident_records_are_readable_only_by_oversight() {
        for r in Role::ALL {
            let caps = r.capabilities();
            let expect = matches!(r, Role::Steward | Role::Board | Role::FarmLead);
            assert_eq!(caps.has(IncidentsRead), expect, "{r} incidents.read");
            assert_eq!(caps.has(IncidentsReport), *r != Role::Service, "{r} incidents.report");
        }
    }

    #[test]
    fn machines_never_see_people_money_or_safety_records() {
        let s = Role::Service.capabilities();
        for c in [
            ContactsRead,
            GiftsRead,
            IncidentsRead,
            IncidentsReport,
            AnimalsMedical,
            ReleasesRead,
            GrantsRead,
            SystemsAdmin,
        ] {
            assert!(!s.has(c), "service has {c}");
        }
    }

    #[test]
    fn every_read_accompanies_its_write() {
        // Holding x.write without x.read would let someone change what they
        // can't see; the one deliberate exception is reporting an incident.
        for r in Role::ALL {
            let caps = r.capabilities();
            for c in caps.iter() {
                let (domain, verb) = c.as_str().split_once('.').expect("domain.verb");
                if verb == "read" || c == IncidentsReport {
                    continue;
                }
                let Ok(read) = format!("{domain}.read").parse::<Capability>() else { continue };
                assert!(caps.has(read), "{r} has {c} without {read}");
            }
        }
    }

    #[test]
    fn role_and_kind_literals_match_the_migration() {
        let sql = include_str!("../../../migrations/0011_people.sql");
        let roles = &sql[sql.find("add constraint actor_roles_role_check").expect("role check")..];
        crate::tests::assert_check_list(roles, "role", Role::ALL, Role::as_str);
        crate::tests::assert_check_list(sql, "kind", ActorKind::ALL, ActorKind::as_str);
    }

    #[test]
    fn audit_visibility_follows_record_visibility() {
        let service = visible_audit_tables(Role::Service.capabilities());
        assert!(service.contains(&"tasks") && !service.contains(&"contacts") && !service.contains(&"animal_care"));
        let board = visible_audit_tables(Role::Board.capabilities());
        assert!(board.contains(&"gifts") && !board.contains(&"animal_care") && !board.contains(&"actors"));
        assert_eq!(visible_audit_tables(CapabilitySet::all()).len(), AUDIT_VISIBILITY.len());
    }

    #[test]
    fn roles_union() {
        let both = CapabilitySet::for_roles(&[Role::Volunteer, Role::Board]);
        assert!(both.has(GrantsApprove) && both.has(TasksWrite) && !both.has(SystemsAdmin));
    }

    #[test]
    fn new_actors_are_validated() {
        let person = |email: Option<&str>, roles: Vec<Role>| NewActor {
            display_name: " Sam ".into(),
            email: email.map(str::to_owned),
            kind: ActorKind::Human,
            roles,
        };
        let ok = person(Some(" Sam@Example.org "), vec![Role::Crew, Role::Crew]).validated().expect("ok");
        assert_eq!(
            (ok.display_name.as_str(), ok.email.as_deref(), ok.roles.len()),
            ("Sam", Some("sam@example.org"), 1)
        );
        assert!(person(None, vec![Role::Crew]).validated().is_err());
        assert!(person(Some("nope"), vec![Role::Crew]).validated().is_err());
        assert!(person(Some("a@b.example"), vec![]).validated().is_err());
        assert!(person(Some("a@b.example"), vec![Role::Service]).validated().is_err());
        let svc = |roles| NewActor { display_name: "cron".into(), email: None, kind: ActorKind::Service, roles };
        assert!(svc(vec![Role::Service]).validated().is_ok());
        assert!(svc(vec![Role::Steward]).validated().is_err());
        assert!(svc(vec![Role::Service, Role::Crew]).validated().is_err());
    }

    #[test]
    fn passwords_need_length_not_rules() {
        assert!(check_password("short").is_err());
        assert!(check_password("correct horse battery").is_ok());
        assert!(check_password(&"x".repeat(2000)).is_err());
    }
}

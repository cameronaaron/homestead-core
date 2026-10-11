//! The fiscal-year report: figures for the board, grant applications and
//! the annual summary, derived from the operational records themselves.
//!
//! Nothing here is a maintained total. Every figure is a query over dated
//! records, so it can be traced back to the rows (and their audit history)
//! it came from. Quantities are grouped by unit and never added across
//! units: 12 lb and 3 bunches of kale stay two lines.
//!
//! A report is split into [`ReportSection`]s, and a reader sees a section
//! only if they could open the records it sums. An aggregate over a handful
//! of rows can reveal one of them (one cash gift this year *is* that
//! donor's gift), so `reports.read` alone reveals nothing.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::{Date, Month};

use crate::ValidationError;
use crate::auth::{Capability, CapabilitySet};

/// A July–June fiscal year (the IRS accounting period ending in month 06),
/// named by the calendar year it starts in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FiscalYear {
    pub start_year: i32,
}

impl FiscalYear {
    /// The fiscal year `day` falls in.
    pub fn containing(day: Date) -> Self {
        let start_year = if day.month() >= Month::July { day.year() } else { day.year() - 1 };
        Self { start_year }
    }

    /// A fiscal year from its starting calendar year, within a sane range.
    pub fn starting(start_year: i32) -> Result<Self, ValidationError> {
        if !(2000..=2100).contains(&start_year) {
            return Err(ValidationError::Malformed("year (between 2000 and 2100)"));
        }
        Ok(Self { start_year })
    }

    pub fn first_day(self) -> Date {
        Date::from_calendar_date(self.start_year, Month::July, 1).expect("July 1 exists")
    }

    pub fn last_day(self) -> Date {
        Date::from_calendar_date(self.start_year + 1, Month::June, 30).expect("June 30 exists")
    }

    /// The range a report covers as of `today`: the whole year, or up to
    /// today while it's still running. `None` for a year that hasn't begun.
    pub fn through(self, today: Date) -> Option<Date> {
        (today >= self.first_day()).then(|| today.min(self.last_day()))
    }

    /// "FY 2026–27".
    pub fn label(self) -> String {
        format!("FY {}–{:02}", self.start_year, (self.start_year + 1) % 100)
    }
}

string_enum!(
    /// One part of the report, visible with one capability.
    ReportSection, "section" {
    Volunteers => "volunteers", Events => "events", Animals => "animals", Harvest => "harvest",
    Stand => "stand", Gifts => "gifts", Inventory => "inventory", Costs => "costs", Incidents => "incidents",
});

impl ReportSection {
    /// What it takes to see this section: what it takes to read the records
    /// it's computed from (their `AUDIT_VISIBILITY` rule).
    pub fn read_cap(self) -> Capability {
        match self {
            ReportSection::Volunteers | ReportSection::Events => Capability::CommunityRead,
            // Eggs are in here: `egg_days` are flock records.
            ReportSection::Animals => Capability::AnimalsRead,
            ReportSection::Harvest => Capability::GardenRead,
            ReportSection::Stand => Capability::StandRead,
            ReportSection::Gifts => Capability::GiftsRead,
            ReportSection::Inventory => Capability::InventoryRead,
            ReportSection::Costs => Capability::PurchasesRead,
            ReportSection::Incidents => Capability::IncidentsRead,
        }
    }

    /// The sections `caps` may see; none at all without `reports.read`.
    pub fn visible(caps: CapabilitySet) -> Vec<ReportSection> {
        if !caps.has(Capability::ReportsRead) {
            return Vec::new();
        }
        ReportSection::ALL.iter().copied().filter(|s| caps.has(s.read_cap())).collect()
    }
}

/// How many of something, by a key such as a kind or species.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Count {
    pub key: String,
    pub count: i64,
}

/// A quantity in one unit. `key` is what was measured (a crop, an item);
/// rows with the same key and different units are never merged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Measure {
    pub key: String,
    pub unit: String,
    pub quantity: Decimal,
}

/// Dollars, by a key such as a program or category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Money {
    pub key: String,
    pub amount: Decimal,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub fiscal_year: FiscalYear,
    pub from: Date,
    pub through: Date,
    pub volunteers: Option<Volunteers>,
    pub events: Option<Events>,
    pub animals: Option<Animals>,
    pub harvest: Option<Harvest>,
    pub stand: Option<Stand>,
    pub gifts: Option<Gifts>,
    pub inventory: Option<Inventory>,
    pub costs: Option<Costs>,
    pub incidents: Option<Incidents>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Volunteers {
    pub hours: Decimal,
    pub shifts: i64,
    /// Distinct volunteers. "Sam" and "Sam R." count once only after
    /// someone merges them on the Volunteers page.
    pub people: i64,
    pub hours_by_group: Vec<Money>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Events {
    pub held: i64,
    pub by_kind: Vec<Count>,
    /// Sum over events whose attendance was recorded.
    pub attendance: i64,
    pub without_attendance: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Animals {
    /// Head count of animals in our care at some point in the period, by
    /// species: arrived in it, placed during it, or still in care today
    /// having arrived by its end. A lower bound: an animal that died with
    /// no other dated record that year isn't counted.
    pub assisted: Vec<Count>,
    /// Head count that arrived in the period, by species.
    pub intake: Vec<Count>,
    /// Head count placed in the period, by placement kind (adoption,
    /// foster, return to colony, transfer).
    pub placements: Vec<Count>,
    pub eggs_collected: i64,
    pub eggs_damaged: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Harvest {
    /// By crop and unit.
    pub harvested: Vec<Measure>,
    /// By where it went (sale, donation, feed…) and unit.
    pub dispositions: Vec<Measure>,
    /// Produce donated, by unit.
    pub donated: Vec<Measure>,
    /// Losses that recorded a quantity, by unit.
    pub losses: Vec<Measure>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stand {
    pub revenue: Decimal,
    pub sales: i64,
    /// Quantity sold by item and unit.
    pub sold: Vec<Measure>,
    pub compost_dropoffs: i64,
    pub compost_lb: Decimal,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Gifts {
    /// Cash and checks recorded here. Donations through Zeffy are receipted
    /// and totalled in Zeffy and are not included.
    pub cash: Money,
    pub cash_count: i64,
    pub sponsorship: Money,
    pub sponsorship_count: i64,
    pub in_kind_count: i64,
    /// The donors' own estimates, where they gave one; for our books, never
    /// for the donor's acknowledgment.
    pub in_kind_estimated: Decimal,
    pub in_kind_unvalued: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Inventory {
    /// Stock used, by item and unit.
    pub consumed: Vec<Measure>,
    pub wasted: Vec<Measure>,
    pub received: Vec<Measure>,
    /// Added from our own harvest.
    pub harvested_in: Vec<Measure>,
    /// Net count corrections, signed.
    pub adjusted: Vec<Measure>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Costs {
    pub total: Decimal,
    /// By the program a purchase was linked to, else its category.
    pub by_program: Vec<Money>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Incidents {
    pub reported: i64,
    pub by_kind: Vec<Count>,
    pub still_open: i64,
}

impl Report {
    /// One `section, metric, key, unit, value` row per figure, for a
    /// spreadsheet. Values are plain numbers; keys are whatever people
    /// typed, so the CSV writer must neutralise them.
    pub fn rows(&self) -> Vec<[String; 5]> {
        let mut out = Vec::new();
        let mut row = |s: &str, metric: &str, key: &str, unit: &str, value: String| {
            out.push([s.to_owned(), metric.to_owned(), key.to_owned(), unit.to_owned(), value]);
        };
        let n = |d: Decimal| d.normalize().to_string();
        if let Some(v) = &self.volunteers {
            row("volunteers", "hours", "", "hours", n(v.hours));
            row("volunteers", "shifts", "", "", v.shifts.to_string());
            row("volunteers", "people", "", "", v.people.to_string());
            for g in &v.hours_by_group {
                row("volunteers", "hours_by_group", &g.key, "hours", n(g.amount));
            }
        }
        if let Some(e) = &self.events {
            row("events", "held", "", "", e.held.to_string());
            for k in &e.by_kind {
                row("events", "held_by_kind", &k.key, "", k.count.to_string());
            }
            row("events", "attendance", "", "people", e.attendance.to_string());
            row("events", "without_attendance", "", "", e.without_attendance.to_string());
        }
        if let Some(a) = &self.animals {
            for (metric, list) in [("assisted", &a.assisted), ("intake", &a.intake), ("placements", &a.placements)] {
                for c in list {
                    row("animals", metric, &c.key, "head", c.count.to_string());
                }
            }
            row("animals", "eggs_collected", "", "eggs", a.eggs_collected.to_string());
            row("animals", "eggs_damaged", "", "eggs", a.eggs_damaged.to_string());
        }
        let mut measures = |section: &str, metric: &str, list: &[Measure]| {
            for m in list {
                out.push([section.into(), metric.into(), m.key.clone(), m.unit.clone(), n(m.quantity)]);
            }
        };
        if let Some(h) = &self.harvest {
            measures("harvest", "harvested", &h.harvested);
            measures("harvest", "dispositions", &h.dispositions);
            measures("harvest", "donated", &h.donated);
            measures("harvest", "losses", &h.losses);
        }
        if let Some(s) = &self.stand {
            measures("stand", "sold", &s.sold);
        }
        if let Some(i) = &self.inventory {
            measures("inventory", "consumed", &i.consumed);
            measures("inventory", "wasted", &i.wasted);
            measures("inventory", "received", &i.received);
            measures("inventory", "harvested_in", &i.harvested_in);
            measures("inventory", "adjusted", &i.adjusted);
        }
        let mut row = |s: &str, metric: &str, key: &str, unit: &str, value: String| {
            out.push([s.to_owned(), metric.to_owned(), key.to_owned(), unit.to_owned(), value]);
        };
        if let Some(s) = &self.stand {
            row("stand", "revenue", "", "USD", n(s.revenue));
            row("stand", "sales", "", "", s.sales.to_string());
            row("stand", "compost_dropoffs", "", "", s.compost_dropoffs.to_string());
            row("stand", "compost", "", "lb", n(s.compost_lb));
        }
        if let Some(g) = &self.gifts {
            row("gifts", "cash", "", "USD", n(g.cash.amount));
            row("gifts", "cash_count", "", "", g.cash_count.to_string());
            row("gifts", "sponsorship", "", "USD", n(g.sponsorship.amount));
            row("gifts", "sponsorship_count", "", "", g.sponsorship_count.to_string());
            row("gifts", "in_kind_count", "", "", g.in_kind_count.to_string());
            row("gifts", "in_kind_estimated", "", "USD", n(g.in_kind_estimated));
            row("gifts", "in_kind_unvalued", "", "", g.in_kind_unvalued.to_string());
        }
        if let Some(c) = &self.costs {
            row("costs", "total", "", "USD", n(c.total));
            for m in &c.by_program {
                row("costs", "by_program", &m.key, "USD", n(m.amount));
            }
        }
        if let Some(i) = &self.incidents {
            row("incidents", "reported", "", "", i.reported.to_string());
            for k in &i.by_kind {
                row("incidents", "by_kind", &k.key, "", k.count.to_string());
            }
            row("incidents", "still_open", "", "", i.still_open.to_string());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use time::macros::date;

    use super::*;
    use crate::auth::Role;

    #[test]
    fn fiscal_year_turns_over_on_july_first() {
        assert_eq!(FiscalYear::containing(date!(2026 - 06 - 30)).start_year, 2025);
        assert_eq!(FiscalYear::containing(date!(2026 - 07 - 01)).start_year, 2026);
        assert_eq!(FiscalYear::containing(date!(2027 - 01 - 15)).start_year, 2026);
        let fy = FiscalYear::starting(2026).expect("fy");
        assert_eq!((fy.first_day(), fy.last_day()), (date!(2026 - 07 - 01), date!(2027 - 06 - 30)));
        assert_eq!(fy.label(), "FY 2026–27");
        assert_eq!(FiscalYear::starting(2099).expect("fy").label(), "FY 2099–00");
        assert!(FiscalYear::starting(1).is_err());
    }

    #[test]
    fn a_running_year_reports_up_to_today() {
        let fy = FiscalYear::starting(2026).expect("fy");
        assert_eq!(fy.through(date!(2026 - 09 - 28)), Some(date!(2026 - 09 - 28)));
        assert_eq!(fy.through(date!(2028 - 01 - 01)), Some(date!(2027 - 06 - 30)));
        assert_eq!(fy.through(date!(2026 - 06 - 30)), None, "hasn't begun");
    }

    #[test]
    fn sections_follow_the_records_they_sum() {
        let farm_lead = ReportSection::visible(Role::FarmLead.capabilities());
        assert!(farm_lead.contains(&ReportSection::Harvest) && farm_lead.contains(&ReportSection::Incidents));
        assert!(!farm_lead.contains(&ReportSection::Gifts), "a farm lead can't read gifts, so can't total them");
        let board = ReportSection::visible(Role::Board.capabilities());
        assert!(board.contains(&ReportSection::Gifts) && board.contains(&ReportSection::Costs));
        let service = ReportSection::visible(Role::Service.capabilities());
        assert!(!service.contains(&ReportSection::Gifts) && !service.contains(&ReportSection::Incidents));
        // Crew read animals and gardens, but hold no reports.read.
        assert!(ReportSection::visible(Role::Crew.capabilities()).is_empty());
        assert_eq!(ReportSection::visible(CapabilitySet::all()).len(), ReportSection::ALL.len());
        for s in ReportSection::ALL {
            assert!(
                crate::auth::AUDIT_VISIBILITY.iter().any(|(_, c)| *c == s.read_cap()),
                "{s} is gated by a capability that guards no records"
            );
        }
    }

    #[test]
    fn rows_keep_units_apart_and_omit_hidden_sections() {
        let fy = FiscalYear::starting(2026).expect("fy");
        let kale = |unit: &str, q: i64| Measure { key: "Kale".into(), unit: unit.into(), quantity: Decimal::from(q) };
        let report = Report {
            fiscal_year: fy,
            from: fy.first_day(),
            through: fy.last_day(),
            volunteers: None,
            events: None,
            animals: None,
            harvest: Some(Harvest {
                harvested: vec![kale("lb", 12), kale("bunch", 3)],
                dispositions: vec![],
                donated: vec![],
                losses: vec![],
            }),
            stand: None,
            gifts: None,
            inventory: None,
            costs: None,
            incidents: None,
        };
        let rows = report.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], ["harvest", "harvested", "Kale", "lb", "12"].map(String::from));
        assert_eq!(rows[1][3..], ["bunch".to_owned(), "3".to_owned()]);
        assert!(rows.iter().all(|r| r[0] == "harvest"), "only the section the reader may see");
    }
}

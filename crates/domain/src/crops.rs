//! Crop plans, what's due, and what has grown where.
//!
//! A plan is a crop with its timing and one planting per succession date.
//! Each succession's sowing (and transplanting) is an ordinary task, so the
//! work shows up where the rest of the farm's work does.
//!
//! Rotation here is information, not a rule: nothing refuses a planting
//! because the same family was in the bed last year.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::{Date, Duration};
use uuid::Uuid;

use crate::{HarvestUnit, Planting, PlantingStatus, SowMethod, ValidationError, non_blank};

string_enum!(
    /// Which part of a planting a task is for. Completing the task records it.
    PlantingStep, "planting_step" { Sow => "sow", Transplant => "transplant" }
);

/// One sowing in a plan: where it grows and the day its seed goes in (for a
/// transplant, the day it's started in trays).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct Succession {
    pub location_id: Uuid,
    pub sow_on: Date,
}

/// Dates `count` sowings apart by `every_days`, starting on `first`.
pub fn succession_dates(first: Date, every_days: u16, count: u8) -> Vec<Date> {
    (0..i64::from(count.max(1))).map(|i| first + Duration::days(i * i64::from(every_days))).collect()
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewCropPlan {
    pub crop: String,
    #[serde(default)]
    pub variety: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default = "direct")]
    pub method: SowMethod,
    /// Days from starting seed to transplanting it out.
    #[serde(default)]
    pub days_to_transplant: Option<u16>,
    /// Days from sowing to the first harvest.
    #[serde(default)]
    pub days_to_harvest: Option<u16>,
    /// How long it keeps giving once harvest starts.
    #[serde(default)]
    pub harvest_window_days: Option<u16>,
    #[serde(default)]
    pub seed_lot_id: Option<Uuid>,
    /// Expected from each succession, not the whole plan.
    #[serde(default)]
    pub expected_yield: Option<Decimal>,
    #[serde(default)]
    pub expected_yield_unit: Option<HarvestUnit>,
    pub successions: Vec<Succession>,
}

fn direct() -> SowMethod {
    SowMethod::Direct
}

/// The dates a succession works out to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub transplant_on: Option<Date>,
    pub harvest_from: Option<Date>,
    pub harvest_until: Option<Date>,
}

impl NewCropPlan {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.successions.is_empty() {
            return Err(ValidationError::Blank("succession date"));
        }
        if self.successions.len() > 52 {
            return Err(ValidationError::Malformed("successions (at most 52 in one plan)"));
        }
        let mut seen = self.successions.clone();
        seen.sort_by_key(|s| (s.location_id, s.sow_on));
        if seen.windows(2).any(|w| w[0] == w[1]) {
            return Err(ValidationError::Malformed("successions (the same place and date twice)"));
        }
        match (self.method, self.days_to_transplant) {
            (SowMethod::Transplant, None) => return Err(ValidationError::Blank("days to transplant")),
            (SowMethod::Direct, Some(_)) => {
                return Err(ValidationError::Malformed("days to transplant (only for transplants)"));
            }
            _ => {}
        }
        if self.days_to_transplant.is_some_and(|d| !(1..=180).contains(&d)) {
            return Err(ValidationError::Malformed("days to transplant (1–180)"));
        }
        if self.days_to_harvest.is_some_and(|d| !(1..=730).contains(&d)) {
            return Err(ValidationError::Malformed("days to harvest (1–730)"));
        }
        if let (Some(t), Some(h)) = (self.days_to_transplant, self.days_to_harvest)
            && h < t
        {
            return Err(ValidationError::Malformed("days to harvest (it's before transplanting)"));
        }
        if self.harvest_window_days.is_some_and(|d| !(1..=365).contains(&d)) {
            return Err(ValidationError::Malformed("harvest window (1–365 days)"));
        }
        if self.harvest_window_days.is_some() && self.days_to_harvest.is_none() {
            return Err(ValidationError::Blank("days to harvest (a harvest window needs its start)"));
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
            family: self.family.trim().to_owned(),
            notes: self.notes.trim().to_owned(),
            ..self
        })
    }

    pub fn timing(&self, sow_on: Date) -> Timing {
        let after = |d: Option<u16>| d.map(|d| sow_on + Duration::days(i64::from(d)));
        let harvest_from = after(self.days_to_harvest);
        Timing {
            transplant_on: after(self.days_to_transplant),
            harvest_from,
            harvest_until: harvest_from.zip(self.harvest_window_days).map(|(f, w)| f + Duration::days(i64::from(w))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CropPlan {
    pub id: Uuid,
    pub crop: String,
    pub variety: String,
    pub family: String,
    pub notes: String,
    pub method: SowMethod,
    pub days_to_transplant: Option<u16>,
    pub days_to_harvest: Option<u16>,
    pub harvest_window_days: Option<u16>,
    pub seed_lot_id: Option<Uuid>,
    pub expected_yield: Option<Decimal>,
    pub expected_yield_unit: Option<HarvestUnit>,
}

/// What a planting needs, as of a day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Due {
    /// Planned to be sown before today and not sown.
    SowOverdue,
    /// To be sown today or in the next six days.
    SowThisWeek,
    /// Its harvest window closed (or its start passed, with no window) and nothing was harvested.
    HarvestOverdue,
    /// Expected to be harvestable today or within two weeks.
    HarvestSoon,
}

/// How far ahead "soon" looks for harvests.
pub const HARVEST_LOOKAHEAD_DAYS: i64 = 13;

impl Due {
    pub fn of(p: &Planting, today: Date) -> Option<Due> {
        match p.status {
            PlantingStatus::Planned if p.sown_on < today => Some(Due::SowOverdue),
            PlantingStatus::Planned if p.sown_on <= today + Duration::days(6) => Some(Due::SowThisWeek),
            PlantingStatus::Growing => {
                let from = p.expected_harvest_on?;
                if p.expected_harvest_until.unwrap_or(from) < today {
                    Some(Due::HarvestOverdue)
                } else if from <= today + Duration::days(HARVEST_LOOKAHEAD_DAYS) {
                    Some(Due::HarvestSoon)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Expected yield set against what was harvested. Only harvests in the
/// expected unit are compared; anything else is shown beside it, never
/// converted (a bunch is not a weight).
#[derive(Debug, Clone, PartialEq)]
pub struct YieldComparison {
    pub expected: Option<(Decimal, HarvestUnit)>,
    /// Harvested in the expected unit (or in total, when one unit was used and nothing was expected).
    pub actual: Option<(Decimal, HarvestUnit)>,
    /// Harvested in any other unit.
    pub other: Vec<(Decimal, HarvestUnit)>,
}

impl YieldComparison {
    pub fn new(expected: Option<(Decimal, HarvestUnit)>, harvested: &[(Decimal, HarvestUnit)]) -> Self {
        let mut by_unit: Vec<(Decimal, HarvestUnit)> = Vec::new();
        for (q, u) in harvested {
            match by_unit.iter_mut().find(|(_, x)| x == u) {
                Some((sum, _)) => *sum += *q,
                None => by_unit.push((*q, *u)),
            }
        }
        // Nothing harvested yet is not "0% of expected": there's nothing to compare.
        if by_unit.is_empty() {
            return Self { expected, actual: None, other: Vec::new() };
        }
        let key = expected.map(|(_, u)| u).or_else(|| (by_unit.len() == 1).then(|| by_unit[0].1));
        let actual = key.map(|k| (by_unit.iter().filter(|(_, u)| *u == k).map(|(q, _)| *q).sum(), k));
        let other = by_unit.into_iter().filter(|(_, u)| Some(*u) != key).collect();
        Self { expected, actual, other }
    }

    /// Harvested as a percentage of expected, when the two are comparable.
    pub fn percent(&self) -> Option<Decimal> {
        let ((e, _), (a, _)) = (self.expected?, self.actual?);
        Some((a * Decimal::from(100) / e).round_dp(0))
    }
}

/// When a crop, or anything of its family, was last in a place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastGrown {
    /// The last day it's known to have been in the ground.
    pub on: Date,
    /// False when a finished planting's end wasn't recorded, so `on` is its
    /// sowing date and the real gap is shorter than it looks.
    pub exact: bool,
    /// It's in the ground now.
    pub present: bool,
}

impl LastGrown {
    pub fn days_before(self, today: Date) -> i64 {
        (today - self.on).whole_days().max(0)
    }
}

fn same(a: &str, b: &str) -> bool {
    !a.is_empty() && a.trim().eq_ignore_ascii_case(b.trim())
}

/// Whether `p` shares `crop` or (when both record one) `family`.
pub fn is_related(p: &Planting, crop: &str, family: &str) -> bool {
    same(&p.crop, crop) || (!family.trim().is_empty() && same(&p.family, family))
}

/// The last time `crop` or its `family` occupied a place, from that place's
/// plantings. A plan is not history, even one whose date has passed: it may
/// never have gone in.
pub fn last_grown(history: &[Planting], crop: &str, family: &str, today: Date) -> Option<LastGrown> {
    history
        .iter()
        .filter(|p| is_related(p, crop, family) && p.status != PlantingStatus::Planned && p.sown_on <= today)
        .map(|p| match (p.status.is_over(), p.ended_on) {
            (false, _) => LastGrown { on: today, exact: true, present: true },
            (true, Some(end)) => LastGrown { on: end, exact: true, present: false },
            (true, None) => LastGrown { on: p.sown_on, exact: false, present: false },
        })
        .max_by_key(|l| (l.on, l.exact))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn plan() -> NewCropPlan {
        NewCropPlan {
            crop: " Lettuce ".into(),
            variety: "Jericho".into(),
            family: "aster".into(),
            notes: String::new(),
            method: SowMethod::Direct,
            days_to_transplant: None,
            days_to_harvest: Some(45),
            harvest_window_days: Some(21),
            seed_lot_id: None,
            expected_yield: Some(Decimal::from(8)),
            expected_yield_unit: Some(HarvestUnit::Lb),
            successions: succession_dates(date!(2026 - 03 - 01), 14, 4)
                .into_iter()
                .map(|sow_on| Succession { location_id: Uuid::nil(), sow_on })
                .collect(),
        }
    }

    #[test]
    fn successions_are_evenly_spaced() {
        assert_eq!(
            succession_dates(date!(2026 - 03 - 01), 14, 4),
            [date!(2026 - 03 - 01), date!(2026 - 03 - 15), date!(2026 - 03 - 29), date!(2026 - 04 - 12)]
        );
        assert_eq!(succession_dates(date!(2026 - 03 - 01), 14, 0).len(), 1, "at least the first");
    }

    #[test]
    fn a_plan_is_checked_whole() {
        let ok = plan().validated().expect("valid");
        assert_eq!(ok.crop, "Lettuce");
        assert!(NewCropPlan { successions: vec![], ..plan() }.validated().is_err());
        let twice = vec![plan().successions[0]; 2];
        assert!(NewCropPlan { successions: twice, ..plan() }.validated().is_err(), "same bed, same day");
        assert!(NewCropPlan { method: SowMethod::Transplant, ..plan() }.validated().is_err(), "needs its timing");
        assert!(NewCropPlan { days_to_transplant: Some(30), ..plan() }.validated().is_err(), "direct sown");
        let transplant = NewCropPlan { method: SowMethod::Transplant, days_to_transplant: Some(30), ..plan() };
        assert!(transplant.clone().validated().is_ok());
        assert!(
            NewCropPlan { days_to_harvest: Some(20), ..transplant }.validated().is_err(),
            "harvest before transplant"
        );
        assert!(NewCropPlan { days_to_harvest: None, ..plan() }.validated().is_err(), "a window needs a start");
        assert!(NewCropPlan { expected_yield_unit: None, ..plan() }.validated().is_err());
        assert!(NewCropPlan { expected_yield: Some(Decimal::ZERO), ..plan() }.validated().is_err());
    }

    #[test]
    fn timing_follows_from_the_sowing_date() {
        let p = NewCropPlan { method: SowMethod::Transplant, days_to_transplant: Some(28), ..plan() };
        assert_eq!(
            p.timing(date!(2026 - 03 - 01)),
            Timing {
                transplant_on: Some(date!(2026 - 03 - 29)),
                harvest_from: Some(date!(2026 - 04 - 15)),
                harvest_until: Some(date!(2026 - 05 - 06)),
            }
        );
        let bare = NewCropPlan { days_to_harvest: None, harvest_window_days: None, ..plan() };
        assert_eq!(
            bare.timing(date!(2026 - 03 - 01)),
            Timing { transplant_on: None, harvest_from: None, harvest_until: None }
        );
    }

    #[test]
    fn what_is_due_depends_on_the_day() {
        let today = date!(2026 - 03 - 10);
        let planned = |sown| Planting { status: PlantingStatus::Planned, ..crate::tests::planting("Lettuce", sown) };
        assert_eq!(Due::of(&planned(date!(2026 - 03 - 09)), today), Some(Due::SowOverdue));
        assert_eq!(Due::of(&planned(date!(2026 - 03 - 10)), today), Some(Due::SowThisWeek));
        assert_eq!(Due::of(&planned(date!(2026 - 03 - 16)), today), Some(Due::SowThisWeek));
        assert_eq!(Due::of(&planned(date!(2026 - 03 - 17)), today), None);
        let growing = |from, until| Planting {
            expected_harvest_on: from,
            expected_harvest_until: until,
            ..crate::tests::planting("Lettuce", date!(2026 - 01 - 20))
        };
        assert_eq!(Due::of(&growing(Some(date!(2026 - 03 - 20)), None), today), Some(Due::HarvestSoon));
        assert_eq!(Due::of(&growing(Some(date!(2026 - 03 - 24)), None), today), None, "two weeks out");
        assert_eq!(Due::of(&growing(Some(date!(2026 - 03 - 01)), None), today), Some(Due::HarvestOverdue));
        assert_eq!(
            Due::of(&growing(Some(date!(2026 - 03 - 01)), Some(date!(2026 - 03 - 15))), today),
            Some(Due::HarvestSoon),
            "still inside its window"
        );
        assert_eq!(Due::of(&growing(None, None), today), None, "no expectation, nothing overdue");
        let harvesting = Planting { status: PlantingStatus::Harvesting, ..growing(Some(date!(2026 - 03 - 01)), None) };
        assert_eq!(Due::of(&harvesting, today), None, "already being picked");
    }

    #[test]
    fn yields_are_compared_only_in_the_same_unit() {
        use HarvestUnit::*;
        let d = Decimal::from;
        let c = YieldComparison::new(Some((d(8), Lb)), &[(d(3), Lb), (d(4), Lb), (d(2), Bunch)]);
        assert_eq!(c.actual, Some((d(7), Lb)));
        assert_eq!(c.other, [(d(2), Bunch)]);
        assert_eq!(c.percent(), Some(d(88)));
        let none = YieldComparison::new(Some((d(8), Lb)), &[(d(2), Bunch)]);
        assert_eq!((none.actual, none.percent()), (Some((d(0), Lb)), Some(d(0))));
        let unharvested = YieldComparison::new(Some((d(8), Lb)), &[]);
        assert_eq!((unharvested.actual, unharvested.percent()), (None, None), "not harvested is not 0%");
        let unexpected = YieldComparison::new(None, &[(d(5), Each)]);
        assert_eq!((unexpected.actual, unexpected.percent()), (Some((d(5), Each)), None));
        let mixed = YieldComparison::new(None, &[(d(5), Each), (d(1), Lb)]);
        assert_eq!((mixed.actual, mixed.other.len()), (None, 2), "nothing to single out");
    }

    #[test]
    fn literals_match_the_migration() {
        let sql = include_str!("../../../migrations/0027_crop_planning.sql");
        crate::tests::assert_check_list(sql, "planting_step", PlantingStep::ALL, PlantingStep::as_str);
        crate::tests::assert_check_list(sql, "method", SowMethod::ALL, SowMethod::as_str);
    }

    fn p(crop: &str, family: &str, sown: Date, status: PlantingStatus, ended: Option<Date>) -> Planting {
        Planting { family: family.into(), status, ended_on: ended, ..crate::tests::planting(crop, sown) }
    }

    const TODAY: Date = date!(2026 - 09 - 28);

    #[test]
    fn a_family_is_remembered_across_its_crops() {
        use PlantingStatus::*;
        let history = [
            p("Tomato", "nightshade", date!(2025 - 04 - 01), Finished, Some(date!(2025 - 10 - 01))),
            p("Pepper", "Nightshade", date!(2024 - 04 - 01), Finished, Some(date!(2024 - 09 - 01))),
            p("Lettuce", "aster", date!(2026 - 03 - 01), Finished, Some(date!(2026 - 05 - 01))),
        ];
        let last = last_grown(&history, "Eggplant", "nightshade", TODAY).expect("tomato counts");
        assert_eq!(last, LastGrown { on: date!(2025 - 10 - 01), exact: true, present: false });
        assert_eq!(last.days_before(TODAY), 362);
        assert!(last_grown(&history, "Squash", "cucurbit", TODAY).is_none());
        assert!(last_grown(&history, "Squash", "", TODAY).is_none(), "a blank family matches nothing");
        assert!(last_grown(&history, "lettuce", "", TODAY).is_some(), "the crop itself matches, any case");
    }

    #[test]
    fn a_crop_in_the_ground_is_there_today_and_plans_are_not_history() {
        use PlantingStatus::*;
        let growing = [p("Kale", "brassica", date!(2026 - 08 - 01), Growing, None)];
        let now = last_grown(&growing, "Kale", "", TODAY).expect("growing");
        assert!(now.present && now.days_before(TODAY) == 0);
        let planned = [
            p("Kale", "brassica", date!(2026 - 10 - 15), Planned, None),
            p("Kale", "brassica", date!(2026 - 09 - 01), Planned, None),
        ];
        assert!(last_grown(&planned, "Kale", "brassica", TODAY).is_none(), "an overdue plan never went in");
    }

    #[test]
    fn an_unrecorded_end_is_flagged_not_invented() {
        use PlantingStatus::*;
        let history = [p("Beans", "legume", date!(2025 - 05 - 01), Finished, None)];
        let last = last_grown(&history, "Peas", "legume", TODAY).expect("beans");
        assert_eq!(last, LastGrown { on: date!(2025 - 05 - 01), exact: false, present: false });
    }
}

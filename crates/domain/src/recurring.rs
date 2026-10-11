//! Routine work that repeats: feed the hens every morning, inspect the
//! irrigation every Tuesday, count the feed on the 1st.
//!
//! A template makes ordinary tasks. Each occurrence is a date, and a task
//! is made at most once per (template, date), however often generation
//! runs, so a restart or a second run can't duplicate anything.

use serde::{Deserialize, Serialize};
use time::{Date, Duration, Month, Weekday};
use uuid::Uuid;

use crate::{TaskPriority, ValidationError, non_blank};

/// When a template recurs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "every", rename_all = "snake_case")]
pub enum Schedule {
    /// Every `days` days (1 = daily), counted from the first occurrence.
    Days { days: u16 },
    /// On these weekdays each week.
    Weekdays { on: WeekdaySet },
    /// On this day of every month; 29–31 fall on the last day of shorter months.
    MonthDay { day: u8 },
}

/// A set of weekdays as a bitmask (Monday = bit 0), matching the column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WeekdaySet(pub u8);

impl WeekdaySet {
    pub const ALL_DAYS: [Weekday; 7] = [
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
        Weekday::Saturday,
        Weekday::Sunday,
    ];

    pub fn of(days: &[Weekday]) -> Self {
        Self(days.iter().fold(0, |m, d| m | 1 << d.number_days_from_monday()))
    }

    pub fn contains(self, d: Weekday) -> bool {
        self.0 & (1 << d.number_days_from_monday()) != 0
    }

    pub fn days(self) -> impl Iterator<Item = Weekday> {
        Self::ALL_DAYS.into_iter().filter(move |d| self.contains(*d))
    }
}

fn last_day(year: i32, month: Month) -> u8 {
    time::util::days_in_month(month, year)
}

impl Schedule {
    pub fn validated(self) -> Result<Self, ValidationError> {
        match self {
            Schedule::Days { days: 0 } => Err(ValidationError::NotPositive("every N days")),
            Schedule::Days { days } if days > 366 => Err(ValidationError::Malformed("every N days (at most 366)")),
            Schedule::Weekdays { on } if on.0 == 0 || on.0 > 0x7f => {
                Err(ValidationError::Malformed("weekdays (choose at least one)"))
            }
            Schedule::MonthDay { day } if !(1..=31).contains(&day) => {
                Err(ValidationError::Malformed("day of the month (1–31)"))
            }
            ok => Ok(ok),
        }
    }

    /// The first occurrence on or after `from`. For `Days`, `anchor` is an
    /// occurrence and the rest follow every N days from it.
    pub fn next_on_or_after(self, from: Date, anchor: Date) -> Date {
        match self {
            Schedule::Days { days } => {
                let n = i64::from(days.max(1));
                if from <= anchor {
                    return anchor;
                }
                let behind = (from - anchor).whole_days();
                let steps = (behind + n - 1) / n;
                anchor + Duration::days(steps * n)
            }
            Schedule::Weekdays { on } => {
                let mut d = from;
                // A non-empty set always matches within a week.
                for _ in 0..7 {
                    if on.contains(d.weekday()) {
                        return d;
                    }
                    d = d.next_day().unwrap_or(d);
                }
                d
            }
            Schedule::MonthDay { day } => {
                let on = |y: i32, m: Month| {
                    Date::from_calendar_date(y, m, day.min(last_day(y, m))).expect("clamped to the month")
                };
                let this = on(from.year(), from.month());
                if this >= from {
                    return this;
                }
                let (y, m) = if from.month() == Month::December {
                    (from.year() + 1, Month::January)
                } else {
                    (from.year(), from.month().next())
                };
                on(y, m)
            }
        }
    }

    /// Every occurrence in `[from, to]`, in order.
    pub fn between(self, from: Date, to: Date, anchor: Date) -> Vec<Date> {
        let mut out = Vec::new();
        let mut d = self.next_on_or_after(from, anchor);
        while d <= to {
            out.push(d);
            match d.next_day() {
                Some(next) => d = self.next_on_or_after(next, anchor),
                None => break,
            }
        }
        out
    }

    pub fn describe(self) -> String {
        match self {
            Schedule::Days { days: 1 } => "every day".into(),
            Schedule::Days { days: 7 } => "every week".into(),
            Schedule::Days { days } => format!("every {days} days"),
            Schedule::Weekdays { on } => {
                let names: Vec<String> = on.days().map(|d| d.to_string()).collect();
                format!("every {}", names.join(", "))
            }
            Schedule::MonthDay { day } => format!("monthly on day {day}"),
        }
    }
}

/// Which occurrences to turn into tasks, given the ones now due (oldest
/// first). Without catch-up only the latest is made: a week of missed
/// "check the water" tasks helps nobody, today's does. With catch-up
/// (medication, anything whose every occurrence matters) all of them are.
pub fn to_create(due: &[Date], catch_up: bool) -> &[Date] {
    if catch_up || due.len() <= 1 { due } else { &due[due.len() - 1..] }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewTemplate {
    pub title: String,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub location_id: Option<Uuid>,
    #[serde(default = "normal")]
    pub priority: TaskPriority,
    pub schedule: Schedule,
    /// The first occurrence is on or after this date.
    pub starts_on: Date,
    /// Make each task this many days before it's due.
    #[serde(default)]
    pub lead_days: u8,
    #[serde(default)]
    pub catch_up: bool,
}

fn normal() -> TaskPriority {
    TaskPriority::Normal
}

impl NewTemplate {
    pub fn validated(self) -> Result<Self, ValidationError> {
        if self.lead_days > 30 {
            return Err(ValidationError::Malformed("lead days (at most 30)"));
        }
        Ok(Self {
            title: non_blank("title", &self.title)?,
            details: self.details.trim().to_owned(),
            schedule: self.schedule.validated()?,
            ..self
        })
    }

    /// The first occurrence: also the anchor `Days` counts from.
    pub fn first_occurrence(&self) -> Date {
        self.schedule.next_on_or_after(self.starts_on, self.starts_on)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Template {
    pub id: Uuid,
    pub title: String,
    pub details: String,
    pub location_id: Option<Uuid>,
    pub priority: TaskPriority,
    pub schedule: Schedule,
    pub anchor_on: Date,
    /// The next occurrence not yet turned into a task.
    pub next_due_on: Date,
    pub lead_days: u8,
    pub catch_up: bool,
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn every_n_days_counts_from_the_first_occurrence() {
        let s = Schedule::Days { days: 3 };
        let anchor = date!(2026 - 09 - 01);
        assert_eq!(s.next_on_or_after(date!(2026 - 08 - 20), anchor), anchor);
        assert_eq!(s.next_on_or_after(date!(2026 - 09 - 02), anchor), date!(2026 - 09 - 04));
        assert_eq!(s.next_on_or_after(date!(2026 - 09 - 04), anchor), date!(2026 - 09 - 04));
        assert_eq!(
            s.between(date!(2026 - 09 - 01), date!(2026 - 09 - 10), anchor),
            vec![date!(2026 - 09 - 01), date!(2026 - 09 - 04), date!(2026 - 09 - 07), date!(2026 - 09 - 10)]
        );
        let daily = Schedule::Days { days: 1 };
        assert_eq!(daily.between(date!(2026 - 09 - 28), date!(2026 - 09 - 30), anchor).len(), 3);
    }

    #[test]
    fn weekly_falls_on_the_chosen_weekdays() {
        // 2026-09-29 is a Tuesday.
        let tuesdays = Schedule::Weekdays { on: WeekdaySet::of(&[Weekday::Tuesday]) };
        let any = date!(2000 - 01 - 01);
        assert_eq!(tuesdays.next_on_or_after(date!(2026 - 09 - 28), any), date!(2026 - 09 - 29));
        assert_eq!(tuesdays.next_on_or_after(date!(2026 - 09 - 30), any), date!(2026 - 10 - 06));
        let mwf = Schedule::Weekdays { on: WeekdaySet::of(&[Weekday::Monday, Weekday::Wednesday, Weekday::Friday]) };
        assert_eq!(
            mwf.between(date!(2026 - 09 - 28), date!(2026 - 10 - 04), any),
            vec![date!(2026 - 09 - 28), date!(2026 - 09 - 30), date!(2026 - 10 - 02)]
        );
    }

    #[test]
    fn monthly_on_the_31st_lands_on_each_months_last_day() {
        let s = Schedule::MonthDay { day: 31 };
        let any = date!(2000 - 01 - 01);
        assert_eq!(
            s.between(date!(2026 - 01 - 01), date!(2026 - 04 - 30), any),
            vec![date!(2026 - 01 - 31), date!(2026 - 02 - 28), date!(2026 - 03 - 31), date!(2026 - 04 - 30)]
        );
        assert_eq!(Schedule::MonthDay { day: 29 }.next_on_or_after(date!(2028 - 02 - 01), any), date!(2028 - 02 - 29));
        assert_eq!(Schedule::MonthDay { day: 1 }.next_on_or_after(date!(2026 - 12 - 02), any), date!(2027 - 01 - 01));
    }

    #[test]
    fn occurrences_are_strictly_increasing_and_on_schedule() {
        let any = date!(2026 - 01 - 03);
        for s in [
            Schedule::Days { days: 1 },
            Schedule::Days { days: 10 },
            Schedule::Weekdays { on: WeekdaySet(0b0101_0101) },
            Schedule::MonthDay { day: 30 },
        ] {
            let all = s.between(date!(2026 - 01 - 01), date!(2027 - 12 - 31), any);
            assert!(!all.is_empty(), "{s:?}");
            for w in all.windows(2) {
                assert!(w[0] < w[1], "{s:?}: {w:?}");
                assert_eq!(s.next_on_or_after(w[0], any), w[0], "{s:?}: {} is an occurrence", w[0]);
                assert_eq!(s.next_on_or_after(w[0].next_day().expect("day"), any), w[1], "{s:?}: nothing between");
            }
        }
    }

    #[test]
    fn missed_occurrences_collapse_unless_each_one_matters() {
        let due = [date!(2026 - 09 - 25), date!(2026 - 09 - 26), date!(2026 - 09 - 27)];
        assert_eq!(to_create(&due, false), &[date!(2026 - 09 - 27)]);
        assert_eq!(to_create(&due, true), &due);
        assert!(to_create(&[], false).is_empty());
    }

    #[test]
    fn schedules_and_templates_are_validated() {
        assert!(Schedule::Days { days: 0 }.validated().is_err());
        assert!(Schedule::Weekdays { on: WeekdaySet(0) }.validated().is_err());
        assert!(Schedule::MonthDay { day: 32 }.validated().is_err());
        let t = NewTemplate {
            title: " Feed hens ".into(),
            details: String::new(),
            location_id: None,
            priority: TaskPriority::Normal,
            schedule: Schedule::Weekdays { on: WeekdaySet::of(&[Weekday::Tuesday]) },
            starts_on: date!(2026 - 09 - 28),
            lead_days: 0,
            catch_up: false,
        };
        let ok = t.clone().validated().expect("ok");
        assert_eq!((ok.title.as_str(), ok.first_occurrence()), ("Feed hens", date!(2026 - 09 - 29)));
        assert!(NewTemplate { lead_days: 31, ..t.clone() }.validated().is_err());
        assert!(NewTemplate { title: " ".into(), ..t }.validated().is_err());
        assert_eq!(Schedule::Weekdays { on: WeekdaySet::of(&[Weekday::Tuesday]) }.describe(), "every Tuesday");
    }

    #[test]
    fn schedules_serialise_as_tagged_objects() {
        let s: Schedule = serde_json::from_str(r#"{"every":"weekdays","on":2}"#).expect("json");
        assert_eq!(s, Schedule::Weekdays { on: WeekdaySet::of(&[Weekday::Tuesday]) });
        let s: Schedule = serde_json::from_str(r#"{"every":"days","days":1}"#).expect("json");
        assert_eq!(s.describe(), "every day");
    }
}

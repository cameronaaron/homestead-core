//! Deadline reminders: when a task's due date is close enough to post
//! about, and how a batch of them reads.
//!
//! A reminder goes out once per stage per due date. Stages only get more
//! urgent as days pass, so a task sees at most five posts: two weeks out,
//! the week of, the day before, the day, and once when overdue. A server
//! that slept through a stage posts only the stage it wakes up in, the
//! way routines make only the latest missed occurrence. Moving the due
//! date starts over, since the old reminders were about a date that no
//! longer holds.

use serde::{Deserialize, Serialize};
use time::Date;

use crate::ValidationError;

string_enum! {
    /// How close a due date is.
    Stage, "stage" {
        TwoWeeks => "two_weeks",
        Week => "week",
        Tomorrow => "tomorrow",
        Today => "today",
        Overdue => "overdue",
    }
}

impl Stage {
    /// The stage a task due in `days_left` days is in (negative when
    /// overdue), or `None` while it's too far off to mention.
    pub fn for_days_left(days_left: i64) -> Option<Stage> {
        match days_left {
            ..0 => Some(Stage::Overdue),
            0 => Some(Stage::Today),
            1 => Some(Stage::Tomorrow),
            2..=7 => Some(Stage::Week),
            8..=14 => Some(Stage::TwoWeeks),
            _ => None,
        }
    }
}

/// Tasks further out than this aren't looked at.
pub const HORIZON_DAYS: i64 = 14;

/// One task in a reminder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DueItem {
    pub title: String,
    pub due_on: Date,
    /// Days from the reminder's date to `due_on`; negative when overdue.
    pub days_left: i64,
    /// `SUN-46` and its link, for a task that came from Linear.
    pub issue: Option<String>,
    pub url: Option<String>,
    /// Who it's assigned to in Linear; empty when nobody.
    pub assignee: String,
}

impl DueItem {
    fn when(&self) -> String {
        let date = short_date(self.due_on);
        match self.days_left {
            ..-1 => format!("**overdue** since {date}"),
            -1 => format!("**overdue** since yesterday, {date}"),
            0 => format!("due **today**, {date}"),
            1 => format!("due **tomorrow**, {date}"),
            n => format!("due in {n} days, {date}"),
        }
    }

    /// One bullet: title (linked when it has an issue), when, and who.
    pub fn line(&self) -> String {
        let title = match (&self.issue, &self.url) {
            (Some(id), Some(url)) if !url.is_empty() => format!("{} ([{id}](<{url}>))", self.title),
            (Some(id), _) => format!("{} ({id})", self.title),
            _ => self.title.clone(),
        };
        let who = if self.assignee.is_empty() { String::new() } else { format!(" · {}", self.assignee) };
        format!("• {title}: {}{who}", self.when())
    }
}

/// "Thu Nov 12", as a chat message says a date.
pub fn short_date(d: Date) -> String {
    format!("{} {} {}", &d.weekday().to_string()[..3], month(d), d.day())
}

fn month(d: Date) -> &'static str {
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][d.month() as usize - 1]
}

/// A batch of reminders, most urgent first.
pub fn summary(project: &str, items: &[DueItem]) -> String {
    let heading = if project.is_empty() { "Deadlines coming up".to_owned() } else { format!("{project}: deadlines") };
    let mut out = heading;
    for item in items {
        out.push('\n');
        out.push_str(&item.line());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn stages_follow_the_calendar_and_stop_at_the_horizon() {
        let at = |d| Stage::for_days_left(d);
        assert_eq!(at(HORIZON_DAYS + 1), None, "nothing more than two weeks out");
        assert_eq!(at(HORIZON_DAYS), Some(Stage::TwoWeeks));
        assert_eq!(at(8), Some(Stage::TwoWeeks));
        assert_eq!(at(7), Some(Stage::Week));
        assert_eq!(at(2), Some(Stage::Week));
        assert_eq!(at(1), Some(Stage::Tomorrow));
        assert_eq!(at(0), Some(Stage::Today));
        assert_eq!(at(-1), Some(Stage::Overdue));
        assert_eq!(at(-30), Some(Stage::Overdue));
        // Counting down never returns to a stage already passed, which is
        // what makes "once per stage" mean "at most five posts".
        let mut seen: Vec<Stage> = Vec::new();
        for d in (-3..=20).rev() {
            if let Some(s) = at(d)
                && seen.last() != Some(&s)
            {
                assert!(!seen.contains(&s), "{s} came back at {d} days");
                seen.push(s);
            }
        }
        assert_eq!(seen.len(), Stage::ALL.len());
    }

    #[test]
    fn stage_strings_match_the_migration() {
        let sql = include_str!("../../../migrations/0049_deadline_reminders.sql");
        crate::tests::assert_check_list(sql, "stage", Stage::ALL, Stage::as_str);
    }

    #[test]
    fn a_reminder_names_the_task_the_date_and_who_has_it() {
        let item = |days_left, issue: Option<&str>, assignee: &str| DueItem {
            title: "Submit the grant application".into(),
            due_on: date!(2026 - 11 - 12),
            days_left,
            issue: issue.map(str::to_owned),
            url: issue.map(|i| format!("https://linear.app/sunbrewedhomestead/issue/{i}")),
            assignee: assignee.into(),
        };
        assert_eq!(
            item(7, Some("SUN-46"), "Ava").line(),
            "• Submit the grant application ([SUN-46](<https://linear.app/sunbrewedhomestead/issue/SUN-46>)): \
             due in 7 days, Thu Nov 12 · Ava"
        );
        assert_eq!(item(1, None, "").line(), "• Submit the grant application: due **tomorrow**, Thu Nov 12");
        assert_eq!(item(0, None, "").line(), "• Submit the grant application: due **today**, Thu Nov 12");
        assert_eq!(
            item(-1, None, "").line(),
            "• Submit the grant application: **overdue** since yesterday, Thu Nov 12"
        );
        assert_eq!(item(-3, None, "").line(), "• Submit the grant application: **overdue** since Thu Nov 12");
        let s = summary("Grant writing", &[item(7, None, ""), item(0, None, "")]);
        assert_eq!(s.lines().count(), 3);
        assert!(s.starts_with("Grant writing: deadlines\n• "));
        assert!(summary("", &[item(3, None, "")]).starts_with("Deadlines coming up\n"));
    }
}

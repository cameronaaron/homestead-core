//! The morning digest: one Discord post a day saying what needs doing.
//!
//! Deadline reminders (see [`crate::deadlines`]) speak up about single
//! tasks as their dates approach. The digest is the other half: once each
//! morning, how much is overdue, due today and urgent, and what's low. It
//! carries tasks and stock only, since those are what everyone reading
//! #farm-feed may see; incidents and animal care stay in Homestead.

use serde::{Deserialize, Serialize};
use time::Date;

use crate::TaskPriority;

/// The farm-time hours in which the digest goes out. The core sleeps when
/// idle, so "at 7" means "the first time it's awake from 7"; a core that
/// sleeps all morning skips the day rather than posting a morning summary
/// at dinner time.
pub const HOURS: std::ops::Range<u8> = 7..12;

/// Lines shown per section; the rest is counted.
pub const SECTION_LINES: usize = 8;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DigestTask {
    pub title: String,
    pub due_on: Option<Date>,
    pub priority: TaskPriority,
    /// Who has it; empty when anyone can take it.
    pub assignee: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Digest {
    pub on: Date,
    /// Open tasks due before `on`, oldest first.
    pub overdue: Vec<DigestTask>,
    pub due_today: Vec<DigestTask>,
    /// Urgent or high tasks not already overdue or due today.
    pub high_priority: Vec<DigestTask>,
    /// Items at or below their reorder level, as "Layer feed (1 bag)".
    pub low_stock: Vec<String>,
}

impl Digest {
    /// Nothing to say: the digest isn't posted.
    pub fn is_empty(&self) -> bool {
        self.overdue.is_empty()
            && self.due_today.is_empty()
            && self.high_priority.is_empty()
            && self.low_stock.is_empty()
    }

    /// "3 overdue · 5 due today · 2 high priority · 1 low on stock", with
    /// empty counts left out.
    pub fn headline(&self) -> String {
        let parts = [
            (self.overdue.len(), "overdue"),
            (self.due_today.len(), "due today"),
            (self.high_priority.len(), "high priority"),
            (self.low_stock.len(), "low on stock"),
        ];
        parts.iter().filter(|(n, _)| *n > 0).map(|(n, what)| format!("{n} {what}")).collect::<Vec<_>>().join(" · ")
    }

    /// The whole post.
    pub fn render(&self) -> String {
        let mut out = format!("**{}**: {}", crate::deadlines::short_date(self.on), self.headline());
        let tasks = |out: &mut String, heading: &str, items: &[DigestTask], dated: bool| {
            let lines = items.iter().map(|t| {
                let mut line = format!("• {}", t.title);
                if matches!(t.priority, TaskPriority::Urgent | TaskPriority::High) {
                    line.push_str(&format!(" ({})", t.priority));
                }
                if dated && let Some(d) = t.due_on {
                    line.push_str(&format!(", {}", crate::deadlines::short_date(d)));
                }
                line.push_str(&if t.assignee.is_empty() {
                    " · anyone".to_owned()
                } else {
                    format!(" · {}", t.assignee)
                });
                line
            });
            section(out, heading, lines, items.len());
        };
        tasks(&mut out, "Overdue", &self.overdue, true);
        tasks(&mut out, "Due today", &self.due_today, false);
        tasks(&mut out, "High priority", &self.high_priority, true);
        section(&mut out, "Low on stock", self.low_stock.iter().map(|s| format!("• {s}")), self.low_stock.len());
        out
    }
}

fn section(out: &mut String, heading: &str, lines: impl Iterator<Item = String>, total: usize) {
    if total == 0 {
        return;
    }
    out.push_str(&format!("\n\n**{heading}**"));
    for line in lines.take(SECTION_LINES) {
        out.push('\n');
        out.push_str(&line);
    }
    if total > SECTION_LINES {
        out.push_str(&format!("\n…and {} more in Homestead", total - SECTION_LINES));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn t(title: &str, due_on: Option<Date>, priority: TaskPriority, assignee: &str) -> DigestTask {
        DigestTask { title: title.into(), due_on, priority, assignee: assignee.into() }
    }

    fn empty() -> Digest {
        Digest {
            on: date!(2026 - 09 - 30),
            overdue: vec![],
            due_today: vec![],
            high_priority: vec![],
            low_stock: vec![],
        }
    }

    #[test]
    fn the_digest_leads_with_counts_and_names_who_has_each_task() {
        let d = Digest {
            overdue: vec![t("Mend the fence", Some(date!(2026 - 09 - 28)), TaskPriority::Normal, "Ava")],
            due_today: vec![t("Fix irrigation at Bed 3", Some(date!(2026 - 09 - 30)), TaskPriority::High, "")],
            low_stock: vec!["Layer feed (1 bag)".into()],
            ..empty()
        };
        assert_eq!(
            d.render(),
            "**Wed Sep 30**: 1 overdue · 1 due today · 1 low on stock\n\n\
             **Overdue**\n• Mend the fence, Mon Sep 28 · Ava\n\n\
             **Due today**\n• Fix irrigation at Bed 3 (high) · anyone\n\n\
             **Low on stock**\n• Layer feed (1 bag)"
        );
        assert!(!d.is_empty());
        assert!(empty().is_empty(), "a quiet day posts nothing");
    }

    #[test]
    fn a_long_section_is_cut_short_and_counted() {
        let many: Vec<_> =
            (0..SECTION_LINES + 3).map(|i| t(&format!("Task {i}"), None, TaskPriority::Normal, "")).collect();
        let d = Digest { overdue: many, ..empty() };
        let text = d.render();
        assert!(text.contains(&format!("{} overdue", SECTION_LINES + 3)), "the count is the whole count");
        assert_eq!(text.matches("• Task").count(), SECTION_LINES);
        assert!(text.ends_with("…and 3 more in Homestead"), "{text}");
    }
}

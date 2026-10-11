//! QR tags on physical things, and the tool loans they make easy.
//!
//! A tag's code is printed as a QR code linking to `/t/{code}`. Whoever
//! holds the tag may do the handful of things that make sense for what it's
//! stuck to, without an account: most of the people doing the work are in
//! the group chat, not in this app. So the code is a credential (like a
//! setup link) and each public action is small, bounded and attributed to
//! the name the person typed.

use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::ValidationError;
use crate::auth::Capability;

string_enum!(TagKind, "kind" {
    Equipment => "equipment", InventoryItem => "inventory_item", Location => "location", Animal => "animal",
    Gate => "gate",
});

impl TagKind {
    /// What it takes to make, print or retire a tag of this kind: the
    /// right to change the thing it's stuck to.
    pub fn manage_cap(self) -> Capability {
        match self {
            TagKind::Equipment => Capability::EquipmentWrite,
            TagKind::InventoryItem => Capability::InventoryAdjust,
            TagKind::Location => Capability::GardenWrite,
            TagKind::Animal => Capability::AnimalsWrite,
            TagKind::Gate => Capability::CommunityWrite,
        }
    }
}

/// Crockford-style: no 0/O or 1/I/L, so a code read aloud or typed from
/// a faded label can't be mistaken.
pub const CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
pub const CODE_LEN: usize = 10;

/// A fresh code from a source of random bytes. Bytes at or above the
/// largest multiple of the alphabet's size are skipped, so every character
/// is equally likely (a plain `% 31` would favour the first eight).
pub fn new_code(mut random_byte: impl FnMut() -> u8) -> String {
    let n = CODE_ALPHABET.len();
    let limit = 256 - 256 % n;
    let mut out = String::with_capacity(CODE_LEN);
    while out.len() < CODE_LEN {
        let b = usize::from(random_byte());
        if b < limit {
            out.push(char::from(CODE_ALPHABET[b % n]));
        }
    }
    out
}

/// A code as someone might type it (lower case, with spaces or dashes), or
/// `None` if it can't be one.
pub fn normalise_code(s: &str) -> Option<String> {
    let c: String = s.chars().filter(|c| !matches!(c, ' ' | '-')).collect::<String>().to_ascii_uppercase();
    (c.len() == CODE_LEN && c.bytes().all(|b| CODE_ALPHABET.contains(&b))).then_some(c)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tag {
    pub code: String,
    pub kind: TagKind,
    /// The tool, item, place or animal; `None` for the gate.
    pub target_id: Option<Uuid>,
    /// What's printed under the code.
    pub label: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The longest name someone can give at a tag, and the longest note.
pub const MAX_NAME: usize = 80;
pub const MAX_NOTE: usize = 500;

/// A name typed at a tag: trimmed, not blank, not a novel.
pub fn visitor_name(s: &str) -> Result<String, ValidationError> {
    let t = s.trim();
    if t.is_empty() {
        return Err(ValidationError::Blank("your name"));
    }
    if t.chars().count() > MAX_NAME {
        return Err(ValidationError::Malformed("your name (80 characters at most)"));
    }
    Ok(t.to_owned())
}

/// A free-text note from a tag: trimmed and bounded.
pub fn note(field: &'static str, s: &str) -> Result<String, ValidationError> {
    let t = s.trim();
    if t.chars().count() > MAX_NOTE {
        return Err(ValidationError::Malformed(field));
    }
    Ok(t.to_owned())
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Loan {
    pub id: Uuid,
    pub equipment_id: Uuid,
    pub equipment_name: String,
    pub borrower: String,
    #[serde(with = "time::serde::rfc3339")]
    pub out_at: OffsetDateTime,
    pub due_on: Option<Date>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub returned_at: Option<OffsetDateTime>,
    pub return_note: String,
}

impl Loan {
    pub fn overdue(&self, today: Date) -> bool {
        self.returned_at.is_none() && self.due_on.is_some_and(|d| d < today)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewLoan {
    pub borrower: String,
    #[serde(default)]
    pub due_on: Option<Date>,
}

impl NewLoan {
    pub fn validated(self, today: Date) -> Result<Self, ValidationError> {
        if self.due_on.is_some_and(|d| d < today) {
            return Err(ValidationError::Malformed("due date (it's already past)"));
        }
        Ok(Self { borrower: visitor_name(&self.borrower)?, ..self })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn codes_use_the_whole_alphabet_evenly_and_nothing_else() {
        let mut counter = 0u8;
        let mut counts = [0usize; 256];
        for _ in 0..3100 {
            let code = new_code(|| {
                counter = counter.wrapping_add(1);
                counter
            });
            assert_eq!(normalise_code(&code).as_deref(), Some(code.as_str()));
            for b in code.bytes() {
                counts[usize::from(b)] += 1;
            }
        }
        // Every byte value comes up equally often from the counter, so
        // after rejection each character must too.
        let used: Vec<usize> = CODE_ALPHABET.iter().map(|b| counts[usize::from(*b)]).collect();
        assert!(used.iter().all(|n| *n == used[0]), "uneven: {used:?}");
        assert_eq!(used.iter().sum::<usize>(), 31_000);
    }

    #[test]
    fn a_skipped_byte_is_skipped_not_wrapped() {
        // 248..=255 would favour '2'..'9' under a plain modulo.
        let mut bytes = [255u8, 248, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0].into_iter();
        assert_eq!(new_code(|| bytes.next().expect("enough")), "2222222222");
    }

    #[test]
    fn typed_codes_are_normalised_and_confusable_letters_refused() {
        assert_eq!(normalise_code("abcd-efgh 23").as_deref(), Some("ABCDEFGH23"));
        assert_eq!(normalise_code("ABCDEFGH2O"), None, "O is not in the alphabet");
        assert_eq!(normalise_code("ABCDEFGH21"), None);
        assert_eq!(normalise_code("ABCDEFGH2"), None);
        assert_eq!(normalise_code("../../etc"), None);
    }

    #[test]
    fn names_and_notes_at_a_tag_are_bounded() {
        assert_eq!(visitor_name("  Sam "), Ok("Sam".into()));
        assert!(visitor_name(" ").is_err());
        assert!(visitor_name(&"x".repeat(81)).is_err());
        assert!(note("note", &"x".repeat(501)).is_err());
        assert_eq!(note("note", " blade chipped "), Ok("blade chipped".into()));
    }

    #[test]
    fn loans_are_due_in_the_future_and_overdue_after() {
        let today = date!(2026 - 09 - 29);
        let late = NewLoan { borrower: "Sam".into(), due_on: Some(date!(2026 - 09 - 28)) };
        assert!(late.validated(today).is_err());
        let ok = NewLoan { borrower: " Sam ".into(), due_on: Some(today) }.validated(today).expect("ok");
        assert_eq!(ok.borrower, "Sam");
        let loan = Loan {
            id: Uuid::nil(),
            equipment_id: Uuid::nil(),
            equipment_name: "Trap 3".into(),
            borrower: "Sam".into(),
            out_at: OffsetDateTime::UNIX_EPOCH,
            due_on: Some(today),
            returned_at: None,
            return_note: String::new(),
        };
        assert!(!loan.overdue(today) && loan.overdue(today.next_day().expect("date")));
    }

    #[test]
    fn kinds_match_the_migration_and_each_needs_its_things_write() {
        let sql = include_str!("../../../migrations/0034_tags_and_loans.sql");
        crate::tests::assert_check_list(sql, "kind", TagKind::ALL, TagKind::as_str);
        assert_eq!(TagKind::Equipment.manage_cap(), Capability::EquipmentWrite);
        assert_eq!(TagKind::Gate.manage_cap(), Capability::CommunityWrite);
    }
}

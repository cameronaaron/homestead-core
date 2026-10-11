//! Where an animal or a piece of equipment has been. Each move is recorded
//! by the database whenever its current place changes; this turns the
//! moves into stays ("Pasture A, 14 days").

use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::ValidationError;

/// What moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum Mover {
    Animal(Uuid),
    Equipment(Uuid),
}

/// A move entered by a person: where to, and when if not now.
#[derive(Debug, Clone, PartialEq)]
pub struct NewMove {
    /// `None`: it's gone somewhere untracked (off the farm, not recorded).
    pub to: Option<Uuid>,
    pub at: Option<OffsetDateTime>,
    pub note: String,
}

impl NewMove {
    pub fn validated(self) -> Result<Self, ValidationError> {
        let note = self.note.split_whitespace().collect::<Vec<_>>().join(" ");
        if note.chars().count() > 200 {
            return Err(ValidationError::Malformed("note (200 characters at most)"));
        }
        Ok(Self { note, ..self })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Movement {
    pub id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub moved_at: OffsetDateTime,
    pub mover: Mover,
    /// The animal's or machine's name.
    pub mover_name: String,
    pub from: Option<(Uuid, String)>,
    pub to: Option<(Uuid, String)>,
    pub note: String,
}

/// A stretch of time in one place (or nowhere recorded).
#[derive(Debug, Clone, PartialEq)]
pub struct Stay {
    pub place: Option<(Uuid, String)>,
    pub from: OffsetDateTime,
    /// `None`: still there.
    pub until: Option<OffsetDateTime>,
    pub note: String,
}

impl Stay {
    /// Whole days, to `now` for a stay that hasn't ended.
    pub fn days(&self, now: OffsetDateTime) -> i64 {
        (self.until.unwrap_or(now) - self.from).whole_days()
    }
}

/// One thing's moves as stays, newest first: each move starts a stay at
/// its destination that lasts until the next move.
pub fn stays(moves: &[Movement]) -> Vec<Stay> {
    let mut sorted: Vec<&Movement> = moves.iter().collect();
    sorted.sort_by_key(|m| m.moved_at);
    let mut out: Vec<Stay> = sorted
        .iter()
        .enumerate()
        .map(|(i, m)| Stay {
            place: m.to.clone(),
            from: m.moved_at,
            until: sorted.get(i + 1).map(|n| n.moved_at),
            note: m.note.clone(),
        })
        .collect();
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn mv(at: OffsetDateTime, from: Option<&str>, to: Option<&str>) -> Movement {
        let place = |n: &str| (Uuid::from_u128(n.bytes().map(u128::from).sum()), n.to_owned());
        Movement {
            id: Uuid::new_v4(),
            moved_at: at,
            mover: Mover::Animal(Uuid::nil()),
            mover_name: "Clover".into(),
            from: from.map(place),
            to: to.map(place),
            note: String::new(),
        }
    }

    #[test]
    fn a_goats_moves_become_stays() {
        let moves = [
            mv(datetime!(2026-09-15 9:00 UTC), Some("Pasture B"), Some("Barn")),
            mv(datetime!(2026-08-01 9:00 UTC), None, Some("Pasture A")),
            mv(datetime!(2026-08-15 9:00 UTC), Some("Pasture A"), Some("Pasture B")),
        ];
        let s = stays(&moves);
        let names: Vec<_> = s.iter().map(|s| s.place.as_ref().map(|p| p.1.as_str())).collect();
        assert_eq!(names, [Some("Barn"), Some("Pasture B"), Some("Pasture A")], "newest first");
        let now = datetime!(2026-09-29 9:00 UTC);
        assert_eq!(s.iter().map(|s| s.days(now)).collect::<Vec<_>>(), [14, 31, 14]);
        assert_eq!(s[0].until, None, "still in the barn");
        assert_eq!(s[1].until, Some(datetime!(2026-09-15 9:00 UTC)));
    }

    #[test]
    fn leaving_for_nowhere_recorded_is_a_stay_too() {
        let s = stays(&[
            mv(datetime!(2026-09-01 9:00 UTC), None, Some("Tool shed")),
            mv(datetime!(2026-09-10 9:00 UTC), Some("Tool shed"), None),
        ]);
        assert_eq!(s[0].place, None, "out for repair, somewhere untracked");
        assert_eq!(s[1].days(datetime!(2026-12-01 0:00 UTC)), 9);
        assert!(stays(&[]).is_empty());
    }

    #[test]
    fn notes_are_tidied_and_bounded() {
        let ok = NewMove { to: None, at: None, note: "  to the  vet \n".into() }.validated().expect("ok");
        assert_eq!(ok.note, "to the vet");
        assert!(NewMove { to: None, at: None, note: "x".repeat(201) }.validated().is_err());
    }
}

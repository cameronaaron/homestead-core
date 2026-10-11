//! The mailing list.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{ValidationError, non_blank};

string_enum!(Interest, "interests" {
    Newsletter => "newsletter", Volunteering => "volunteering", Workshops => "workshops",
    CatAdoption => "cat_adoption", Donating => "donating",
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contact {
    pub id: Uuid,
    pub name: String,
    pub email: String,
    pub interests: Vec<Interest>,
    pub source: String,
    #[serde(with = "time::serde::rfc3339")]
    pub consented_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub unsubscribed_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewContact {
    #[serde(default)]
    pub name: String,
    pub email: String,
    #[serde(default)]
    pub interests: Vec<Interest>,
    pub source: String,
}

/// Lower-cases and checks an email address the way the database does.
pub fn normalise_email(raw: &str) -> Result<String, ValidationError> {
    let e = raw.trim().to_lowercase();
    let ok = match e.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !domain.contains('@')
                && !e.contains(char::is_whitespace)
        }
        None => false,
    };
    if ok { Ok(e) } else { Err(ValidationError::Malformed("email")) }
}

impl NewContact {
    pub fn validated(self) -> Result<Self, ValidationError> {
        let mut interests = self.interests;
        interests.sort_by_key(|i| i.as_str());
        interests.dedup();
        Ok(Self {
            name: self.name.trim().to_owned(),
            email: normalise_email(&self.email)?,
            interests,
            source: non_blank("source", &self.source)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_strings_match_migration() {
        // The list is an array literal: `array['newsletter', …]`.
        let sql = include_str!("../../../migrations/0010_contacts.sql");
        let start = sql.find("array[").expect("array") + "array[".len();
        let list = &sql[start..start + sql[start..].find(']').expect("]")];
        let mut db: Vec<&str> = list.split(',').map(|x| x.trim().trim_matches('\'')).collect();
        let mut ours: Vec<&str> = Interest::ALL.iter().map(|i| i.as_str()).collect();
        db.sort();
        ours.sort();
        assert_eq!(db, ours);
    }

    #[test]
    fn emails_are_normalised_and_checked() {
        assert_eq!(normalise_email("  Kiki@Example.ORG "), Ok("kiki@example.org".into()));
        for bad in ["", "kiki", "kiki@", "@example.org", "kiki@example", "ki ki@example.org", "a@b@c.example", "a@.org"]
        {
            assert!(normalise_email(bad).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn interests_are_deduplicated() {
        let c = NewContact {
            name: String::new(),
            email: "a@b.example".into(),
            interests: vec![Interest::Workshops, Interest::Newsletter, Interest::Workshops],
            source: "website".into(),
        }
        .validated()
        .expect("ok");
        assert_eq!(c.interests, [Interest::Newsletter, Interest::Workshops]);
    }
}

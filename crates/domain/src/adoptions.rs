//! Adoption applications, from the public page to a decision.

use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::ValidationError;
use crate::animals::{Sex, Species};

string_enum!(ApplicationStatus, "status" {
    New => "new", Approved => "approved", Declined => "declined", Withdrawn => "withdrawn",
});

/// What the public page may say about an animal: never its notes, care
/// or where it came from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Adoptable {
    pub id: Uuid,
    pub name: String,
    pub species: Species,
    pub sex: Sex,
    pub born_on: Option<Date>,
    pub blurb: String,
}

impl Adoptable {
    /// "about 2 years", "about 5 months", or `None` when the birthday's
    /// unknown.
    pub fn age(&self, today: Date) -> Option<String> {
        let born = self.born_on?;
        let months = (today.year() - born.year()) * 12 + i32::from(u8::from(today.month()))
            - i32::from(u8::from(born.month()))
            - i32::from(today.day() < born.day());
        Some(match months {
            m if m < 1 => "under a month".into(),
            1 => "about a month".into(),
            m if m < 24 => format!("about {m} months"),
            m => format!("about {} years", m / 12),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewApplication {
    /// `None`: any animal that fits.
    #[serde(default)]
    pub animal_id: Option<Uuid>,
    pub applicant_name: String,
    pub email: String,
    #[serde(default)]
    pub phone: String,
    /// Their home, other pets, what they're looking for.
    #[serde(default)]
    pub about: String,
}

impl NewApplication {
    pub fn validated(self) -> Result<Self, ValidationError> {
        let bounded = |field, s: &str, max: usize| {
            let t = s.trim().to_owned();
            if t.chars().count() > max { Err(ValidationError::Malformed(field)) } else { Ok(t) }
        };
        let applicant_name = bounded("name (80 characters at most)", &self.applicant_name, 80)?;
        if applicant_name.is_empty() {
            return Err(ValidationError::Blank("your name"));
        }
        let email = crate::contacts::normalise_email(&self.email)?;
        if email.len() > 254 {
            return Err(ValidationError::Malformed("email"));
        }
        Ok(Self {
            applicant_name,
            email,
            phone: bounded("phone (40 characters at most)", &self.phone, 40)?,
            about: bounded("about you (4000 characters at most)", &self.about, 4000)?,
            ..self
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Application {
    pub id: Uuid,
    pub animal_id: Option<Uuid>,
    pub animal_name: Option<String>,
    pub applicant_name: String,
    pub email: String,
    pub phone: String,
    pub about: String,
    pub status: ApplicationStatus,
    #[serde(with = "time::serde::rfc3339")]
    pub received_at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn app() -> NewApplication {
        NewApplication {
            animal_id: None,
            applicant_name: " Randy ".into(),
            email: " Randy@Example.org ".into(),
            phone: "555-0100".into(),
            about: "Apartment, one other cat.".into(),
        }
    }

    #[test]
    fn applications_are_trimmed_and_bounded() {
        let ok = app().validated().expect("ok");
        assert_eq!((ok.applicant_name.as_str(), ok.email.as_str()), ("Randy", "randy@example.org"));
        assert!(NewApplication { applicant_name: " ".into(), ..app() }.validated().is_err());
        assert!(NewApplication { email: "nope".into(), ..app() }.validated().is_err());
        assert!(NewApplication { about: "x".repeat(4001), ..app() }.validated().is_err());
        let sql = include_str!("../../../migrations/0040_adoption_applications.sql");
        crate::tests::assert_check_list(sql, "status", ApplicationStatus::ALL, ApplicationStatus::as_str);
    }

    #[test]
    fn ages_read_the_way_a_person_says_them() {
        let a = |born_on| Adoptable {
            id: Uuid::nil(),
            name: "Mochi".into(),
            species: Species::Cat,
            sex: Sex::Female,
            born_on,
            blurb: String::new(),
        };
        let today = date!(2026 - 09 - 29);
        assert_eq!(a(None).age(today), None);
        assert_eq!(a(Some(date!(2026 - 09 - 10))).age(today).as_deref(), Some("under a month"));
        assert_eq!(a(Some(date!(2026 - 08 - 29))).age(today).as_deref(), Some("about a month"));
        assert_eq!(a(Some(date!(2026 - 03 - 30))).age(today).as_deref(), Some("about 5 months"));
        assert_eq!(a(Some(date!(2023 - 06 - 01))).age(today).as_deref(), Some("about 3 years"));
    }
}

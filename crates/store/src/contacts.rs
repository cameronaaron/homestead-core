//! The mailing list.

use homestead_domain::contacts::{Contact, Interest, NewContact};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Result, Store, parse};

type ContactRow = (Uuid, String, String, Vec<String>, String, OffsetDateTime, Option<OffsetDateTime>);

fn contact(r: ContactRow) -> Result<Contact> {
    let (id, name, email, interests, source, consented_at, unsubscribed_at) = r;
    let interests = interests.iter().map(|i| parse::<Interest>(i)).collect::<Result<_>>()?;
    Ok(Contact { id, name, email, interests, source, consented_at, unsubscribed_at })
}

/// What an import did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ImportReport {
    pub added: usize,
    pub updated: usize,
    /// Already unsubscribed: left unsubscribed, not re-added.
    pub kept_unsubscribed: usize,
}

const UPSERT: &str = "insert into homestead.contacts (name, email, interests, source)
     values ($1, $2, $3, $4)
     on conflict (email) do update set
       name = case when excluded.name <> '' then excluded.name else contacts.name end,
       interests = (select coalesce(array_agg(distinct i order by i), '{}')
                    from unnest(contacts.interests || excluded.interests) i)
     where contacts.unsubscribed_at is null
     returning (xmax = 0) as inserted";

impl Store {
    /// Adds someone who asked to hear from us, or merges their interests.
    /// Someone who unsubscribed stays unsubscribed: returns `None` for them.
    pub async fn add_contact(&self, input: NewContact) -> Result<Option<bool>> {
        let c = input.validated()?;
        let interests: Vec<&str> = c.interests.iter().map(|i| i.as_str()).collect();
        Ok(sqlx::query_scalar(UPSERT)
            .bind(&c.name)
            .bind(&c.email)
            .bind(&interests)
            .bind(&c.source)
            .fetch_optional(&self.pool)
            .await?)
    }

    /// Imports every contact or none (one bad row rejects the batch).
    pub async fn import_contacts(&self, inputs: Vec<NewContact>) -> Result<ImportReport> {
        let valid = inputs.into_iter().map(NewContact::validated).collect::<std::result::Result<Vec<_>, _>>()?;
        let mut tx = self.begin().await?;
        let mut report = ImportReport::default();
        for c in valid {
            let interests: Vec<&str> = c.interests.iter().map(|i| i.as_str()).collect();
            let inserted: Option<bool> = sqlx::query_scalar(UPSERT)
                .bind(&c.name)
                .bind(&c.email)
                .bind(&interests)
                .bind(&c.source)
                .fetch_optional(&mut *tx)
                .await?;
            match inserted {
                Some(true) => report.added += 1,
                Some(false) => report.updated += 1,
                None => report.kept_unsubscribed += 1,
            }
        }
        tx.commit().await?;
        Ok(report)
    }

    pub async fn unsubscribe_contact(&self, email: &str) -> Result<bool> {
        let email = homestead_domain::contacts::normalise_email(email)?;
        let mut tx = self.begin().await?;
        let n = sqlx::query(
            "update homestead.contacts set unsubscribed_at = coalesce(unsubscribed_at, now()) where email = $1",
        )
        .bind(email)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(n > 0)
    }

    /// Everyone still subscribed, optionally only those with an interest.
    pub async fn subscribed_contacts(&self, interest: Option<Interest>) -> Result<Vec<Contact>> {
        let rows: Vec<ContactRow> = sqlx::query_as(
            "select id, name, email, interests, source, consented_at, unsubscribed_at from homestead.contacts
             where unsubscribed_at is null and ($1::text is null or $1 = any(interests))
             order by lower(name), email",
        )
        .bind(interest.map(|i| i.as_str()))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(contact).collect()
    }

    pub async fn unsubscribed_count(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("select count(*) from homestead.contacts where unsubscribed_at is not null")
            .fetch_one(&self.pool)
            .await?)
    }
}

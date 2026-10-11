//! Adoption applications, and what the public page may show.

use homestead_domain::Event;
use homestead_domain::adoptions::{Adoptable, Application, ApplicationStatus, NewApplication};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::{Result, Store, StoreError, outbox, parse};

type ApplicationRow = (Uuid, Option<Uuid>, Option<String>, String, String, String, String, String, OffsetDateTime);

impl Store {
    /// Animals up for adoption, as the public may see them.
    pub async fn adoptable(&self) -> Result<Vec<Adoptable>> {
        let rows: Vec<(Uuid, String, String, String, Option<Date>, String)> = sqlx::query_as(
            "select id, name, species, sex, born_on, adoption_blurb from homestead.animals
             where status = 'adoptable' order by arrived_on, name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(id, name, species, sex, born_on, blurb)| {
                Ok(Adoptable { id, name, species: parse(&species)?, sex: parse(&sex)?, born_on, blurb })
            })
            .collect()
    }

    /// What the adoption page says about an animal.
    pub async fn set_adoption_blurb(&self, animal_id: Uuid, blurb: &str) -> Result<()> {
        let blurb = blurb.trim();
        if blurb.chars().count() > 1000 {
            return Err(homestead_domain::ValidationError::Malformed("adoption blurb (1000 characters at most)").into());
        }
        let mut tx = self.begin().await?;
        let n = sqlx::query("update homestead.animals set adoption_blurb = $2 where id = $1")
            .bind(animal_id)
            .bind(blurb)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        if n == 0 { Err(StoreError::NotFound("animal")) } else { Ok(()) }
    }

    /// Takes an application. Only for an animal that's adoptable now (or
    /// none, for "any"); a second submit of the same one is the first.
    pub async fn apply_to_adopt(&self, input: NewApplication) -> Result<Uuid> {
        let a = input.validated()?;
        let mut tx = self.begin().await?;
        let animal_name: Option<String> = match a.animal_id {
            Some(id) => Some(
                sqlx::query_scalar("select name from homestead.animals where id = $1 and status = 'adoptable'")
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or(StoreError::NotFound("adoptable animal"))?,
            ),
            None => None,
        };
        let id: Option<Uuid> = sqlx::query_scalar(
            "insert into homestead.adoption_applications (animal_id, applicant_name, email, phone, about)
             values ($1, $2, $3, $4, $5)
             on conflict (lower(email), coalesce(animal_id, '00000000-0000-0000-0000-000000000000'))
               where status = 'new' do nothing
             returning id",
        )
        .bind(a.animal_id)
        .bind(&a.applicant_name)
        .bind(&a.email)
        .bind(&a.phone)
        .bind(&a.about)
        .fetch_optional(&mut *tx)
        .await?;
        let id = match id {
            Some(id) => {
                outbox::emit(&mut tx, &Event::AdoptionApplied { animal_name }).await?;
                id
            }
            None => {
                sqlx::query_scalar(
                    "select id from homestead.adoption_applications
                 where lower(email) = lower($1) and animal_id is not distinct from $2 and status = 'new'",
                )
                .bind(&a.email)
                .bind(a.animal_id)
                .fetch_one(&mut *tx)
                .await?
            }
        };
        tx.commit().await?;
        Ok(id)
    }

    /// Applications, open ones first, newest first.
    pub async fn list_applications(&self, limit: i64) -> Result<Vec<Application>> {
        let rows: Vec<ApplicationRow> = sqlx::query_as(
            "select p.id, p.animal_id, a.name, p.applicant_name, p.email, p.phone, p.about, p.status, p.received_at
             from homestead.adoption_applications p left join homestead.animals a on a.id = p.animal_id
             order by p.status <> 'new', p.received_at desc limit $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(id, animal_id, animal_name, applicant_name, email, phone, about, status, received_at)| {
                Ok(Application {
                    id,
                    animal_id,
                    animal_name,
                    applicant_name,
                    email,
                    phone,
                    about,
                    status: parse(&status)?,
                    received_at,
                })
            })
            .collect()
    }

    /// Approves, declines or withdraws an open application.
    pub async fn decide_application(&self, id: Uuid, status: ApplicationStatus) -> Result<()> {
        if status == ApplicationStatus::New {
            return Err(homestead_domain::ValidationError::Malformed("status (decide it one way or another)").into());
        }
        let mut tx = self.begin().await?;
        let n = sqlx::query(
            "update homestead.adoption_applications set status = $2, decided_at = now()
             where id = $1 and status = 'new'",
        )
        .bind(id)
        .bind(status.as_str())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        if n == 0 { Err(StoreError::Conflict("that application was already decided".into())) } else { Ok(()) }
    }
}

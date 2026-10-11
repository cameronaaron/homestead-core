//! Animals, their care, and where they went.

use homestead_domain::animals::{
    Animal, AnimalStatus, CareEntry, CareKind, CareSchedule, CatClubNumbers, Colony, ColonyProgress, Housing, Identity,
    NewAnimal, NewCareEntry, NewCareSchedule, NewColony, NewPlacement, Placement, PlacementKind, Species, next_due,
    normalise_chip,
};
use homestead_domain::{Event, NewTask, TaskPriority};
use rust_decimal::Decimal;
use time::Date;
use uuid::Uuid;

use crate::{Result, Store, StoreError, outbox, parse};

macro_rules! animal_cols {
    () => {
        "id, species, name, head_count, sex, status, origin, arrived_on, born_on, microchip, notes,
         chip_registry, chip_registered_to, band, home_location_id, colony_id, ear_tipped, adoption_blurb"
    };
}

#[derive(sqlx::FromRow)]
struct AnimalRow {
    id: Uuid,
    species: String,
    name: String,
    head_count: i32,
    sex: String,
    status: String,
    origin: String,
    arrived_on: Date,
    born_on: Option<Date>,
    microchip: String,
    notes: String,
    chip_registry: String,
    chip_registered_to: String,
    band: String,
    home_location_id: Option<Uuid>,
    colony_id: Option<Uuid>,
    ear_tipped: bool,
    adoption_blurb: String,
}

fn animal(r: AnimalRow) -> Result<Animal> {
    Ok(Animal {
        id: r.id,
        species: parse(&r.species)?,
        name: r.name,
        head_count: r.head_count,
        sex: parse(&r.sex)?,
        status: parse(&r.status)?,
        origin: parse(&r.origin)?,
        arrived_on: r.arrived_on,
        born_on: r.born_on,
        microchip: r.microchip,
        notes: r.notes,
        chip_registry: r.chip_registry,
        chip_registered_to: parse(&r.chip_registered_to)?,
        band: r.band,
        home_location_id: r.home_location_id,
        colony_id: r.colony_id,
        ear_tipped: r.ear_tipped,
        adoption_blurb: r.adoption_blurb,
    })
}

/// A chip number already on another animal is a mistake worth naming.
fn chip_taken(e: sqlx::Error) -> StoreError {
    match StoreError::from(e) {
        StoreError::Conflict(_) => StoreError::Conflict("that microchip is already recorded on another animal".into()),
        other => other,
    }
}

impl Store {
    pub async fn create_animal(&self, input: NewAnimal) -> Result<Animal> {
        let a = input.validated()?;
        let mut tx = self.begin().await?;
        let row: AnimalRow = sqlx::query_as(concat!(
            "insert into homestead.animals
               (species, name, head_count, sex, status, origin, arrived_on, born_on, microchip, notes)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) returning ",
            animal_cols!()
        ))
        .bind(a.species.as_str())
        .bind(&a.name)
        .bind(a.head_count)
        .bind(a.sex.as_str())
        .bind(a.status.as_str())
        .bind(a.origin.as_str())
        .bind(a.arrived_on)
        .bind(a.born_on)
        .bind(&a.microchip)
        .bind(&a.notes)
        .fetch_one(&mut *tx)
        .await
        .map_err(chip_taken)?;
        tx.commit().await?;
        animal(row)
    }

    /// Where an animal lives (`None`: not recorded).
    pub async fn set_animal_home(&self, id: Uuid, home: Option<Uuid>) -> Result<()> {
        let mut tx = self.begin().await?;
        let n = sqlx::query("update homestead.animals set home_location_id = $2 where id = $1")
            .bind(id)
            .bind(home)
            .execute(&mut *tx)
            .await
            .map_err(|e| match StoreError::from(e) {
                StoreError::NotFound(_) => StoreError::NotFound("place"),
                other => other,
            })?
            .rows_affected();
        tx.commit().await?;
        if n == 0 { Err(StoreError::NotFound("animal")) } else { Ok(()) }
    }

    /// How many head of animals a place holds (`None`: not set).
    pub async fn set_capacity(&self, location_id: Uuid, capacity: Option<i32>) -> Result<()> {
        if capacity.is_some_and(|c| c < 1) {
            return Err(homestead_domain::ValidationError::NotPositive("capacity").into());
        }
        let mut tx = self.begin().await?;
        let n = sqlx::query("update homestead.locations set capacity = $2 where id = $1")
            .bind(location_id)
            .bind(capacity)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        if n == 0 { Err(StoreError::NotFound("place")) } else { Ok(()) }
    }

    /// Every place with a capacity or with animals living there, against
    /// the head of animals still in our care there. Most crowded first.
    pub async fn housing(&self) -> Result<Vec<Housing>> {
        let rows: Vec<(Uuid, String, Option<i32>, i64)> = sqlx::query_as(
            "select l.id, l.name, l.capacity,
                    coalesce(sum(a.head_count) filter (where a.status in ('resident', 'in_care', 'adoptable')), 0)::bigint
             from homestead.locations l
             left join homestead.animals a on a.home_location_id = l.id
             group by l.id
             having l.capacity is not null or count(a.id) > 0
             order by coalesce(sum(a.head_count) filter (where a.status in ('resident', 'in_care', 'adoptable')), 0)
                        - coalesce(l.capacity, 1000000) desc, l.name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(location_id, name, capacity, head)| Housing { location_id, name, capacity, head })
            .collect())
    }

    /// The animal with this chip, however the number was typed or scanned.
    pub async fn find_by_chip(&self, chip: &str) -> Result<Option<Animal>> {
        let chip = normalise_chip(chip)?;
        if chip.is_empty() {
            return Ok(None);
        }
        let row: Option<AnimalRow> =
            sqlx::query_as(concat!("select ", animal_cols!(), " from homestead.animals where microchip = $1"))
                .bind(&chip)
                .fetch_optional(&self.pool)
                .await?;
        row.map(animal).transpose()
    }

    /// Sets how an animal is identified: its chip, where the chip is
    /// registered and to whom, and any band or tag.
    pub async fn set_identity(&self, id: Uuid, input: Identity) -> Result<Animal> {
        let i = input.validated()?;
        let mut tx = self.begin().await?;
        let row: Option<AnimalRow> = sqlx::query_as(concat!(
            "update homestead.animals set microchip = $2, chip_registry = $3, chip_registered_to = $4, band = $5
             where id = $1 returning ",
            animal_cols!()
        ))
        .bind(id)
        .bind(&i.microchip)
        .bind(&i.chip_registry)
        .bind(i.chip_registered_to.as_str())
        .bind(&i.band)
        .fetch_optional(&mut *tx)
        .await
        .map_err(chip_taken)?;
        tx.commit().await?;
        animal(row.ok_or(StoreError::NotFound("animal"))?)
    }

    pub async fn get_animal(&self, id: Uuid) -> Result<Animal> {
        let row: Option<AnimalRow> =
            sqlx::query_as(concat!("select ", animal_cols!(), " from homestead.animals where id = $1"))
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        animal(row.ok_or(StoreError::NotFound("animal"))?)
    }

    /// Everyone, the ones still in our care first.
    pub async fn list_animals(&self) -> Result<Vec<Animal>> {
        let rows: Vec<AnimalRow> = sqlx::query_as(concat!(
            "select ",
            animal_cols!(),
            " from homestead.animals
             order by status not in ('resident', 'in_care', 'adoptable'), species, name"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(animal).collect()
    }

    pub async fn set_animal_status(&self, id: Uuid, status: AnimalStatus) -> Result<()> {
        let mut tx = self.begin().await?;
        let n = sqlx::query("update homestead.animals set status = $2 where id = $1")
            .bind(id)
            .bind(status.as_str())
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        if n == 0 { Err(StoreError::NotFound("animal")) } else { Ok(()) }
    }

    pub async fn log_care(&self, animal_id: Uuid, input: NewCareEntry) -> Result<CareEntry> {
        let c = input.validated()?;
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.animal_care (animal_id, occurred_on, kind, details, provider, cost)
             values ($1, $2, $3, $4, $5, $6) returning id",
        )
        .bind(animal_id)
        .bind(c.occurred_on)
        .bind(c.kind.as_str())
        .bind(&c.details)
        .bind(&c.provider)
        .bind(c.cost)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match StoreError::from(e) {
            StoreError::NotFound(_) => StoreError::NotFound("animal"),
            other => other,
        })?;
        advance_schedules(&mut tx, animal_id, c.kind, c.occurred_on).await?;
        tx.commit().await?;
        Ok(CareEntry {
            id,
            animal_id,
            occurred_on: c.occurred_on,
            kind: c.kind,
            details: c.details,
            provider: c.provider,
            cost: c.cost,
        })
    }

    pub async fn list_care(&self, animal_id: Uuid) -> Result<Vec<CareEntry>> {
        let rows: Vec<(Uuid, Date, String, String, String, Option<Decimal>)> = sqlx::query_as(
            "select id, occurred_on, kind, details, provider, cost from homestead.animal_care
             where animal_id = $1 order by occurred_on desc, created_at desc",
        )
        .bind(animal_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(id, occurred_on, kind, details, provider, cost)| {
                Ok(CareEntry { id, animal_id, occurred_on, kind: parse(&kind)?, details, provider, cost })
            })
            .collect()
    }

    /// Records where an animal went and moves its status to match, in one
    /// transaction. An adoption is announced (the animal's name only).
    pub async fn place_animal(&self, animal_id: Uuid, input: NewPlacement) -> Result<Placement> {
        let p = input.validated()?;
        let mut tx = self.begin().await?;
        let (name, species, microchip, registry): (String, String, String, String) = sqlx::query_as(
            "select name, species, microchip, chip_registry from homestead.animals where id = $1 for update",
        )
        .bind(animal_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::NotFound("animal"))?;
        let species: Species = parse(&species)?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.animal_placements (animal_id, kind, person_name, started_on, fee, notes)
             values ($1, $2, $3, $4, $5, $6) returning id",
        )
        .bind(animal_id)
        .bind(p.kind.as_str())
        .bind(&p.person_name)
        .bind(p.started_on)
        .bind(p.fee)
        .bind(&p.notes)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("update homestead.animals set status = $2 where id = $1")
            .bind(animal_id)
            .bind(p.kind.resulting_status().as_str())
            .execute(&mut *tx)
            .await?;
        if p.kind == PlacementKind::Adoption {
            // The step that's easy to forget: until the registry has the
            // adopter's details, a lost pet's chip leads back to us, or to
            // nobody.
            if !microchip.is_empty() {
                let registry = if registry.is_empty() { "its registry".to_owned() } else { registry };
                crate::insert_task(
                    &mut tx,
                    NewTask {
                        title: format!("Transfer {name}'s microchip to {}", p.person_name),
                        details: format!(
                            "Chip {microchip}. Update {registry} with the adopter's contact details, then set \
                             \"registered to\" to adopter on {name}'s page."
                        ),
                        location_id: None,
                        due_on: Some(p.started_on + time::Duration::days(7)),
                        priority: TaskPriority::High,
                        mirror_to_linear: false,
                        assigned_to: None,
                    },
                )
                .await?;
            }
            outbox::emit(&mut tx, &Event::AnimalAdopted { animal_name: name, species }).await?;
        }
        tx.commit().await?;
        Ok(Placement {
            id,
            animal_id,
            kind: p.kind,
            person_name: p.person_name,
            started_on: p.started_on,
            fee: p.fee,
            notes: p.notes,
        })
    }

    pub async fn list_placements(&self, animal_id: Uuid) -> Result<Vec<Placement>> {
        let rows: Vec<(Uuid, String, String, Date, Option<Decimal>, String)> = sqlx::query_as(
            "select id, kind, person_name, started_on, fee, notes from homestead.animal_placements
             where animal_id = $1 order by started_on desc, created_at desc",
        )
        .bind(animal_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(id, kind, person_name, started_on, fee, notes)| {
                Ok(Placement { id, animal_id, kind: parse(&kind)?, person_name, started_on, fee, notes })
            })
            .collect()
    }

    /// Kiki's Cat Club counts, for cats that arrived or were placed on or
    /// after `since` (placements and procedures are counted by their own
    /// date; "in our care" and "fostered" are as of now).
    pub async fn cat_club_numbers(&self, since: Date) -> Result<CatClubNumbers> {
        let (adoptions, returned_to_colony, spay_neuters, in_our_care, fostered_now): (i64, i64, i64, i64, i64) =
            sqlx::query_as(
                "select
                   (select count(*) from homestead.animal_placements p join homestead.animals a on a.id = p.animal_id
                     where a.species = 'cat' and p.kind = 'adoption' and p.started_on >= $1),
                   (select count(*) from homestead.animal_placements p join homestead.animals a on a.id = p.animal_id
                     where a.species = 'cat' and p.kind = 'return_to_colony' and p.started_on >= $1),
                   (select count(*) from homestead.animal_care c join homestead.animals a on a.id = c.animal_id
                     where a.species = 'cat' and c.kind = 'spay_neuter' and c.occurred_on >= $1),
                   (select count(*) from homestead.animals
                     where species = 'cat' and status in ('resident', 'in_care', 'adoptable')),
                   (select count(*) from homestead.animals where species = 'cat' and status = 'fostered')",
            )
            .bind(since)
            .fetch_one(&self.pool)
            .await?;
        Ok(CatClubNumbers { adoptions, returned_to_colony, spay_neuters, in_our_care, fostered_now })
    }

    // ── TNR colonies ────────────────────────────────────────────────────────

    pub async fn create_colony(&self, input: NewColony) -> Result<Colony> {
        let c = input.validated()?;
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.colonies (name, area, caretaker_name, caretaker_contact, estimated_size, notes)
             values ($1, $2, $3, $4, $5, $6) returning id",
        )
        .bind(&c.name)
        .bind(&c.area)
        .bind(&c.caretaker_name)
        .bind(&c.caretaker_contact)
        .bind(c.estimated_size)
        .bind(&c.notes)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match StoreError::from(e) {
            StoreError::Conflict(_) => StoreError::Conflict("there's already a colony by that name".into()),
            other => other,
        })?;
        tx.commit().await?;
        Ok(Colony {
            id,
            name: c.name,
            area: c.area,
            caretaker_name: c.caretaker_name,
            caretaker_contact: c.caretaker_contact,
            estimated_size: c.estimated_size,
            notes: c.notes,
        })
    }

    /// The caretaker's latest count.
    pub async fn set_colony_estimate(&self, id: Uuid, estimated_size: Option<i32>) -> Result<()> {
        if estimated_size.is_some_and(|n| n < 0) {
            return Err(homestead_domain::ValidationError::Negative("estimated size").into());
        }
        let mut tx = self.begin().await?;
        let n = sqlx::query("update homestead.colonies set estimated_size = $2 where id = $1")
            .bind(id)
            .bind(estimated_size)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        if n == 0 { Err(StoreError::NotFound("colony")) } else { Ok(()) }
    }

    /// Every colony with its cats recorded and how many of them are fixed
    /// (ear-tipped, or a spay/neuter on record). Most left to fix first.
    pub async fn colony_progress(&self) -> Result<Vec<ColonyProgress>> {
        type Row = (Uuid, String, String, String, String, Option<i32>, String, i64, i64);
        let rows: Vec<Row> = sqlx::query_as(
            "select c.id, c.name, c.area, c.caretaker_name, c.caretaker_contact, c.estimated_size, c.notes,
                    count(a.id),
                    count(a.id) filter (where a.ear_tipped or exists (
                      select 1 from homestead.animal_care k where k.animal_id = a.id and k.kind = 'spay_neuter'))
             from homestead.colonies c left join homestead.animals a on a.colony_id = c.id
             group by c.id order by c.name",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out: Vec<ColonyProgress> = rows
            .into_iter()
            .map(|(id, name, area, caretaker_name, caretaker_contact, estimated_size, notes, recorded, fixed)| {
                ColonyProgress {
                    colony: Colony { id, name, area, caretaker_name, caretaker_contact, estimated_size, notes },
                    recorded,
                    fixed,
                }
            })
            .collect();
        // Stable: colonies with the same number left keep name order.
        out.sort_by_key(|p| std::cmp::Reverse(p.left_to_fix()));
        Ok(out)
    }

    /// Which colony a cat came from, and whether it's ear-tipped.
    pub async fn set_tnr(&self, id: Uuid, colony_id: Option<Uuid>, ear_tipped: bool) -> Result<()> {
        let mut tx = self.begin().await?;
        let n = sqlx::query("update homestead.animals set colony_id = $2, ear_tipped = $3 where id = $1")
            .bind(id)
            .bind(colony_id)
            .bind(ear_tipped)
            .execute(&mut *tx)
            .await
            .map_err(|e| match StoreError::from(e) {
                StoreError::NotFound(_) => StoreError::NotFound("colony"),
                StoreError::Db(sqlx::Error::Database(d)) if d.code().as_deref() == Some("23514") => {
                    StoreError::Conflict("only cats belong to a colony".into())
                }
                other => other,
            })?
            .rows_affected();
        tx.commit().await?;
        if n == 0 { Err(StoreError::NotFound("animal")) } else { Ok(()) }
    }

    // ── Care schedules ──────────────────────────────────────────────────────

    pub async fn add_care_schedule(&self, animal_id: Uuid, input: NewCareSchedule) -> Result<CareSchedule> {
        let s = input.validated()?;
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.care_schedules (animal_id, kind, details, every_days, next_due_on, ends_on)
             values ($1, $2, $3, $4, $5, $6) returning id",
        )
        .bind(animal_id)
        .bind(s.kind.as_str())
        .bind(&s.details)
        .bind(s.every_days)
        .bind(s.first_due_on)
        .bind(s.ends_on)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match StoreError::from(e) {
            StoreError::NotFound(_) => StoreError::NotFound("animal"),
            other => other,
        })?;
        tx.commit().await?;
        self.care_schedules(Some(animal_id), None)
            .await?
            .into_iter()
            .find(|x| x.id == id)
            .ok_or(StoreError::NotFound("schedule"))
    }

    pub async fn stop_care_schedule(&self, id: Uuid) -> Result<()> {
        let mut tx = self.begin().await?;
        let n =
            sqlx::query("update homestead.care_schedules set stopped_at = now() where id = $1 and stopped_at is null")
                .bind(id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        tx.commit().await?;
        if n == 0 { Err(StoreError::NotFound("schedule")) } else { Ok(()) }
    }

    /// Running schedules, soonest due first: one animal's, or everyone's
    /// due on or before `due_by`.
    pub async fn care_schedules(&self, animal_id: Option<Uuid>, due_by: Option<Date>) -> Result<Vec<CareSchedule>> {
        type Row = (Uuid, Uuid, String, String, String, String, i32, Date, Option<Date>);
        let rows: Vec<Row> = sqlx::query_as(
            "select s.id, s.animal_id, a.name, a.status, s.kind, s.details, s.every_days, s.next_due_on, s.ends_on
             from homestead.care_schedules s join homestead.animals a on a.id = s.animal_id
             where s.stopped_at is null
               and ($1::uuid is null or s.animal_id = $1)
               and ($2::date is null or s.next_due_on <= $2)
               and a.status <> 'deceased'
             order by s.next_due_on, a.name",
        )
        .bind(animal_id)
        .bind(due_by)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(id, animal_id, animal_name, status, kind, details, every_days, next_due_on, ends_on)| {
                Ok(CareSchedule {
                    id,
                    animal_id,
                    animal_name,
                    animal_status: parse(&status)?,
                    kind: parse(&kind)?,
                    details,
                    every_days,
                    next_due_on,
                    ends_on,
                })
            })
            .collect()
    }
}

/// Care of a scheduled kind moves that schedule on from the day it was
/// given, or ends it after the last dose. Care dated before the current
/// interval (a record typed in late) leaves the schedule alone.
async fn advance_schedules(tx: &mut crate::Tx<'_>, animal_id: Uuid, kind: CareKind, given_on: Date) -> Result<()> {
    let due: Vec<(Uuid, i32, Date, Option<Date>)> = sqlx::query_as(
        "select id, every_days, next_due_on, ends_on from homestead.care_schedules
         where animal_id = $1 and kind = $2 and stopped_at is null for update",
    )
    .bind(animal_id)
    .bind(kind.as_str())
    .fetch_all(&mut **tx)
    .await?;
    for (id, every_days, next_due_on, ends_on) in due {
        if given_on < next_due_on - time::Duration::days(i64::from(every_days)) {
            continue;
        }
        match next_due(given_on, every_days, ends_on) {
            Some(next) => {
                sqlx::query("update homestead.care_schedules set next_due_on = $2 where id = $1")
                    .bind(id)
                    .bind(next)
                    .execute(&mut **tx)
                    .await?;
            }
            None => {
                sqlx::query("update homestead.care_schedules set stopped_at = now() where id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
            }
        }
    }
    Ok(())
}

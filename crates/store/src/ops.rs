//! Eggs, compost,
//! checklists, losses, obligations, emergency plans, and cost links.

use homestead_domain::ValidationError;
use rust_decimal::Decimal;
use time::Date;
use uuid::Uuid;

use crate::{Result, Store, StoreError};

/// More than any backyard flock lays in one go: a typo guard for a form
/// anyone holding the coop's tag can fill in.
pub const MAX_EGGS_AT_ONCE: i32 = 200;

fn non_blank(field: &'static str, s: &str) -> Result<String> {
    let t = s.trim();
    if t.is_empty() { Err(ValidationError::Blank(field).into()) } else { Ok(t.to_owned()) }
}

impl Store {
    pub async fn record_eggs(
        &self,
        animal_id: Uuid,
        on_date: Date,
        collected: i32,
        damaged: i32,
        donated: i32,
        internal_use: i32,
    ) -> Result<Uuid> {
        if collected < 0 || damaged < 0 || donated < 0 || internal_use < 0 {
            return Err(ValidationError::Negative("eggs").into());
        }
        let mut tx = self.begin().await?;
        let row = sqlx::query_scalar(
            "insert into homestead.egg_days (animal_id, on_date, collected, damaged, donated, internal_use)
             values ($1,$2,$3,$4,$5,$6) returning id",
        )
        .bind(animal_id)
        .bind(on_date)
        .bind(collected)
        .bind(damaged)
        .bind(donated)
        .bind(internal_use)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    /// Adds a collection to the day's count, for a flock whose eggs are
    /// gathered more than once a day (a QR tag on the coop): two people
    /// logging 6 and 5 make 11, where [`Store::record_eggs`] would refuse
    /// the second. Returns the day's totals (collected, damaged).
    pub async fn add_eggs(&self, animal_id: Uuid, on_date: Date, collected: i32, damaged: i32) -> Result<(i32, i32)> {
        if collected < 1 {
            return Err(ValidationError::NotPositive("eggs collected").into());
        }
        if damaged < 0 || damaged > collected {
            return Err(ValidationError::Malformed("cracked eggs (between none and all of them)").into());
        }
        if collected > MAX_EGGS_AT_ONCE {
            return Err(ValidationError::Malformed("eggs collected (more than a flock lays at once)").into());
        }
        let mut tx = self.begin().await?;
        let species: String = sqlx::query_scalar("select species from homestead.animals where id = $1")
            .bind(animal_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StoreError::NotFound("animal"))?;
        if !matches!(species.as_str(), "chicken" | "duck") {
            return Err(StoreError::Conflict("only the birds' tags take eggs".into()));
        }
        let totals: (i32, i32) = sqlx::query_as(
            "insert into homestead.egg_days (animal_id, on_date, collected, damaged) values ($1, $2, $3, $4)
             on conflict (animal_id, on_date) do update
               set collected = egg_days.collected + excluded.collected,
                   damaged = egg_days.damaged + excluded.damaged
             returning collected, damaged",
        )
        .bind(animal_id)
        .bind(on_date)
        .bind(collected)
        .bind(damaged)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(totals)
    }

    /// The eggs logged for a flock on a day: (collected, damaged).
    pub async fn eggs_on(&self, animal_id: Uuid, on_date: Date) -> Result<(i32, i32)> {
        Ok(sqlx::query_as(
            "select coalesce(sum(collected), 0)::int, coalesce(sum(damaged), 0)::int from homestead.egg_days
             where animal_id = $1 and on_date = $2",
        )
        .bind(animal_id)
        .bind(on_date)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn add_compost_batch(
        &self,
        name: &str,
        started_on: Date,
        inputs: &str,
        location_id: Option<Uuid>,
    ) -> Result<Uuid> {
        let mut tx = self.begin().await?;
        let row = sqlx::query_scalar(
            "insert into homestead.compost_batches (name, started_on, inputs, location_id) values ($1,$2,$3,$4) returning id",
        )
        .bind(non_blank("name", name)?)
        .bind(started_on)
        .bind(inputs.trim())
        .bind(location_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    pub async fn turn_compost(
        &self,
        batch_id: Uuid,
        on_date: Date,
        temperature_c: Option<Decimal>,
        moisture: &str,
    ) -> Result<Uuid> {
        let mut tx = self.begin().await?;
        let row = sqlx::query_scalar(
            "insert into homestead.compost_turns (batch_id, on_date, temperature_c, moisture) values ($1,$2,$3,$4) returning id",
        )
        .bind(batch_id)
        .bind(on_date)
        .bind(temperature_c)
        .bind(moisture.trim())
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    pub async fn create_checklist(&self, name: &str, items: &[String]) -> Result<Uuid> {
        let name = non_blank("name", name)?;
        if items.is_empty() {
            return Err(ValidationError::Blank("checklist item").into());
        }
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar("insert into homestead.checklist_templates (name) values ($1) returning id")
            .bind(&name)
            .fetch_one(&mut *tx)
            .await?;
        for (i, label) in items.iter().enumerate() {
            sqlx::query(
                "insert into homestead.checklist_template_items (template_id, position, label) values ($1,$2,$3)",
            )
            .bind(id)
            .bind(i as i32)
            .bind(non_blank("item", label)?)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(id)
    }

    pub async fn start_checklist(&self, template_id: Uuid, on_date: Date, completed_by: &str) -> Result<Uuid> {
        let mut tx = self.begin().await?;
        let run: Uuid = sqlx::query_scalar(
            "insert into homestead.checklist_runs (template_id, on_date, completed_by) values ($1,$2,$3) returning id",
        )
        .bind(template_id)
        .bind(on_date)
        .bind(completed_by.trim())
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "insert into homestead.checklist_run_items (run_id, label, position)
             select $1, label, position from homestead.checklist_template_items where template_id = $2 order by position",
        )
        .bind(run)
        .bind(template_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(run)
    }

    pub async fn list_checklist_templates(&self) -> Result<Vec<(Uuid, String)>> {
        Ok(sqlx::query_as("select id, name from homestead.checklist_templates order by name")
            .fetch_all(&self.pool)
            .await?)
    }

    /// Items on checklist runs for this date, in template order.
    pub async fn checklist_items(&self, on_date: Date) -> Result<Vec<(Uuid, String, String, bool)>> {
        Ok(sqlx::query_as(
            "select i.id, t.name, i.label, i.done
             from homestead.checklist_run_items i
             join homestead.checklist_runs r on r.id = i.run_id
             join homestead.checklist_templates t on t.id = r.template_id
             where r.on_date = $1
             order by t.name, i.position",
        )
        .bind(on_date)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn check_item(&self, item_id: Uuid, done: bool, note: &str) -> Result<()> {
        let mut tx = self.begin().await?;
        let n = sqlx::query("update homestead.checklist_run_items set done = $2, note = $3 where id = $1")
            .bind(item_id)
            .bind(done)
            .bind(note.trim())
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        if n == 0 { Err(StoreError::NotFound("checklist item")) } else { Ok(()) }
    }

    pub async fn record_loss(
        &self,
        lost_on: Date,
        reason: &str,
        quantity: Option<Decimal>,
        unit: &str,
        planting_id: Option<Uuid>,
        harvest_id: Option<Uuid>,
    ) -> Result<Uuid> {
        let reason = non_blank("reason", reason)?;
        if quantity.is_some() != !unit.trim().is_empty() {
            return Err(ValidationError::Malformed("quantity (give both an amount and its unit, or neither)").into());
        }
        let mut tx = self.begin().await?;
        let row = sqlx::query_scalar(
            "insert into homestead.losses (lost_on, reason, quantity, unit, planting_id, harvest_id)
             values ($1,$2,$3,$4,$5,$6) returning id",
        )
        .bind(lost_on)
        .bind(&reason)
        .bind(quantity)
        .bind(unit.trim())
        .bind(planting_id)
        .bind(harvest_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    pub async fn add_obligation(
        &self,
        requirement: &str,
        authority: &str,
        due_on: Option<Date>,
        reference: &str,
    ) -> Result<Uuid> {
        let mut tx = self.begin().await?;
        let row = sqlx::query_scalar(
            "insert into homestead.obligations (requirement, authority, due_on, external_reference) values ($1,$2,$3,$4) returning id",
        )
        .bind(non_blank("requirement", requirement)?)
        .bind(authority.trim())
        .bind(due_on)
        .bind(reference.trim())
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    pub async fn complete_obligation(&self, id: Uuid, on: Date) -> Result<()> {
        let mut tx = self.begin().await?;
        let n = sqlx::query(
            "update homestead.obligations set status = 'done', completed_on = $2 where id = $1 and status = 'open'",
        )
        .bind(id)
        .bind(on)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        if n == 0 { Err(StoreError::NotFound("open obligation")) } else { Ok(()) }
    }

    pub async fn save_emergency_plan(
        &self,
        scenario: &str,
        procedure: &str,
        contacts: &str,
        animals: &str,
    ) -> Result<Uuid> {
        let mut tx = self.begin().await?;
        let row = sqlx::query_scalar(
            "insert into homestead.emergency_plans (scenario, procedure, contacts, animals)
             values ($1,$2,$3,$4)
             on conflict (scenario) do update set procedure = excluded.procedure, contacts = excluded.contacts, animals = excluded.animals
             returning id",
        )
        .bind(non_blank("scenario", scenario)?)
        .bind(procedure.trim())
        .bind(contacts.trim())
        .bind(animals.trim())
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    pub async fn link_cost(
        &self,
        purchase_id: Uuid,
        program: &str,
        location_id: Option<Uuid>,
        animal_id: Option<Uuid>,
        equipment_id: Option<Uuid>,
    ) -> Result<()> {
        let mut tx = self.begin().await?;
        sqlx::query(
            "insert into homestead.cost_links (purchase_id, program, location_id, animal_id, equipment_id)
             values ($1,$2,$3,$4,$5)
             on conflict (purchase_id) do update set program = excluded.program, location_id = excluded.location_id,
               animal_id = excluded.animal_id, equipment_id = excluded.equipment_id",
        )
        .bind(purchase_id)
        .bind(program.trim())
        .bind(location_id)
        .bind(animal_id)
        .bind(equipment_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn open_obligations(&self, through: Date) -> Result<Vec<OpenObligation>> {
        Ok(sqlx::query_as(
            "select id, requirement, authority, due_on from homestead.obligations
             where status = 'open' and (due_on is null or due_on <= $1)
             order by due_on nulls last, requirement",
        )
        .bind(through)
        .fetch_all(&self.pool)
        .await?)
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OpenObligation {
    pub id: Uuid,
    pub requirement: String,
    pub authority: String,
    pub due_on: Option<Date>,
}

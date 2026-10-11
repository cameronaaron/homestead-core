//! QR tags, and the tool loans and problem reports people make at them.

use homestead_domain::equipment::EquipmentStatus;
use homestead_domain::tags::{Loan, NewLoan, Tag, TagKind};
use homestead_domain::{NewTask, Task, TaskPriority};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::{Result, Store, StoreError, insert_task, parse};

/// Columns matching [`TagRow`].
macro_rules! tag_cols {
    () => {
        "code, kind, coalesce(equipment_id, inventory_item_id, location_id, animal_id), label, created_at"
    };
}

type TagRow = (String, String, Option<Uuid>, String, OffsetDateTime);

fn tag(r: TagRow) -> Result<Tag> {
    let (code, kind, target_id, label, created_at) = r;
    Ok(Tag { code, kind: parse(&kind)?, target_id, label, created_at })
}

/// A live tag and the name of what it's stuck to, for the print sheet.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TagListing {
    pub tag: Tag,
    pub target_name: String,
}

macro_rules! loan_cols {
    () => {
        "l.id, l.equipment_id, e.name, l.borrower, l.out_at, l.due_on, l.returned_at, l.return_note
         from homestead.equipment_loans l join homestead.equipment e on e.id = l.equipment_id"
    };
}

type LoanRow = (Uuid, Uuid, String, String, OffsetDateTime, Option<Date>, Option<OffsetDateTime>, String);

fn loan(r: LoanRow) -> Loan {
    let (id, equipment_id, equipment_name, borrower, out_at, due_on, returned_at, return_note) = r;
    Loan { id, equipment_id, equipment_name, borrower, out_at, due_on, returned_at, return_note }
}

impl Store {
    /// The live tag for `target` (or the gate), making one with `code` if
    /// there's none. Asking twice gives the same code, so a reprint never
    /// orphans the label already on the thing.
    pub async fn ensure_tag(&self, kind: TagKind, target: Option<Uuid>, code: &str) -> Result<Tag> {
        if homestead_domain::tags::normalise_code(code).as_deref() != Some(code) {
            return Err(StoreError::Invalid(homestead_domain::ValidationError::Malformed("tag code")));
        }
        if (kind == TagKind::Gate) != target.is_none() {
            return Err(StoreError::Invalid(homestead_domain::ValidationError::Malformed("tag target")));
        }
        let mut tx = self.begin().await?;
        let (equipment, item, location, animal) = match kind {
            TagKind::Equipment => (target, None, None, None),
            TagKind::InventoryItem => (None, target, None, None),
            TagKind::Location => (None, None, target, None),
            TagKind::Animal => (None, None, None, target),
            TagKind::Gate => (None, None, None, None),
        };
        let label: Option<String> = sqlx::query_scalar(
            "select coalesce(
               (select name from homestead.equipment where id = $1),
               (select name from homestead.inventory_items where id = $2),
               (select name from homestead.locations where id = $3),
               (select name from homestead.animals where id = $4),
               case when $5 then 'Sign in at the gate' end)",
        )
        .bind(equipment)
        .bind(item)
        .bind(location)
        .bind(animal)
        .bind(kind == TagKind::Gate)
        .fetch_one(&mut *tx)
        .await?;
        let label = label.ok_or(StoreError::NotFound("thing to tag"))?;
        sqlx::query(
            "insert into homestead.tags (code, kind, equipment_id, inventory_item_id, location_id, animal_id, label)
             values ($1, $2, $3, $4, $5, $6, $7) on conflict do nothing",
        )
        .bind(code)
        .bind(kind.as_str())
        .bind(equipment)
        .bind(item)
        .bind(location)
        .bind(animal)
        .bind(&label)
        .execute(&mut *tx)
        .await?;
        let row: TagRow = sqlx::query_as(concat!(
            "select ",
            tag_cols!(),
            " from homestead.tags where retired_at is null and kind = $1
               and coalesce(equipment_id, inventory_item_id, location_id, animal_id) is not distinct from $2"
        ))
        .bind(kind.as_str())
        .bind(target)
        .fetch_optional(&mut *tx)
        .await?
        // Only when the fresh code collided with another thing's (about one
        // chance in 10^14): the caller makes a new code and asks again.
        .ok_or_else(|| StoreError::Conflict("that tag code is taken; try again".into()))?;
        tx.commit().await?;
        tag(row)
    }

    /// A live tag by its code.
    pub async fn tag(&self, code: &str) -> Result<Tag> {
        let row: Option<TagRow> = sqlx::query_as(concat!(
            "select ",
            tag_cols!(),
            " from homestead.tags where code = $1 and retired_at is null"
        ))
        .bind(code)
        .fetch_optional(&self.pool)
        .await?;
        tag(row.ok_or(StoreError::NotFound("tag"))?)
    }

    /// Every live tag, with what it's on.
    pub async fn list_tags(&self) -> Result<Vec<TagListing>> {
        let rows: Vec<(String, String, Option<Uuid>, String, OffsetDateTime, String)> = sqlx::query_as(
            "select t.code, t.kind, coalesce(t.equipment_id, t.inventory_item_id, t.location_id, t.animal_id),
                    t.label, t.created_at,
                    coalesce(e.name, i.name, l.name, a.name, 'The gate')
             from homestead.tags t
             left join homestead.equipment e on e.id = t.equipment_id
             left join homestead.inventory_items i on i.id = t.inventory_item_id
             left join homestead.locations l on l.id = t.location_id
             left join homestead.animals a on a.id = t.animal_id
             where t.retired_at is null order by t.kind, 6",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(code, kind, target_id, label, created_at, target_name)| {
                Ok(TagListing { tag: tag((code, kind, target_id, label, created_at))?, target_name })
            })
            .collect()
    }

    /// Stops a code working. Print a new one for the same thing.
    pub async fn retire_tag(&self, code: &str) -> Result<()> {
        let mut tx = self.begin().await?;
        let n = sqlx::query("update homestead.tags set retired_at = now() where code = $1 and retired_at is null")
            .bind(code)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        if n == 0 { Err(StoreError::NotFound("tag")) } else { Ok(()) }
    }

    // ── Loans ───────────────────────────────────────────────────────────────

    /// Checks a tool out. Refused while someone else has it, or while it's
    /// out of service or retired.
    pub async fn check_out(&self, equipment_id: Uuid, input: NewLoan) -> Result<Loan> {
        let today = self.today().await?;
        let l = input.validated(today)?;
        let mut tx = self.begin().await?;
        let status: String = sqlx::query_scalar("select status from homestead.equipment where id = $1 for update")
            .bind(equipment_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StoreError::NotFound("equipment"))?;
        let status: EquipmentStatus = parse(&status)?;
        if matches!(status, EquipmentStatus::OutOfService | EquipmentStatus::Retired) {
            return Err(StoreError::Conflict(format!("it's {status}, so it can't be checked out")));
        }
        let open: Option<String> = sqlx::query_scalar(
            "select borrower from homestead.equipment_loans where equipment_id = $1 and returned_at is null",
        )
        .bind(equipment_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(who) = open {
            return Err(StoreError::Conflict(format!("{who} has it; return it first")));
        }
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.equipment_loans (equipment_id, borrower, actor_id, due_on)
             values ($1, $2, nullif(current_setting('homestead.actor_id', true), '')::uuid, $3) returning id",
        )
        .bind(equipment_id)
        .bind(&l.borrower)
        .bind(l.due_on)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        self.loan(id).await
    }

    /// Brings a tool back, whoever returns it.
    pub async fn return_equipment(&self, equipment_id: Uuid, note: &str) -> Result<Loan> {
        let note = homestead_domain::tags::note("return note (500 characters at most)", note)?;
        let mut tx = self.begin().await?;
        let id: Option<Uuid> = sqlx::query_scalar(
            "update homestead.equipment_loans set returned_at = now(), return_note = $2
             where equipment_id = $1 and returned_at is null returning id",
        )
        .bind(equipment_id)
        .bind(&note)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        self.loan(id.ok_or_else(|| StoreError::Conflict("it isn't checked out".into()))?).await
    }

    async fn loan(&self, id: Uuid) -> Result<Loan> {
        let row: LoanRow = sqlx::query_as(concat!("select ", loan_cols!(), " where l.id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound("loan"))?;
        Ok(loan(row))
    }

    /// Who has this tool now, if anyone.
    pub async fn open_loan(&self, equipment_id: Uuid) -> Result<Option<Loan>> {
        let row: Option<LoanRow> =
            sqlx::query_as(concat!("select ", loan_cols!(), " where l.equipment_id = $1 and l.returned_at is null"))
                .bind(equipment_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(loan))
    }

    /// Everything out right now, longest out first.
    pub async fn open_loans(&self) -> Result<Vec<Loan>> {
        let rows: Vec<LoanRow> =
            sqlx::query_as(concat!("select ", loan_cols!(), " where l.returned_at is null order by l.out_at"))
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(loan).collect())
    }

    /// A tool's loans, newest first.
    pub async fn loan_history(&self, equipment_id: Uuid, limit: i64) -> Result<Vec<Loan>> {
        let rows: Vec<LoanRow> = sqlx::query_as(concat!(
            "select ",
            loan_cols!(),
            " where l.equipment_id = $1 order by l.out_at desc limit $2"
        ))
        .bind(equipment_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(loan).collect())
    }

    /// Someone says a tool is broken: it's marked as needing repair (unless
    /// it's already out of service) and a fix-it task lands in Tasks, in
    /// one transaction.
    pub async fn report_equipment_problem(&self, equipment_id: Uuid, reporter: &str, problem: &str) -> Result<Task> {
        let problem = homestead_domain::tags::note("problem (500 characters at most)", problem)?;
        if problem.is_empty() {
            return Err(homestead_domain::ValidationError::Blank("what's wrong").into());
        }
        let mut tx = self.begin().await?;
        let (name, location_id): (String, Option<Uuid>) =
            sqlx::query_as("select name, location_id from homestead.equipment where id = $1 for update")
                .bind(equipment_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::NotFound("equipment"))?;
        sqlx::query("update homestead.equipment set status = 'needs_repair' where id = $1 and status = 'in_service'")
            .bind(equipment_id)
            .execute(&mut *tx)
            .await?;
        let task = insert_task(
            &mut tx,
            NewTask {
                title: format!("Fix {name}"),
                details: format!("{reporter} reported: {problem}"),
                location_id,
                due_on: None,
                priority: TaskPriority::High,
                mirror_to_linear: false,
                assigned_to: None,
            },
        )
        .await?;
        tx.commit().await?;
        Ok(task)
    }

    /// Someone at a tag says something's wrong with a place or an animal:
    /// a task to look into it, at that place (or where the animal is kept
    /// isn't recorded, so no place).
    pub async fn report_concern(&self, kind: TagKind, target: Uuid, reporter: &str, concern: &str) -> Result<Task> {
        let concern = homestead_domain::tags::note("what you noticed (500 characters at most)", concern)?;
        if concern.is_empty() {
            return Err(homestead_domain::ValidationError::Blank("what you noticed").into());
        }
        let mut tx = self.begin().await?;
        let (title, location_id) = match kind {
            TagKind::Location => {
                let name: String = sqlx::query_scalar("select name from homestead.locations where id = $1")
                    .bind(target)
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or(StoreError::NotFound("place"))?;
                (format!("Look at {name}"), Some(target))
            }
            TagKind::Animal => {
                let name: String = sqlx::query_scalar("select name from homestead.animals where id = $1")
                    .bind(target)
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or(StoreError::NotFound("animal"))?;
                (format!("Check on {name}"), None)
            }
            _ => {
                return Err(
                    homestead_domain::ValidationError::Malformed("tag kind (tools use a problem report)").into()
                );
            }
        };
        let task = insert_task(
            &mut tx,
            NewTask {
                title,
                details: format!("{reporter} noticed: {concern}"),
                location_id,
                due_on: None,
                priority: TaskPriority::High,
                mirror_to_linear: false,
                assigned_to: None,
            },
        )
        .await?;
        tx.commit().await?;
        Ok(task)
    }
}

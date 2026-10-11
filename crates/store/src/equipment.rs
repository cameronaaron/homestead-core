//! Equipment and its maintenance.

use homestead_domain::equipment::{Equipment, EquipmentState, Maintenance, NewEquipment, NewMaintenance};
use homestead_domain::{Event, Task, TaskPriority, TaskStatus};
use rust_decimal::Decimal;
use time::Date;
use uuid::Uuid;

use crate::{Result, Store, StoreError, outbox, parse};

macro_rules! equipment_cols {
    () => {
        "e.id, e.name, e.category, e.manufacturer, e.model, e.serial_number, e.purchased_on, e.purchase_cost,
         e.purchase_id, e.location_id, l.name, e.condition, e.status, e.notes, m.performed_on, m.next_service_on
         from homestead.equipment e
         left join homestead.locations l on l.id = e.location_id
         left join lateral (
           select performed_on, next_service_on from homestead.equipment_maintenance
           where equipment_id = e.id order by performed_on desc, created_at desc limit 1
         ) m on true"
    };
}

type Row = (
    Uuid,
    String,
    String,
    String,
    String,
    String,
    Option<Date>,
    Option<Decimal>,
    Option<Uuid>,
    Option<Uuid>,
    Option<String>,
    String,
    String,
    String,
    Option<Date>,
    Option<Date>,
);

fn equipment(r: Row) -> Result<Equipment> {
    let (
        id,
        name,
        category,
        manufacturer,
        model,
        serial_number,
        purchased_on,
        purchase_cost,
        purchase_id,
        location_id,
        location_name,
        condition,
        status,
        notes,
        last_serviced_on,
        next_service_on,
    ) = r;
    Ok(Equipment {
        id,
        name,
        category,
        manufacturer,
        model,
        serial_number,
        purchased_on,
        purchase_cost,
        purchase_id,
        location_id,
        location_name,
        condition: parse(&condition)?,
        status: parse(&status)?,
        notes,
        last_serviced_on,
        next_service_on,
    })
}

type MaintenanceRow = (
    Uuid,
    Uuid,
    Date,
    String,
    Option<Decimal>,
    String,
    Option<Uuid>,
    Option<String>,
    Option<Date>,
    String,
    Option<Uuid>,
);

impl Store {
    pub async fn create_equipment(&self, input: NewEquipment) -> Result<Equipment> {
        let e = input.validated()?;
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.equipment
               (name, category, manufacturer, model, serial_number, purchased_on, purchase_cost, purchase_id,
                location_id, condition, notes)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) returning id",
        )
        .bind(&e.name)
        .bind(&e.category)
        .bind(&e.manufacturer)
        .bind(&e.model)
        .bind(&e.serial_number)
        .bind(e.purchased_on)
        .bind(e.purchase_cost)
        .bind(e.purchase_id)
        .bind(e.location_id)
        .bind(e.condition.as_str())
        .bind(&e.notes)
        .fetch_one(&mut *tx)
        .await
        .map_err(|err| match StoreError::from(err) {
            StoreError::NotFound(_) => StoreError::NotFound("purchase or location"),
            StoreError::Conflict(_) => StoreError::Conflict("that serial number is already recorded".into()),
            other => other,
        })?;
        tx.commit().await?;
        self.get_equipment(id).await
    }

    pub async fn get_equipment(&self, id: Uuid) -> Result<Equipment> {
        let row: Option<Row> = sqlx::query_as(concat!("select ", equipment_cols!(), " where e.id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        equipment(row.ok_or(StoreError::NotFound("equipment"))?)
    }

    /// Everything, in use first, soonest service due first.
    pub async fn list_equipment(&self) -> Result<Vec<Equipment>> {
        let rows: Vec<Row> = sqlx::query_as(concat!(
            "select ",
            equipment_cols!(),
            " order by e.status = 'retired', m.next_service_on nulls last, e.category, e.name"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(equipment).collect()
    }

    pub async fn set_equipment_state(&self, id: Uuid, state: EquipmentState) -> Result<Equipment> {
        let mut tx = self.begin().await?;
        let n =
            sqlx::query("update homestead.equipment set status = $2, condition = $3, location_id = $4 where id = $1")
                .bind(id)
                .bind(state.status.as_str())
                .bind(state.condition.as_str())
                .bind(state.location_id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        tx.commit().await?;
        if n == 0 {
            return Err(StoreError::NotFound("equipment"));
        }
        self.get_equipment(id).await
    }

    /// Records a service, and (if asked) a task due on the next one, in the
    /// same transaction.
    pub async fn add_maintenance(&self, equipment_id: Uuid, input: NewMaintenance) -> Result<Maintenance> {
        let m = input.validated()?;
        let mut tx = self.begin().await?;
        let (name, location_id): (String, Option<Uuid>) =
            sqlx::query_as("select name, location_id from homestead.equipment where id = $1 for update")
                .bind(equipment_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::NotFound("equipment"))?;
        let task_id = match (m.remind, m.next_service_on) {
            (true, Some(due)) => {
                let title = format!("Service {name}");
                let details = format!("Last time ({}): {}", m.performed_on, m.work);
                let id: Uuid = sqlx::query_scalar(
                    "insert into homestead.tasks (title, details, location_id, due_on) values ($1, $2, $3, $4) returning id",
                )
                .bind(&title)
                .bind(&details)
                .bind(location_id)
                .bind(due)
                .fetch_one(&mut *tx)
                .await?;
                let task = Task {
                    id,
                    title,
                    details,
                    location_id,
                    due_on: Some(due),
                    priority: TaskPriority::Normal,
                    status: TaskStatus::Open,
                    mirror_to_linear: false,
                    linear_issue_id: None,
                    completed_at: None,
                    template_id: None,
                    assigned_to: None,
                };
                outbox::emit(&mut tx, &Event::TaskCreated { task }).await?;
                Some(id)
            }
            _ => None,
        };
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.equipment_maintenance
               (equipment_id, performed_on, work, cost, performed_by, vendor_id, next_service_on, notes, task_id)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9) returning id",
        )
        .bind(equipment_id)
        .bind(m.performed_on)
        .bind(&m.work)
        .bind(m.cost)
        .bind(&m.performed_by)
        .bind(m.vendor_id)
        .bind(m.next_service_on)
        .bind(&m.notes)
        .bind(task_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match StoreError::from(e) {
            StoreError::NotFound(_) => StoreError::NotFound("vendor"),
            other => other,
        })?;
        tx.commit().await?;
        self.maintenance_history(equipment_id)
            .await?
            .into_iter()
            .find(|x| x.id == id)
            .ok_or(StoreError::NotFound("maintenance"))
    }

    pub async fn maintenance_history(&self, equipment_id: Uuid) -> Result<Vec<Maintenance>> {
        let rows: Vec<MaintenanceRow> = sqlx::query_as(
            "select m.id, m.equipment_id, m.performed_on, m.work, m.cost, m.performed_by, m.vendor_id, v.name,
                    m.next_service_on, m.notes, m.task_id
             from homestead.equipment_maintenance m left join homestead.vendors v on v.id = m.vendor_id
             where m.equipment_id = $1 order by m.performed_on desc, m.created_at desc",
        )
        .bind(equipment_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    id,
                    equipment_id,
                    performed_on,
                    work,
                    cost,
                    performed_by,
                    vendor_id,
                    vendor_name,
                    next_service_on,
                    notes,
                    task_id,
                )| {
                    Maintenance {
                        id,
                        equipment_id,
                        performed_on,
                        work,
                        cost,
                        performed_by,
                        vendor_id,
                        vendor_name,
                        next_service_on,
                        notes,
                        task_id,
                    }
                },
            )
            .collect())
    }
}

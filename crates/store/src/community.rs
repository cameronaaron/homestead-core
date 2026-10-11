//! Community events, volunteers and their hours.

use homestead_domain::community::{
    Availability, CommunityEvent, EventEdit, EventEquipment, EventInventory, EventPlan, EventRsvp, EventStatus,
    HoursByGroup, NewCommunityEvent, NewVolunteerShift, Visit, Volunteer, VolunteerEdit, VolunteerShift, WorkingGroup,
    gate_hours,
};
use homestead_domain::{Task, TaskPriority, TaskStatus};
use rust_decimal::Decimal;
use time::Date;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Result, Store, StoreError, parse};

type ShiftRow = (Uuid, Uuid, String, Date, Decimal, String, Option<Uuid>, String);
type EventRow = (Uuid, String, String, Date, String, String, String, Option<i32>, String);

fn event(r: EventRow) -> Result<CommunityEvent> {
    let (id, title, kind, held_on, time_note, place, description, attendance, status) = r;
    Ok(CommunityEvent {
        id,
        title,
        kind: parse(&kind)?,
        held_on,
        time_note,
        place,
        description,
        attendance,
        status: parse(&status)?,
    })
}

impl Store {
    pub async fn create_community_event(&self, input: NewCommunityEvent) -> Result<CommunityEvent> {
        let e = input.validated()?;
        let mut tx = self.begin().await?;
        let row: EventRow = sqlx::query_as(
            "insert into homestead.community_events (title, kind, held_on, time_note, place, description)
             values ($1, $2, $3, $4, $5, $6)
             returning id, title, kind, held_on, time_note, place, description, attendance, status",
        )
        .bind(&e.title)
        .bind(e.kind.as_str())
        .bind(e.held_on)
        .bind(&e.time_note)
        .bind(&e.place)
        .bind(&e.description)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        event(row)
    }

    /// Upcoming first (soonest first), then past (most recent first).
    pub async fn list_community_events(&self, today: Date) -> Result<Vec<CommunityEvent>> {
        let rows: Vec<EventRow> = sqlx::query_as(
            "select id, title, kind, held_on, time_note, place, description, attendance, status
             from homestead.community_events
             order by status = 'cancelled', held_on < $1, case when held_on >= $1 then held_on end asc, held_on desc",
        )
        .bind(today)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(event).collect()
    }

    pub async fn get_community_event(&self, id: Uuid) -> Result<CommunityEvent> {
        let row: EventRow = sqlx::query_as(
            "select id, title, kind, held_on, time_note, place, description, attendance, status
             from homestead.community_events where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound("event"))?;
        event(row)
    }

    pub async fn update_community_event(&self, id: Uuid, input: EventEdit) -> Result<CommunityEvent> {
        let e = input.validated()?;
        let mut tx = self.begin().await?;
        let row: EventRow = sqlx::query_as(
            "update homestead.community_events
             set title = $2, kind = $3, held_on = $4, time_note = $5, place = $6, description = $7
             where id = $1
             returning id, title, kind, held_on, time_note, place, description, attendance, status",
        )
        .bind(id)
        .bind(&e.title)
        .bind(e.kind.as_str())
        .bind(e.held_on)
        .bind(&e.time_note)
        .bind(&e.place)
        .bind(&e.description)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::NotFound("event"))?;
        tx.commit().await?;
        event(row)
    }

    pub async fn set_community_event_status(&self, id: Uuid, status: EventStatus) -> Result<()> {
        let mut tx = self.begin().await?;
        let n = sqlx::query("update homestead.community_events set status = $2 where id = $1")
            .bind(id)
            .bind(status.as_str())
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        if n == 0 { Err(StoreError::NotFound("event")) } else { Ok(()) }
    }

    pub async fn event_plan(&self, id: Uuid) -> Result<EventPlan> {
        let event = self.get_community_event(id).await?;
        let rsvp_rows: Vec<(Uuid, String, String, String)> = sqlx::query_as(
            "select r.volunteer_id, v.name, r.availability, r.note
             from homestead.event_rsvps r join homestead.volunteers v on v.id = r.volunteer_id
             where r.event_id = $1 order by r.availability, lower(v.name)",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        let rsvps = rsvp_rows
            .into_iter()
            .map(|(volunteer_id, volunteer_name, availability, note)| {
                Ok(EventRsvp { volunteer_id, volunteer_name, availability: parse(&availability)?, note })
            })
            .collect::<Result<Vec<_>>>()?;
        type TaskRow = (
            Uuid,
            String,
            String,
            Option<Uuid>,
            Option<Date>,
            String,
            String,
            bool,
            Option<String>,
            Option<OffsetDateTime>,
            Option<Uuid>,
            Option<Uuid>,
        );
        let task_rows: Vec<TaskRow> = sqlx::query_as(
            "select t.id, t.title, t.details, t.location_id, t.due_on, t.priority, t.status,
                    t.mirror_to_linear, t.linear_issue_id, t.completed_at, t.template_id, t.assigned_to
             from homestead.event_tasks e join homestead.tasks t on t.id = e.task_id
             where e.event_id = $1 order by t.status, t.due_on nulls last, t.title",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        let tasks = task_rows
            .into_iter()
            .map(|r| {
                Ok(Task {
                    id: r.0,
                    title: r.1,
                    details: r.2,
                    location_id: r.3,
                    due_on: r.4,
                    priority: parse::<TaskPriority>(&r.5)?,
                    status: parse::<TaskStatus>(&r.6)?,
                    mirror_to_linear: r.7,
                    linear_issue_id: r.8,
                    completed_at: r.9,
                    template_id: r.10,
                    assigned_to: r.11,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let inventory = sqlx::query_as::<_, (Uuid, String, String, Decimal, Decimal, String)>(
            "select i.id, i.name, i.unit, e.quantity, i.on_hand, e.note
             from homestead.event_inventory e join homestead.inventory_items i on i.id = e.item_id
             where e.event_id = $1 order by lower(i.name)",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|r| EventInventory { item_id: r.0, name: r.1, unit: r.2, quantity: r.3, on_hand: r.4, note: r.5 })
        .collect();
        let equipment = sqlx::query_as::<_, (Uuid, String, bool, String)>(
            "select q.id, q.name, q.status = 'in_service', e.note
             from homestead.event_equipment e join homestead.equipment q on q.id = e.equipment_id
             where e.event_id = $1 order by lower(q.name)",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|r| EventEquipment { equipment_id: r.0, name: r.1, available: r.2, note: r.3 })
        .collect();
        Ok(EventPlan { event, rsvps, tasks, inventory, equipment })
    }

    pub async fn set_event_rsvp(
        &self,
        event_id: Uuid,
        volunteer_id: Uuid,
        availability: Availability,
        note: &str,
    ) -> Result<()> {
        let mut tx = self.begin().await?;
        sqlx::query(
            "insert into homestead.event_rsvps (event_id, volunteer_id, availability, note)
             values ($1, $2, $3, $4)
             on conflict (event_id, volunteer_id) do update
             set availability = excluded.availability, note = excluded.note, updated_at = now()",
        )
        .bind(event_id)
        .bind(volunteer_id)
        .bind(availability.as_str())
        .bind(note.trim())
        .execute(&mut *tx)
        .await
        .map_err(|e| match StoreError::from(e) {
            StoreError::NotFound(_) => StoreError::NotFound("event or volunteer"),
            other => other,
        })?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn link_event_task(&self, event_id: Uuid, task_id: Uuid) -> Result<()> {
        let mut tx = self.begin().await?;
        sqlx::query("insert into homestead.event_tasks (event_id, task_id) values ($1, $2) on conflict do nothing")
            .bind(event_id)
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn link_event_equipment(&self, event_id: Uuid, equipment_id: Uuid, note: &str) -> Result<()> {
        let mut tx = self.begin().await?;
        sqlx::query("insert into homestead.event_equipment (event_id, equipment_id, note) values ($1, $2, $3) on conflict (event_id, equipment_id) do update set note = excluded.note")
            .bind(event_id).bind(equipment_id).bind(note.trim()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn link_event_inventory(
        &self,
        event_id: Uuid,
        item_id: Uuid,
        quantity: Decimal,
        note: &str,
    ) -> Result<()> {
        if quantity <= Decimal::ZERO {
            return Err(homestead_domain::ValidationError::NotPositive("quantity").into());
        }
        let mut tx = self.begin().await?;
        sqlx::query("insert into homestead.event_inventory (event_id, item_id, quantity, note) values ($1, $2, $3, $4) on conflict (event_id, item_id) do update set quantity = excluded.quantity, note = excluded.note")
            .bind(event_id).bind(item_id).bind(quantity).bind(note.trim()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn unlink_event_resource(&self, event_id: Uuid, kind: &str, target_id: Uuid) -> Result<()> {
        let sql = match kind {
            "task" => "delete from homestead.event_tasks where event_id = $1 and task_id = $2",
            "inventory" => "delete from homestead.event_inventory where event_id = $1 and item_id = $2",
            "equipment" => "delete from homestead.event_equipment where event_id = $1 and equipment_id = $2",
            _ => return Err(StoreError::NotFound("event resource")),
        };
        let mut tx = self.begin().await?;
        sqlx::query(sql).bind(event_id).bind(target_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn set_attendance(&self, id: Uuid, attendance: i32) -> Result<()> {
        if attendance < 0 {
            return Err(homestead_domain::ValidationError::Negative("attendance").into());
        }
        let mut tx = self.begin().await?;
        let n = sqlx::query("update homestead.community_events set attendance = $2 where id = $1")
            .bind(id)
            .bind(attendance)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        if n == 0 { Err(StoreError::NotFound("event")) } else { Ok(()) }
    }

    pub async fn log_volunteer_shift(&self, input: NewVolunteerShift) -> Result<VolunteerShift> {
        let v = input.validated()?;
        let mut tx = self.begin().await?;
        let volunteer_id = volunteer_called(&mut tx, &v.person_name).await?;
        let shift = insert_shift(&mut tx, volunteer_id, v).await.map_err(|e| match e {
            StoreError::NotFound(_) => StoreError::NotFound("event"),
            other => other,
        })?;
        tx.commit().await?;
        Ok(shift)
    }

    pub async fn recent_volunteer_shifts(&self, limit: i64) -> Result<Vec<VolunteerShift>> {
        let rows: Vec<ShiftRow> = sqlx::query_as(
            "select id, volunteer_id, person_name, worked_on, hours, working_group, event_id, notes
             from homestead.volunteer_shifts order by worked_on desc, created_at desc limit $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(id, volunteer_id, person_name, worked_on, hours, group, event_id, notes)| {
                Ok(VolunteerShift {
                    id,
                    volunteer_id,
                    person_name,
                    worked_on,
                    hours,
                    working_group: parse(&group)?,
                    event_id,
                    notes,
                })
            })
            .collect()
    }

    /// Hours and distinct volunteers per working group, `from..=to`.
    pub async fn volunteer_hours(&self, from: Date, to: Date) -> Result<Vec<HoursByGroup>> {
        let rows: Vec<(String, Decimal, i64)> = sqlx::query_as(
            "select working_group, sum(hours), count(distinct volunteer_id)
             from homestead.volunteer_shifts where worked_on between $1 and $2
             group by working_group order by sum(hours) desc",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(g, hours, people)| Ok(HoursByGroup { working_group: parse(&g)?, hours, people }))
            .collect()
    }

    // ── Volunteers ──────────────────────────────────────────────────────────

    /// Everyone, active first then by name, with their hours `from..=to`
    /// and the last day they worked.
    pub async fn list_volunteers(&self, from: Date, to: Date) -> Result<Vec<(Volunteer, Decimal, Option<Date>)>> {
        type Row = (Uuid, String, Option<String>, String, bool, Vec<String>, Decimal, Option<Date>);
        let rows: Vec<Row> = sqlx::query_as(
            "select v.id, v.name, v.email, v.phone, v.active,
                    array(select n.name from homestead.volunteer_names n
                          where n.volunteer_id = v.id and n.name <> lower(btrim(v.name)) order by n.name),
                    coalesce((select sum(s.hours) from homestead.volunteer_shifts s
                              where s.volunteer_id = v.id and s.worked_on between $1 and $2), 0),
                    (select max(s.worked_on) from homestead.volunteer_shifts s where s.volunteer_id = v.id)
             from homestead.volunteers v order by not v.active, lower(v.name)",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, name, email, phone, active, other_names, hours, last)| {
                (Volunteer { id, name, email, phone, active, other_names }, hours, last)
            })
            .collect())
    }

    pub async fn active_volunteer_names(&self) -> Result<Vec<(Uuid, String)>> {
        Ok(sqlx::query_as("select id, name from homestead.volunteers where active order by lower(name)")
            .fetch_all(&self.pool)
            .await?)
    }

    /// Changes a volunteer's details. A new name is added to the names
    /// that find them (the old one keeps working); a name that already
    /// means someone else is refused, since that's a merge.
    pub async fn update_volunteer(&self, id: Uuid, input: VolunteerEdit) -> Result<()> {
        let v = input.validated()?;
        let mut tx = self.begin().await?;
        lock_name(&mut tx, &v.name).await?;
        let n =
            sqlx::query("update homestead.volunteers set name = $2, email = $3, phone = $4, active = $5 where id = $1")
                .bind(id)
                .bind(&v.name)
                .bind(&v.email)
                .bind(&v.phone)
                .bind(v.active)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        if n == 0 {
            return Err(StoreError::NotFound("volunteer"));
        }
        let owner: Uuid = sqlx::query_scalar(
            "with added as (insert into homestead.volunteer_names (name, volunteer_id) values (lower(btrim($1)), $2)
                            on conflict (name) do nothing returning volunteer_id)
             select volunteer_id from added
             union all select volunteer_id from homestead.volunteer_names where name = lower(btrim($1))
             limit 1",
        )
        .bind(&v.name)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if owner != id {
            return Err(StoreError::Conflict("another volunteer already goes by that name; merge them instead".into()));
        }
        tx.commit().await?;
        Ok(())
    }

    /// Folds `from` into `into`: their shifts, visits and names move over,
    /// contact details fill any `into` lacks, and `from` is removed. Refused
    /// while both are signed in, since one person can't be at the gate twice.
    pub async fn merge_volunteers(&self, from: Uuid, into: Uuid) -> Result<()> {
        if from == into {
            return Err(StoreError::Conflict("that's the same volunteer".into()));
        }
        let mut tx = self.begin().await?;
        let locked: Vec<Uuid> =
            sqlx::query_scalar("select id from homestead.volunteers where id = any($1) order by id for update")
                .bind([from, into].as_slice())
                .fetch_all(&mut *tx)
                .await?;
        if locked.len() != 2 {
            return Err(StoreError::NotFound("volunteer"));
        }
        let both_in: bool = sqlx::query_scalar(
            "select count(distinct volunteer_id) = 2 from homestead.volunteer_visits
             where volunteer_id = any($1) and out_at is null",
        )
        .bind([from, into].as_slice())
        .fetch_one(&mut *tx)
        .await?;
        if both_in {
            return Err(StoreError::Conflict("both are signed in at the gate; sign one out first".into()));
        }
        for sql in [
            "update homestead.volunteer_shifts set volunteer_id = $2 where volunteer_id = $1",
            "update homestead.volunteer_visits set volunteer_id = $2 where volunteer_id = $1",
            "update homestead.volunteer_names set volunteer_id = $2 where volunteer_id = $1",
            "update homestead.volunteers i set email = coalesce(i.email, f.email),
                    phone = case when i.phone = '' then f.phone else i.phone end, active = i.active or f.active
             from homestead.volunteers f where f.id = $1 and i.id = $2",
            "delete from homestead.volunteers where id = $1",
        ] {
            sqlx::query(sql).bind(from).bind(into).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    // ── The gate ────────────────────────────────────────────────────────────

    /// Signs someone in. Signing in twice keeps the first time, so a second
    /// scan doesn't cut anyone's hours.
    pub async fn sign_in_at_gate(&self, name: &str, group: WorkingGroup) -> Result<Visit> {
        let name = homestead_domain::tags::visitor_name(name)?;
        let mut tx = self.begin().await?;
        let volunteer_id = volunteer_called(&mut tx, &name).await?;
        sqlx::query(
            "insert into homestead.volunteer_visits (volunteer_id, person_name, actor_id, working_group)
             values ($1, $2, nullif(current_setting('homestead.actor_id', true), '')::uuid, $3)
             on conflict (volunteer_id) where out_at is null do nothing",
        )
        .bind(volunteer_id)
        .bind(&name)
        .bind(group.as_str())
        .execute(&mut *tx)
        .await?;
        let visit = open_visit(&mut tx, volunteer_id).await?.ok_or(StoreError::NotFound("visit"))?;
        tx.commit().await?;
        Ok(visit)
    }

    /// Signs someone out and records the shift, in one transaction. A visit
    /// too long to be real (a forgotten check-out) closes without hours;
    /// the second value is `None` then.
    pub async fn sign_out_at_gate(&self, name: &str) -> Result<(Visit, Option<VolunteerShift>)> {
        let name = homestead_domain::tags::visitor_name(name)?;
        let mut tx = self.begin().await?;
        let not_in =
            || StoreError::Conflict(format!("nobody is checked in as {name}; check the spelling, or check in first"));
        let volunteer_id: Uuid =
            sqlx::query_scalar("select volunteer_id from homestead.volunteer_names where name = lower(btrim($1))")
                .bind(&name)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(not_in)?;
        let open = open_visit(&mut tx, volunteer_id).await?.ok_or_else(not_in)?;
        let now: OffsetDateTime = sqlx::query_scalar("select now()").fetch_one(&mut *tx).await?;
        let shift = match gate_hours(now - open.in_at) {
            Some(hours) => {
                let worked_on: Date = sqlx::query_scalar("select ($1 at time zone $2)::date")
                    .bind(open.in_at)
                    .bind(&*self.timezone)
                    .fetch_one(&mut *tx)
                    .await?;
                let shift = NewVolunteerShift {
                    person_name: open.person_name.clone(),
                    worked_on,
                    hours,
                    working_group: open.working_group,
                    event_id: None,
                    notes: "signed in at the gate".into(),
                }
                .validated()?;
                Some(insert_shift(&mut tx, volunteer_id, shift).await?)
            }
            None => None,
        };
        sqlx::query("update homestead.volunteer_visits set out_at = $2, shift_id = $3 where id = $1")
            .bind(open.id)
            .bind(now)
            .bind(shift.as_ref().map(|s| s.id))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok((Visit { out_at: Some(now), shift_id: shift.as_ref().map(|s| s.id), ..open }, shift))
    }

    /// Who's signed in now, earliest first, and whether each has a signed
    /// waiver or volunteer agreement on file under any name they go by.
    pub async fn on_site(&self) -> Result<Vec<(Visit, bool)>> {
        type Row = (Uuid, Uuid, String, String, OffsetDateTime, Option<OffsetDateTime>, Option<Uuid>, bool);
        let rows: Vec<Row> = sqlx::query_as(
            "select v.id, v.volunteer_id, v.person_name, v.working_group, v.in_at, v.out_at, v.shift_id,
                    exists (select 1 from homestead.release_requests r
                            join homestead.volunteer_names n on n.name = lower(btrim(r.signer_name))
                            where r.status = 'signed' and r.release_key in ('liability-waiver', 'volunteer-agreement')
                              and n.volunteer_id = v.volunteer_id)
             from homestead.volunteer_visits v where v.out_at is null order by v.in_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(id, volunteer_id, name, group, in_at, out_at, shift_id, waiver)| {
                Ok((visit((id, volunteer_id, name, group, in_at, out_at, shift_id))?, waiver))
            })
            .collect()
    }
}

/// Serialises everyone resolving or claiming the same name, so two first
/// sign-ins as "Sam" at once make one volunteer, not a failed insert.
async fn lock_name(tx: &mut crate::Tx<'_>, name: &str) -> Result<()> {
    sqlx::query("select pg_advisory_xact_lock(hashtext('volunteer:' || lower(btrim($1))))")
        .bind(name)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// The volunteer a typed name means, made on first use. Anyone who turns
/// up again is active again.
async fn volunteer_called(tx: &mut crate::Tx<'_>, name: &str) -> Result<Uuid> {
    lock_name(tx, name).await?;
    let found: Option<Uuid> =
        sqlx::query_scalar("select volunteer_id from homestead.volunteer_names where name = lower(btrim($1))")
            .bind(name)
            .fetch_optional(&mut **tx)
            .await?;
    let id = match found {
        Some(id) => {
            sqlx::query("update homestead.volunteers set active = true where id = $1 and not active")
                .bind(id)
                .execute(&mut **tx)
                .await?;
            id
        }
        None => {
            let id: Uuid =
                sqlx::query_scalar("insert into homestead.volunteers (name) values (btrim($1)) returning id")
                    .bind(name)
                    .fetch_one(&mut **tx)
                    .await?;
            sqlx::query("insert into homestead.volunteer_names (name, volunteer_id) values (lower(btrim($1)), $2)")
                .bind(name)
                .bind(id)
                .execute(&mut **tx)
                .await?;
            id
        }
    };
    Ok(id)
}

async fn insert_shift(tx: &mut crate::Tx<'_>, volunteer_id: Uuid, v: NewVolunteerShift) -> Result<VolunteerShift> {
    let id: Uuid = sqlx::query_scalar(
        "insert into homestead.volunteer_shifts
           (volunteer_id, person_name, worked_on, hours, working_group, event_id, notes)
         values ($1, $2, $3, $4, $5, $6, $7) returning id",
    )
    .bind(volunteer_id)
    .bind(&v.person_name)
    .bind(v.worked_on)
    .bind(v.hours)
    .bind(v.working_group.as_str())
    .bind(v.event_id)
    .bind(&v.notes)
    .fetch_one(&mut **tx)
    .await?;
    Ok(VolunteerShift {
        id,
        volunteer_id,
        person_name: v.person_name,
        worked_on: v.worked_on,
        hours: v.hours,
        working_group: v.working_group,
        event_id: v.event_id,
        notes: v.notes,
    })
}

type VisitRow = (Uuid, Uuid, String, String, OffsetDateTime, Option<OffsetDateTime>, Option<Uuid>);

fn visit(r: VisitRow) -> Result<Visit> {
    let (id, volunteer_id, person_name, group, in_at, out_at, shift_id) = r;
    Ok(Visit { id, volunteer_id, person_name, working_group: parse(&group)?, in_at, out_at, shift_id })
}

async fn open_visit(tx: &mut crate::Tx<'_>, volunteer_id: Uuid) -> Result<Option<Visit>> {
    let row: Option<VisitRow> = sqlx::query_as(
        "select v.id, v.volunteer_id, v.person_name, v.working_group, v.in_at, v.out_at, v.shift_id
         from homestead.volunteer_visits v where v.volunteer_id = $1 and v.out_at is null for update",
    )
    .bind(volunteer_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.map(visit).transpose()
}

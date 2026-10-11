//! Incident reports.

use homestead_domain::Event;
use homestead_domain::incidents::{FollowUp, Incident, IncidentStatus, NewIncident};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::{Result, Store, StoreError, outbox, parse};

macro_rules! incident_cols {
    () => {
        "i.id, i.occurred_at, i.location_id, l.name, i.location_detail, i.kind, i.description, i.immediate_action,
         i.people_involved, i.witnesses, i.treatment, i.corrective_action, i.follow_up_by, i.status, i.closure_notes,
         i.reported_by, i.reported_by_name, i.reported_at, i.closed_at, c.display_name
         from homestead.incidents i
         left join homestead.locations l on l.id = i.location_id
         left join homestead.actors c on c.id = i.closed_by"
    };
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    occurred_at: OffsetDateTime,
    location_id: Option<Uuid>,
    name: Option<String>,
    location_detail: String,
    kind: String,
    description: String,
    immediate_action: String,
    people_involved: String,
    witnesses: String,
    treatment: Option<String>,
    corrective_action: String,
    follow_up_by: Option<Date>,
    status: String,
    closure_notes: String,
    reported_by: Option<Uuid>,
    reported_by_name: String,
    reported_at: OffsetDateTime,
    closed_at: Option<OffsetDateTime>,
    display_name: Option<String>,
}

fn incident(r: Row) -> Result<Incident> {
    Ok(Incident {
        id: r.id,
        occurred_at: r.occurred_at,
        location_id: r.location_id,
        location_name: r.name,
        location_detail: r.location_detail,
        kind: parse(&r.kind)?,
        description: r.description,
        immediate_action: r.immediate_action,
        people_involved: r.people_involved,
        witnesses: r.witnesses,
        treatment: r.treatment.as_deref().map(parse).transpose()?,
        corrective_action: r.corrective_action,
        follow_up_by: r.follow_up_by,
        status: parse(&r.status)?,
        closure_notes: r.closure_notes,
        reported_by: r.reported_by,
        reported_by_name: r.reported_by_name,
        reported_at: r.reported_at,
        closed_at: r.closed_at,
        closed_by_name: r.display_name,
    })
}

impl Store {
    /// Files a report as the acting person and announces only its kind.
    pub async fn report_incident(&self, input: NewIncident) -> Result<Incident> {
        let now = OffsetDateTime::now_utc();
        let n = input.validated(now)?;
        let (reporter, name) = match self.actor() {
            Some(a) => (a.id, a.label.clone()),
            None => (None, "system".to_owned()),
        };
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.incidents
               (occurred_at, location_id, location_detail, kind, description, immediate_action, people_involved,
                witnesses, treatment, reported_by, reported_by_name)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) returning id",
        )
        .bind(n.occurred_at)
        .bind(n.location_id)
        .bind(&n.location_detail)
        .bind(n.kind.as_str())
        .bind(&n.description)
        .bind(&n.immediate_action)
        .bind(&n.people_involved)
        .bind(&n.witnesses)
        .bind(n.treatment.map(|t| t.as_str()))
        .bind(reporter)
        .bind(&name)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match StoreError::from(e) {
            StoreError::NotFound(_) => StoreError::NotFound("location"),
            other => other,
        })?;
        let row: Row = sqlx::query_as(concat!("select ", incident_cols!(), " where i.id = $1"))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let incident = incident(row)?;
        outbox::emit(&mut tx, &Event::IncidentReported { reference: incident.reference(), kind: incident.kind })
            .await?;
        tx.commit().await?;
        Ok(incident)
    }

    pub async fn get_incident(&self, id: Uuid) -> Result<Incident> {
        let row: Option<Row> = sqlx::query_as(concat!("select ", incident_cols!(), " where i.id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        incident(row.ok_or(StoreError::NotFound("incident"))?)
    }

    /// Open ones first, newest first. `reporter` limits it to one person's
    /// own reports.
    pub async fn list_incidents(&self, reporter: Option<Uuid>, limit: i64) -> Result<Vec<Incident>> {
        let rows: Vec<Row> = sqlx::query_as(concat!(
            "select ",
            incident_cols!(),
            " where ($1::uuid is null or i.reported_by = $1)
              order by i.status = 'closed', i.occurred_at desc limit $2"
        ))
        .bind(reporter)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(incident).collect()
    }

    /// Who filed an incident and whether it's still open (for letting a
    /// reporter add evidence to their own open report).
    pub async fn incident_reporter(&self, id: Uuid) -> Result<Option<(Option<Uuid>, IncidentStatus)>> {
        let row: Option<(Option<Uuid>, String)> =
            sqlx::query_as("select reported_by, status from homestead.incidents where id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        row.map(|(by, status)| Ok((by, parse(&status)?))).transpose()
    }

    pub async fn follow_up_incident(&self, id: Uuid, input: FollowUp) -> Result<Incident> {
        let f = input.validated()?;
        let mut tx = self.begin().await?;
        let n = sqlx::query("update homestead.incidents set corrective_action = $2, follow_up_by = $3 where id = $1")
            .bind(id)
            .bind(&f.corrective_action)
            .bind(f.follow_up_by)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        if n == 0 {
            return Err(StoreError::NotFound("incident"));
        }
        self.get_incident(id).await
    }

    /// Closes an incident; it must say what was done about it first.
    pub async fn close_incident(&self, id: Uuid, notes: &str) -> Result<Incident> {
        let mut tx = self.begin().await?;
        let row: Option<(String, String)> =
            sqlx::query_as("select status, corrective_action from homestead.incidents where id = $1 for update")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let (status, corrective) = row.ok_or(StoreError::NotFound("incident"))?;
        if status == IncidentStatus::Closed.as_str() {
            return Err(StoreError::Conflict("already closed".into()));
        }
        if corrective.trim().is_empty() {
            return Err(StoreError::Conflict("record the corrective action before closing".into()));
        }
        sqlx::query(
            "update homestead.incidents set status = 'closed', closed_at = now(), closed_by = $2, closure_notes = $3
             where id = $1",
        )
        .bind(id)
        .bind(self.actor().and_then(|a| a.id))
        .bind(notes.trim())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.get_incident(id).await
    }
}

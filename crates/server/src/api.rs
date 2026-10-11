//! JSON API (`/api/v1`) for other systems and scripts.

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use homestead_domain::{NewHarvest, NewInventoryItem, NewLocation, NewPlanting, NewTask, StockAdjustment};
use homestead_store::StoreError;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::{Can, cap};
use crate::{AppState, status_of};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/locations", get(list_locations).post(create_location))
        .route("/plantings", get(list_plantings).post(create_planting))
        .route("/harvests", post(log_harvest))
        .route("/tasks", get(list_tasks).post(create_task))
        .route("/tasks/{id}/complete", post(complete_task))
        .route("/tasks/{id}/assign", post(assign_task))
        .route("/inventory", get(list_inventory).post(create_item))
        .route("/inventory/{id}/adjust", post(adjust_stock))
        .route("/events", get(recent_events))
        .route("/audit", get(recent_audit))
        .route("/bus", get(bus))
        .route("/metrics", get(metrics))
}

pub(crate) struct ApiError(StoreError);

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = status_of(&self.0);
        (status, Json(json!({ "error": message }))).into_response()
    }
}

pub(crate) type ApiResult<T = Response> = Result<T, ApiError>;

pub(crate) fn created<T: Serialize>(v: T) -> Response {
    (StatusCode::CREATED, Json(v)).into_response()
}

async fn list_locations(c: Can<cap::GardenRead>) -> ApiResult<Response> {
    Ok(Json(c.store.list_locations().await?).into_response())
}

async fn create_location(c: Can<cap::GardenWrite>, Json(b): Json<NewLocation>) -> ApiResult<Response> {
    Ok(created(c.store.create_location(b).await?))
}

async fn list_plantings(c: Can<cap::GardenRead>) -> ApiResult<Response> {
    let rows = c.store.list_active_plantings().await?;
    let out: Vec<_> = rows
        .into_iter()
        .map(|(p, location_name)| {
            let mut v = serde_json::to_value(p).unwrap_or_default();
            v["location_name"] = json!(location_name);
            v
        })
        .collect();
    Ok(Json(out).into_response())
}

async fn create_planting(c: Can<cap::GardenWrite>, Json(b): Json<NewPlanting>) -> ApiResult<Response> {
    Ok(created(c.store.create_planting(b).await?))
}

async fn log_harvest(c: Can<cap::GardenWrite>, Json(b): Json<NewHarvest>) -> ApiResult<Response> {
    Ok(created(c.store.log_harvest(b).await?))
}

async fn list_tasks(c: Can<cap::TasksRead>) -> ApiResult<Response> {
    Ok(Json(c.store.list_open_tasks().await?).into_response())
}

async fn create_task(c: Can<cap::TasksWrite>, Json(b): Json<NewTask>) -> ApiResult<Response> {
    Ok(created(c.store.create_task(b).await?))
}

async fn complete_task(c: Can<cap::TasksWrite>, Path(id): Path<Uuid>) -> ApiResult<Response> {
    Ok(Json(c.store.complete_task(id).await?).into_response())
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AssignmentValue {
    Person(Uuid),
    Unassigned(()),
}

impl AssignmentValue {
    fn person(self) -> Option<Uuid> {
        match self {
            Self::Person(id) => Some(id),
            Self::Unassigned(()) => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskAssignment {
    /// A person to give the task to; `null` puts it back up for grabs.
    assigned_to: AssignmentValue,
}

async fn assign_task(
    c: Can<cap::TasksWrite>,
    Path(id): Path<Uuid>,
    Json(b): Json<TaskAssignment>,
) -> ApiResult<Response> {
    Ok(Json(c.store.assign_task(id, b.assigned_to.person()).await?).into_response())
}

async fn list_inventory(c: Can<cap::InventoryRead>) -> ApiResult<Response> {
    Ok(Json(c.store.list_inventory().await?).into_response())
}

async fn create_item(c: Can<cap::InventoryAdjust>, Json(b): Json<NewInventoryItem>) -> ApiResult<Response> {
    Ok(created(c.store.create_inventory_item(b).await?))
}

async fn adjust_stock(
    c: Can<cap::InventoryAdjust>,
    Path(id): Path<Uuid>,
    Json(b): Json<StockAdjustment>,
) -> ApiResult<Response> {
    Ok(Json(c.store.adjust_stock(id, b).await?).into_response())
}

#[derive(Deserialize)]
struct Limit {
    limit: Option<i64>,
}

async fn recent_events(c: Can<cap::SystemsRead>, Query(q): Query<Limit>) -> ApiResult<Response> {
    let events = c.store.recent_events(q.limit.unwrap_or(50).clamp(1, 500)).await?;
    let out: Vec<_> = events
        .into_iter()
        .map(|e| json!({ "id": e.id, "kind": e.kind, "payload": e.payload, "occurred_at": e.occurred_at.to_string() }))
        .collect();
    Ok(Json(out).into_response())
}

async fn bus(c: Can<cap::SystemsRead>) -> ApiResult<Response> {
    let st = c.store.bus_stats().await?;
    Ok(Json(json!({
        "unfanned": st.unfanned, "pending": st.pending, "dead": st.dead, "delivered": st.delivered
    }))
    .into_response())
}

async fn recent_audit(c: Can<cap::AuditRead>, Query(q): Query<Limit>) -> ApiResult<Response> {
    Ok(Json(c.store.recent_audit_entries(c.who.caps, q.limit.unwrap_or(50).clamp(1, 500)).await?).into_response())
}

async fn metrics(c: Can<cap::SystemsRead>) -> ApiResult<Response> {
    let bus = c.store.bus_stats().await?;
    let (audit, zeffy, idempotency, restore_age): (i64, i64, i64, Option<f64>) = sqlx::query_as(
        "select
           (select count(*) from homestead.audit_entries),
           (select count(*) from homestead.zeffy_webhook_events),
           (select count(*) from homestead.idempotency_records),
           (select extract(epoch from now() - max(tested_at))::float8 from homestead.restore_tests where integrity_ok)",
    )
    .fetch_one(c.store.pool())
    .await
    .map_err(StoreError::from)?;
    let body = format!(
        "# TYPE homestead_bus_events gauge\n\
         homestead_bus_events{{state=\"unfanned\"}} {}\n\
         homestead_bus_events{{state=\"pending\"}} {}\n\
         homestead_bus_events{{state=\"dead\"}} {}\n\
         homestead_bus_events{{state=\"delivered\"}} {}\n\
         # TYPE homestead_audit_entries_total counter\n\
         homestead_audit_entries_total {audit}\n\
         # TYPE homestead_zeffy_events_total counter\n\
         homestead_zeffy_events_total {zeffy}\n\
         # TYPE homestead_idempotency_records_total counter\n\
         homestead_idempotency_records_total {idempotency}\n\
         # TYPE homestead_last_restore_test_age_seconds gauge\n\
         homestead_last_restore_test_age_seconds {}\n",
        bus.unfanned,
        bus.pending,
        bus.dead,
        bus.delivered,
        restore_age.unwrap_or(f64::NAN)
    );
    let body = body + &crate::observe::render();
    Ok(([(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")], body).into_response())
}

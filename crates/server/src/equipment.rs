//! Equipment: what we have, where, in what shape, and its service history.

use axum::extract::Path;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use homestead_domain::attachments::AttachmentOwner;
use homestead_domain::auth::Capability;
use homestead_domain::equipment::{Condition, EquipmentState, EquipmentStatus, NewEquipment, NewMaintenance};
use maud::html;
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::api::{ApiResult, created};
use crate::auth::{Can, cap};
use crate::ui::{Back, UiError, UiResult, layout, opt_date, opt_money, options, req_date, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/equipment", get(list).post(create))
        .route("/equipment/{id}", get(detail))
        .route("/equipment/{id}/state", post(set_state))
        .route("/equipment/{id}/maintenance", post(add_maintenance))
}

pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/equipment", get(api_list).post(api_create))
        .route("/equipment/{id}", get(api_get))
        .route("/equipment/{id}/maintenance", post(api_add_maintenance))
}

const B: &str = "/equipment";

fn opt_uuid(s: &str, back: &str) -> Result<Option<Uuid>, UiError> {
    match s.trim() {
        "" => Ok(None),
        v => v.parse().map(Some).map_err(|_| UiError::invalid_at("unknown selection", back)),
    }
}

async fn list(c: Can<cap::EquipmentRead>) -> UiResult {
    let items = c.store.list_equipment().await.back(B)?;
    let today = c.store.today().await.back(B)?;
    let locations = c.store.list_locations().await.back(B)?;
    let loans = c.store.open_loans().await.back(B)?;
    Ok(layout(
        "Equipment",
        html! {
            @if items.is_empty() { p.muted { "Nothing recorded yet." } } @else {
                table {
                    tr { th { "What" } th { "Where" } th { "Shape" } th { "Status" } th { "Next service" } }
                    @for e in &items {
                        @let loan = loans.iter().find(|l| l.equipment_id == e.id);
                        tr {
                            td { a href={"/equipment/" (e.id)} { (e.name) } span.muted { " · " (e.category) } }
                            td class=[loan.is_some_and(|l| l.overdue(today)).then_some("warn")] {
                                @match loan {
                                    Some(l) => { "with " (l.borrower) }
                                    None => { (e.location_name.as_deref().unwrap_or("—")) }
                                }
                            }
                            td { (e.condition) }
                            td class=[(e.status != EquipmentStatus::InService).then_some("warn")] { (e.status) }
                            td class=[e.service_due(today).then_some("warn")] { @if let Some(d) = e.next_service_on { (crate::ui::Day(d)) } }
                        }
                    }
                }
            }
            @if c.who.can(Capability::EquipmentWrite) {
                form.card method="post" action="/equipment" {
                    strong { "Add equipment" }
                    div.grid {
                        label { "Name" input name="name" required placeholder="Tiller, egg washer, chainsaw…"; }
                        label { "Category" input name="category" required placeholder="power tools, kitchen, irrigation…"; }
                        label { "Make" input name="manufacturer"; }
                        label { "Model" input name="model"; }
                        label { "Serial number" input name="serial_number"; }
                        label { "Bought on" input name="purchased_on" type="date"; }
                        label { "Cost (USD)" input name="purchase_cost" inputmode="decimal"; }
                        label { "Kept at" select name="location_id" {
                            option value="" { "—" }
                            @for l in &locations { option value=(l.id) { (l.name) } }
                        } }
                        label { "Shape" select name="condition" { (options(Condition::ALL, Some(Condition::Good))) } }
                    }
                    label { "Notes" input name="notes"; }
                    button { "Add" }
                }
            }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct EquipmentForm {
    name: String,
    category: String,
    #[serde(default)]
    manufacturer: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    serial_number: String,
    #[serde(default)]
    purchased_on: String,
    #[serde(default)]
    purchase_cost: String,
    #[serde(default)]
    location_id: String,
    condition: String,
    #[serde(default)]
    notes: String,
}

async fn create(c: Can<cap::EquipmentWrite>, Form(f): Form<EquipmentForm>) -> UiResult {
    let input = NewEquipment {
        name: f.name,
        category: f.category,
        manufacturer: f.manufacturer,
        model: f.model,
        serial_number: f.serial_number,
        purchased_on: opt_date(&f.purchased_on, "bought on", B)?,
        purchase_cost: opt_money(&f.purchase_cost, "cost", B)?,
        purchase_id: None,
        location_id: opt_uuid(&f.location_id, B)?,
        condition: f.condition.parse::<Condition>().back(B)?,
        notes: f.notes,
    };
    let e = c.store.create_equipment(input).await.back(B)?;
    Ok(see(&format!("/equipment/{}", e.id)))
}

async fn detail(c: Can<cap::EquipmentRead>, Path(id): Path<Uuid>) -> UiResult {
    let here = format!("/equipment/{id}");
    let e = c.store.get_equipment(id).await.back(B)?;
    let history = c.store.maintenance_history(id).await.back(B)?;
    let files = c.store.list_attachments(AttachmentOwner::Equipment, id).await.back(B)?;
    let mut service_files = Vec::with_capacity(history.len());
    for m in &history {
        service_files.push(c.store.list_attachments(AttachmentOwner::EquipmentMaintenance, m.id).await.back(B)?);
    }
    let today = c.store.today().await.back(B)?;
    let locations = c.store.list_locations().await.back(B)?;
    let loans = c.store.loan_history(id, 10).await.back(B)?;
    let can_write = c.who.can(Capability::EquipmentWrite);
    let moved =
        crate::movements::section(&c.who, &c.store, homestead_domain::movements::Mover::Equipment(id), &locations)
            .await
            .back(B)?;
    let show_money = c.who.can(Capability::PurchasesRead);
    Ok(layout(
        &e.name,
        html! {
            p {
                span.pill { (e.status) } " " span.pill { (e.condition) } " · " (e.category)
                @if !e.manufacturer.is_empty() || !e.model.is_empty() { " · " (e.manufacturer) " " (e.model) }
                @if !e.serial_number.is_empty() { " · serial " code { (e.serial_number) } }
                @if let Some(l) = &e.location_name { " · at " (l) }
            }
            p.muted {
                @if let Some(d) = e.purchased_on { "Bought " (crate::ui::Day(d)) }
                @if show_money { @if let Some(cost) = e.purchase_cost { " for $" (cost) } }
                @if let Some(p) = e.purchase_id { @if show_money { " · " a href={"/purchases/" (p)} { "purchase" } } }
            }
            @if let Some(n) = e.next_service_on {
                p class=[e.service_due(today).then_some("warn")] { "Next service " (n) @if e.service_due(today) { " — due" } }
            }
            @if !e.notes.is_empty() { p.muted { (e.notes) } }
            @if let Some(l) = loans.first().filter(|l| l.returned_at.is_none()) {
                p class=[l.overdue(today).then_some("warn")] {
                    "Out with " strong { (l.borrower) } " since " (l.out_at.date())
                    @if let Some(d) = l.due_on { ", due back " (crate::ui::Day(d)) }
                }
            }
            (crate::tags::make_button(&c.who, homestead_domain::tags::TagKind::Equipment, id))
            @if can_write {
                form.inline method="post" action={(here) "/state"} {
                    select name="status" { (options(EquipmentStatus::ALL, Some(e.status))) }
                    select name="condition" { (options(Condition::ALL, Some(e.condition))) }
                    select name="location_id" {
                        option value="" { "—" }
                        @for l in &locations { option value=(l.id) selected[Some(l.id) == e.location_id] { (l.name) } }
                    }
                    button.small { "Update" }
                }
            }
            (moved)
            h2 { "Manuals and photos" }
            (crate::attachments::panel(&c.who, AttachmentOwner::Equipment, id, &files, &here))
            h2 { "Service history" }
            @if history.is_empty() { p.muted { "No service recorded." } } @else {
                table {
                    tr { th { "Date" } th { "Work" } th { "By" } th { "Cost" } th { "Next" } }
                    @for (m, files) in history.iter().zip(&service_files) {
                        tr {
                            td { (crate::ui::Day(m.performed_on)) }
                            td { (m.work) @if !m.notes.is_empty() { br; span.muted { (m.notes) } }
                                 (crate::attachments::panel(&c.who, AttachmentOwner::EquipmentMaintenance, m.id, files, &here)) }
                            td { (m.vendor_name.as_deref().unwrap_or(&m.performed_by)) }
                            td { @if let Some(cost) = m.cost { "$" (cost) } }
                            td { @if let Some(n) = m.next_service_on { (n) } }
                        }
                    }
                }
            }
            @if !loans.is_empty() {
                h2 { "Who's had it" }
                table {
                    tr { th { "Out" } th { "Who" } th { "Back" } }
                    @for l in &loans {
                        tr {
                            td { (l.out_at.date()) }
                            td { (l.borrower) }
                            td {
                                @match l.returned_at { Some(t) => { (t.date()) } None => { "still out" } }
                                @if !l.return_note.is_empty() { br; span.muted { (l.return_note) } }
                            }
                        }
                    }
                }
            }
            @if can_write {
                form.card method="post" action={(here) "/maintenance"} {
                    strong { "Record a service or repair" }
                    div.grid {
                        label { "Date" input name="performed_on" type="date" value=(today) required; }
                        label { "Cost (USD)" input name="cost" inputmode="decimal"; }
                        label { "Done by" input name="performed_by" placeholder="a person, or the shop"; }
                        label { "Next service" input name="next_service_on" type="date"; }
                    }
                    label { "What was done" input name="work" required; }
                    label { "Notes" input name="notes"; }
                    label.inline { input type="checkbox" name="remind" checked; "Add a task for the next service" }
                    button { "Record" }
                }
            }
            p { a href="/equipment" { "← Equipment" } }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct StateForm {
    status: String,
    condition: String,
    #[serde(default)]
    location_id: String,
}

async fn set_state(c: Can<cap::EquipmentWrite>, Path(id): Path<Uuid>, Form(f): Form<StateForm>) -> UiResult {
    let here = format!("/equipment/{id}");
    let state = EquipmentState {
        status: f.status.parse::<EquipmentStatus>().back_to(&here)?,
        condition: f.condition.parse::<Condition>().back_to(&here)?,
        location_id: opt_uuid(&f.location_id, &here)?,
    };
    c.store.set_equipment_state(id, state).await.back_to(&here)?;
    Ok(see(&here))
}

#[derive(Deserialize)]
struct MaintenanceForm {
    performed_on: String,
    work: String,
    #[serde(default)]
    cost: String,
    #[serde(default)]
    performed_by: String,
    #[serde(default)]
    next_service_on: String,
    #[serde(default)]
    notes: String,
    remind: Option<String>,
}

async fn add_maintenance(
    c: Can<cap::EquipmentWrite>,
    Path(id): Path<Uuid>,
    Form(f): Form<MaintenanceForm>,
) -> UiResult {
    let here = format!("/equipment/{id}");
    let next_service_on = opt_date(&f.next_service_on, "next service", B)?;
    let input = NewMaintenance {
        performed_on: req_date(&f.performed_on, "date", B)?,
        work: f.work,
        cost: opt_money(&f.cost, "cost", B)?,
        performed_by: f.performed_by,
        vendor_id: None,
        // Only when there's a date to remind about.
        remind: f.remind.is_some() && next_service_on.is_some(),
        next_service_on,
        notes: f.notes,
    };
    c.store.add_maintenance(id, input).await.back_to(&here)?;
    Ok(see(&here))
}

async fn api_list(c: Can<cap::EquipmentRead>) -> ApiResult {
    Ok(Json(c.store.list_equipment().await?).into_response())
}

async fn api_create(c: Can<cap::EquipmentWrite>, Json(b): Json<NewEquipment>) -> ApiResult {
    Ok(created(c.store.create_equipment(b).await?))
}

async fn api_get(c: Can<cap::EquipmentRead>, Path(id): Path<Uuid>) -> ApiResult {
    Ok(Json(serde_json::json!({
        "equipment": c.store.get_equipment(id).await?,
        "maintenance": c.store.maintenance_history(id).await?,
    }))
    .into_response())
}

async fn api_add_maintenance(
    c: Can<cap::EquipmentWrite>,
    Path(id): Path<Uuid>,
    Json(b): Json<NewMaintenance>,
) -> ApiResult {
    Ok(created(c.store.add_maintenance(id, b).await?))
}

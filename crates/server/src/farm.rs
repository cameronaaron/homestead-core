//! Farm operations that sit on the garden, animals, and tasks already recorded.

use axum::extract::Path;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Router};
use homestead_domain::auth::Capability;
use maud::html;
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Can, cap};
use crate::ui::{Back, UiError, UiResult, decimal, layout, opt_date, opt_uuid, req_date, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/farm", get(page))
        .route("/farm/eggs", post(eggs))
        .route("/farm/compost", post(compost))
        .route("/farm/checklists", post(checklist))
        .route("/farm/losses", post(loss))
        .route("/farm/obligations", post(obligation))
        .route("/farm/obligations/{id}/done", post(obligation_done))
        .route("/farm/emergency", post(emergency))
        .route("/farm/costs", post(cost))
        .route("/farm/zones", post(zone))
}

const B: &str = "/farm";

async fn page(c: Can<cap::GardenRead>) -> UiResult {
    let places = c.store.list_locations().await.back(B)?;
    let write = c.who.can(Capability::GardenWrite);
    Ok(layout(
        "Farm",
        html! {
            p.muted { a href="/crops" { "Crop plans" } " and " a href="/donations" { "produce donations" } " have their own pages, " a href="/health" { "crop health" } " too; a harvest's page records where it went and a place's page its soil. Here: losses, checklists, obligations, emergency notes, irrigation zones and compost batches. "
                @if c.who.can(Capability::ReportsRead) { a href="/report" { "Year report" } } }
            @if write {
                form.card method="post" action="/farm/losses" {
                    strong { "Loss" }
                    div.grid {
                        label { "Date" input type="date" name="lost_on" required; }
                        label { "Reason" input name="reason" required; }
                        label { "Quantity" input name="quantity"; }
                        label { "Unit" input name="unit"; }
                    }
                    button { "Record loss" }
                }
                form.card method="post" action="/farm/checklists" {
                    strong { "Checklist" }
                    label { "Name" input name="name" required placeholder="Morning chicken check"; }
                    label { "Items, one per line" textarea name="items" required rows="4" {} }
                    button { "Start today's run" }
                }
                form.card method="post" action="/farm/obligations" {
                    strong { "Obligation" }
                    label { "Requirement" input name="requirement" required; }
                    div.grid {
                        label { "Authority" input name="authority"; }
                        label { "Due" input type="date" name="due_on"; }
                    }
                    button { "Add" }
                }
                form.card method="post" action="/farm/emergency" {
                    strong { "Emergency note" }
                    label { "Scenario" input name="scenario" required placeholder="Power outage"; }
                    label { "Procedure" textarea name="procedure" rows="3" {} }
                    label { "Contacts" input name="contacts"; }
                    button { "Save" }
                }
                form.card method="post" action="/farm/zones" {
                    strong { "Irrigation zone" }
                    label { "Name" input name="name" required; }
                    label { "Serves" select name="location_id" {
                        option value="" { "—" }
                        @for l in &places { option value=(l.id) { (l.name) } }
                    } }
                    button { "Add zone" }
                }
                form.card method="post" action="/farm/compost" {
                    strong { "Compost batch" }
                    div.grid {
                        label { "Name" input name="name" required; }
                        label { "Started" input type="date" name="started_on" required; }
                    }
                    button { "Start batch" }
                }
            }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct EggForm {
    animal_id: String,
    on_date: String,
    collected: String,
    #[serde(default)]
    damaged: String,
}

async fn eggs(c: Can<cap::AnimalsWrite>, Form(f): Form<EggForm>) -> UiResult {
    let n = |s: &str| {
        if s.trim().is_empty() { Ok(0) } else { s.trim().parse::<i32>().map_err(|_| UiError::invalid("count", B)) }
    };
    c.store
        .record_eggs(
            opt_uuid(&f.animal_id, B)?.ok_or_else(|| UiError::invalid("choose a flock", B))?,
            req_date(&f.on_date, "date", B)?,
            n(&f.collected)?,
            n(&f.damaged)?,
            0,
            0,
        )
        .await
        .back(B)?;
    Ok(see("/animals"))
}

#[derive(Deserialize)]
struct CompostForm {
    name: String,
    started_on: String,
}

async fn compost(c: Can<cap::StandWrite>, Form(f): Form<CompostForm>) -> UiResult {
    c.store.add_compost_batch(&f.name, req_date(&f.started_on, "date", B)?, "", None).await.back(B)?;
    Ok(see("/stand"))
}

#[derive(Deserialize)]
struct CheckForm {
    name: String,
    items: String,
}

async fn checklist(c: Can<cap::TasksWrite>, Form(f): Form<CheckForm>) -> UiResult {
    let items: Vec<String> = f.items.lines().map(|l| l.trim().to_owned()).filter(|l| !l.is_empty()).collect();
    let id = c.store.create_checklist(&f.name, &items).await.back("/tasks")?;
    let today = c.store.today().await.back("/tasks")?;
    c.store.start_checklist(id, today, &c.who.actor.display_name).await.back("/tasks")?;
    Ok(see("/tasks"))
}

#[derive(Deserialize)]
struct LossForm {
    lost_on: String,
    reason: String,
    #[serde(default)]
    quantity: String,
    #[serde(default)]
    unit: String,
}

async fn loss(c: Can<cap::GardenWrite>, Form(f): Form<LossForm>) -> UiResult {
    let quantity = match f.quantity.trim() {
        "" => None,
        q => Some(decimal(q, "quantity", B)?),
    };
    c.store.record_loss(req_date(&f.lost_on, "date", B)?, &f.reason, quantity, &f.unit, None, None).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct ObligationForm {
    requirement: String,
    #[serde(default)]
    authority: String,
    #[serde(default)]
    due_on: String,
}

async fn obligation(c: Can<cap::TasksWrite>, Form(f): Form<ObligationForm>) -> UiResult {
    c.store
        .add_obligation(&f.requirement, &f.authority, opt_date(&f.due_on, "due", "/tasks")?, "")
        .await
        .back("/tasks")?;
    Ok(see("/tasks"))
}

async fn obligation_done(c: Can<cap::TasksWrite>, Path(id): Path<Uuid>) -> UiResult {
    let today = c.store.today().await.back("/tasks")?;
    c.store.complete_obligation(id, today).await.back("/tasks")?;
    Ok(see("/tasks"))
}

#[derive(Deserialize)]
struct EmergencyForm {
    scenario: String,
    #[serde(default)]
    procedure: String,
    #[serde(default)]
    contacts: String,
}

async fn emergency(c: Can<cap::TasksWrite>, Form(f): Form<EmergencyForm>) -> UiResult {
    c.store.save_emergency_plan(&f.scenario, &f.procedure, &f.contacts, "").await.back("/tasks")?;
    Ok(see("/tasks"))
}

#[derive(Deserialize)]
struct CostForm {
    purchase_id: String,
    #[serde(default)]
    program: String,
}

async fn cost(c: Can<cap::PurchasesWrite>, Form(f): Form<CostForm>) -> UiResult {
    let id = opt_uuid(&f.purchase_id, "/purchases")?.ok_or_else(|| UiError::invalid("purchase", "/purchases"))?;
    c.store.link_cost(id, &f.program, None, None, None).await.back("/purchases")?;
    Ok(see("/purchases"))
}

#[derive(Deserialize)]
struct ZoneForm {
    name: String,
    #[serde(default)]
    location_id: String,
}

async fn zone(c: Can<cap::GardenWrite>, Form(f): Form<ZoneForm>) -> UiResult {
    let ids = opt_uuid(&f.location_id, "/water")?.into_iter().collect::<Vec<_>>();
    c.store
        .create_zone(homestead_domain::water::NewZone {
            name: f.name,
            location_ids: ids,
            controller_ref: String::new(),
            notes: String::new(),
        })
        .await
        .back("/water")?;
    Ok(see("/water"))
}

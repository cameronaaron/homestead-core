//! Soil tests and amendments: forms on a place's page, a page per test
//! (for the lab's report), and the API.

use axum::extract::Path;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use homestead_domain::attachments::AttachmentOwner;
use homestead_domain::soil::{AmendmentKind, NewAmendment, NewSoilTest, SoilEntry, parse_measurements};
use maud::{Markup, html};
use serde::Deserialize;
use time::Date;
use uuid::Uuid;

use crate::AppState;
use crate::api::{ApiResult, created};
use crate::auth::{Can, Principal, cap};
use crate::ui::{Back, UiError, UiResult, layout, options, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/soil/tests", post(add_test))
        .route("/soil/tests/{id}", get(test_page))
        .route("/soil/amendments", post(add_amendment))
}

pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/soil/tests", post(api_test))
        .route("/soil/amendments", post(api_amendment))
        .route("/locations/{id}/soil", get(api_history))
}

/// A place's soil history and, for people who may, the forms to add to it.
pub fn section(who: &Principal, location_id: Uuid, today: Date, history: &[(SoilEntry, String)], here: &str) -> Markup {
    html! {
        h2 { "Soil" }
        @if history.is_empty() { p.muted { "No soil tests or amendments recorded." } } @else {
            table {
                @for (e, place) in history { tr {
                    td { (e.on()) }
                    @match e {
                        SoilEntry::Test(t) => {
                            td { a href={"/soil/tests/" (t.id)} { "Test" } @if !t.lab.is_empty() { br; span.muted { (t.lab) } } }
                            td { (t.summary()) }
                        }
                        SoilEntry::Amendment(a) => {
                            td { (a.kind) }
                            td { (a.material) @if let Some(q) = a.amount { " · " (q.normalize()) " " (a.unit) }
                                 @if !a.reason.is_empty() { br; span.muted { (a.reason) } } }
                        }
                    }
                    td.muted { @if place != here { (place) } }
                } }
            }
        }
        @if who.can(homestead_domain::auth::Capability::GardenWrite) {
            details.card {
                summary { "Record a soil test" }
                form method="post" action="/soil/tests" style="display:grid;gap:.85rem" {
                    input type="hidden" name="location_id" value=(location_id);
                    div.grid {
                        label { "Tested on" input type="date" name="tested_on" required value=(today); }
                        label { "By (lab or kit)" input name="lab"; }
                        label { "pH" input name="ph" inputmode="decimal"; }
                    }
                    div.grid {
                        label { "Nitrogen" input name="nitrogen" placeholder="as reported"; }
                        label { "Phosphorus" input name="phosphorus"; }
                        label { "Potassium" input name="potassium"; }
                        label { "Organic matter" input name="organic_matter"; }
                    }
                    label { "Other measurements, one per line" textarea name="measurements" rows="3" placeholder="Calcium = 1200 ppm" {} }
                    label { "Notes" input name="notes"; }
                    button { "Save test" }
                }
            }
            details.card {
                summary { "Record an amendment" }
                form method="post" action="/soil/amendments" style="display:grid;gap:.85rem" {
                    input type="hidden" name="location_id" value=(location_id);
                    div.grid {
                        label { "Applied on" input type="date" name="applied_on" required value=(today); }
                        label { "Kind" select name="kind" { (options(AmendmentKind::ALL, Some(AmendmentKind::Compost))) } }
                        label { "Material" input name="material" placeholder="worm castings…"; }
                    }
                    div.grid {
                        label { "Amount" input name="amount" inputmode="decimal"; }
                        label { "Unit" input name="unit" placeholder="lb, cu ft, bag"; }
                        label { "Cost" input name="cost" inputmode="decimal"; }
                    }
                    label { "Why" input name="reason" placeholder="pH was 5.8"; }
                    label { "Notes" input name="notes"; }
                    button { "Save amendment" }
                }
            }
        }
    }
}

fn back(location: &str) -> String {
    format!("/locations/{}", location.trim())
}

fn opt_decimal(s: &str, field: &str, back: &str) -> Result<Option<rust_decimal::Decimal>, UiError> {
    match s.trim().trim_start_matches('$') {
        "" => Ok(None),
        v => v.parse().map(Some).map_err(|_| UiError::invalid_at(format!("{field}: expected a number"), back)),
    }
}

fn date(s: &str, field: &str, back: &str) -> Result<Date, UiError> {
    Date::parse(s.trim(), time::macros::format_description!("[year]-[month]-[day]"))
        .map_err(|_| UiError::invalid_at(format!("{field}: expected a date like 2026-03-01"), back))
}

fn location(s: &str, back: &str) -> Result<Uuid, UiError> {
    s.trim().parse().map_err(|_| UiError::invalid_at("unknown place", back))
}

#[derive(Deserialize)]
struct TestForm {
    location_id: String,
    tested_on: String,
    #[serde(default)]
    lab: String,
    #[serde(default)]
    ph: String,
    #[serde(default)]
    nitrogen: String,
    #[serde(default)]
    phosphorus: String,
    #[serde(default)]
    potassium: String,
    #[serde(default)]
    organic_matter: String,
    #[serde(default)]
    measurements: String,
    #[serde(default)]
    notes: String,
}

async fn add_test(c: Can<cap::GardenWrite>, Form(f): Form<TestForm>) -> UiResult {
    let b = back(&f.location_id);
    let t = c
        .store
        .add_soil_test(NewSoilTest {
            location_id: location(&f.location_id, &b)?,
            tested_on: date(&f.tested_on, "tested on", &b)?,
            ph: opt_decimal(&f.ph, "pH", &b)?,
            nitrogen: f.nitrogen,
            phosphorus: f.phosphorus,
            potassium: f.potassium,
            organic_matter: f.organic_matter,
            lab: f.lab,
            measurements: parse_measurements(&f.measurements).back_to(&b)?,
            notes: f.notes,
        })
        .await
        .back_to(&b)?;
    // Straight to the test's page, where the lab's report can be attached.
    Ok(see(&format!("/soil/tests/{}", t.id)))
}

#[derive(Deserialize)]
struct AmendmentForm {
    location_id: String,
    applied_on: String,
    kind: String,
    #[serde(default)]
    material: String,
    #[serde(default)]
    amount: String,
    #[serde(default)]
    unit: String,
    #[serde(default)]
    cost: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    notes: String,
}

async fn add_amendment(c: Can<cap::GardenWrite>, Form(f): Form<AmendmentForm>) -> UiResult {
    let b = back(&f.location_id);
    c.store
        .add_amendment(NewAmendment {
            location_id: location(&f.location_id, &b)?,
            applied_on: date(&f.applied_on, "applied on", &b)?,
            kind: f.kind.parse::<AmendmentKind>().back_to(&b)?,
            material: f.material,
            amount: opt_decimal(&f.amount, "amount", &b)?,
            unit: f.unit,
            reason: f.reason,
            cost: opt_decimal(&f.cost, "cost", &b)?,
            notes: f.notes,
        })
        .await
        .back_to(&b)?;
    Ok(see(&b))
}

async fn test_page(c: Can<cap::GardenRead>, Path(id): Path<Uuid>) -> UiResult {
    let (t, place) = c.store.get_soil_test(id).await.back("/garden")?;
    let files = c.store.list_attachments(AttachmentOwner::SoilTest, id).await.back("/garden")?;
    Ok(layout(
        &format!("Soil test, {} {}", place, t.tested_on),
        html! {
            p.muted { a href={"/locations/" (t.location_id)} { (place) } @if !t.lab.is_empty() { " · tested by " (t.lab) } }
            table {
                @if let Some(p) = t.ph { tr { th { "pH" } td { (p.normalize()) } } }
                @for (label, v) in [("Nitrogen", &t.nitrogen), ("Phosphorus", &t.phosphorus), ("Potassium", &t.potassium), ("Organic matter", &t.organic_matter)] {
                    @if !v.is_empty() { tr { th { (label) } td { (v) } } }
                }
                @for (k, v) in &t.measurements { tr { th { (k) } td { (v) } } }
            }
            @if !t.notes.is_empty() { p { (t.notes) } }
            h2 { "Lab report" }
            (crate::attachments::panel(&c.who, AttachmentOwner::SoilTest, id, &files, &format!("/soil/tests/{id}")))
        },
    )
    .into_response())
}

async fn api_test(c: Can<cap::GardenWrite>, Json(b): Json<NewSoilTest>) -> ApiResult {
    Ok(created(c.store.add_soil_test(b).await?))
}

async fn api_amendment(c: Can<cap::GardenWrite>, Json(b): Json<NewAmendment>) -> ApiResult {
    Ok(created(c.store.add_amendment(b).await?))
}

async fn api_history(c: Can<cap::GardenRead>, Path(id): Path<Uuid>) -> ApiResult {
    let history: Vec<_> = c
        .store
        .soil_history(id)
        .await?
        .into_iter()
        .map(|(e, place)| {
            let mut v = serde_json::to_value(e).unwrap_or_default();
            v["location_name"] = serde_json::json!(place);
            v
        })
        .collect();
    Ok(Json(history).into_response())
}

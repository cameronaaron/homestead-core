//! Crop health: what was seen, what was done, how it ended, and which crops
//! keep having trouble.

use axum::extract::{Path, Query};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use homestead_domain::attachments::AttachmentOwner;
use homestead_domain::auth::Capability;
use homestead_domain::health::{NewObservation, Observation, ObservationCategory, Resolution, Severity};
use homestead_store::health::ObservationFilter;
use maud::{Markup, html};
use serde::Deserialize;
use time::Duration;
use uuid::Uuid;

use crate::AppState;
use crate::api::{ApiResult, created};
use crate::auth::{Can, cap};
use crate::ui::{Back, UiError, UiResult, layout, opt_date, opt_uuid, options, req_date, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(page).post(record))
        .route("/health/{id}", get(observation_page))
        .route("/health/{id}/resolve", post(resolve))
}

pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/observations", get(api_list).post(api_record))
        .route("/observations/{id}/resolve", post(api_resolve))
}

const B: &str = "/health";

fn severity(s: Severity) -> Markup {
    html! { span class={"pill" @if s == Severity::High { " high" }} { (s) } }
}

/// A table of observations, for this page and a place's page.
pub fn table(rows: &[Observation]) -> Markup {
    html! {
        @if rows.is_empty() { p.muted { "Nothing recorded." } } @else {
            table {
                tr { th { "Seen" } th { "What" } th { "Where" } th { "Status" } }
                @for o in rows { tr {
                    td { a href={"/health/" (o.id)} { (crate::ui::Day(o.observed_on)) } }
                    td { (severity(o.severity)) " " (o.category) ": " (o.observation) }
                    td { @if let Some(c) = &o.crop { (c) " · " } @if let Some(p) = &o.place { (p) } }
                    td { @match o.resolved_on {
                        Some(d) => span.muted { "closed " (crate::ui::Day(d)) },
                        None => @if let Some(f) = o.follow_up_on { "follow up " (f) } @else { "open" },
                    } }
                } }
            }
        }
    }
}

#[derive(Deserialize, Default)]
struct Filter {
    #[serde(default)]
    location_id: String,
    #[serde(default)]
    crop: String,
    #[serde(default)]
    open: Option<String>,
}

async fn page(c: Can<cap::GardenRead>, Query(q): Query<Filter>) -> UiResult {
    let today = c.store.today().await.back(B)?;
    let filter = ObservationFilter {
        location_id: opt_uuid(&q.location_id, B)?,
        crop: Some(q.crop.clone()).filter(|c| !c.trim().is_empty()),
        open_only: q.open.is_some(),
    };
    let rows = c.store.observations(&filter, 100).await.back(B)?;
    let tally = c.store.health_tally(today - Duration::days(365)).await.back(B)?;
    let places = c.store.list_locations().await.back(B)?;
    let plantings = c.store.list_active_plantings().await.back(B)?;
    Ok(layout(
        "Crop health",
        html! {
            form.inline method="get" action="/health" {
                select name="location_id" {
                    option value="" { "Anywhere" }
                    @for l in &places { option value=(l.id) selected[filter.location_id == Some(l.id)] { (l.name) } }
                }
                input name="crop" value=(q.crop) placeholder="Crop";
                label.inline { input type="checkbox" name="open" checked[filter.open_only]; "Open only" }
                button.small { "Show" }
            }
            (table(&rows))
            h2 { "The last twelve months" }
            @if tally.is_empty() { p.muted { "No observations in the last year." } } @else {
                table { tr { th { "Crop or place" } th { "Trouble" } th { "Times seen" } th { "Last" } }
                    @for t in &tally { tr {
                        td { (t.subject) } td { (t.category) }
                        td { (t.seen) @if t.high > 0 { span.warn { " (" (t.high) " high)" } } }
                        td { (t.last_seen) }
                    } }
                }
            }
            @if c.who.can(Capability::GardenWrite) {
                form.card method="post" action="/health" {
                    strong { "Record what you saw" }
                    div.grid {
                        label { "Seen on" input type="date" name="observed_on" required value=(today); }
                        label { "Planting" select name="planting_id" {
                            option value="" { "—" }
                            @for (p, place) in &plantings { option value=(p.id) { (p.crop) " — " (place) } }
                        } }
                        label { "Or place" select name="location_id" {
                            option value="" { "—" }
                            @for l in &places { option value=(l.id) { (l.name) } }
                        } }
                    }
                    div.grid {
                        label { "Kind" select name="category" { (options(ObservationCategory::ALL, Some(ObservationCategory::Pest))) } }
                        label { "Severity" select name="severity" { (options(Severity::ALL, Some(Severity::Low))) } }
                        label { "How much" input name="affected" placeholder="6 plants, half the bed"; }
                    }
                    label { "What you saw" input name="observation" required placeholder="Aphids under kale leaves"; }
                    label { "What was done" input name="action_taken"; }
                    label { "Follow up on" input type="date" name="follow_up_on"; }
                    button { "Record" }
                }
            }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct ObsForm {
    observed_on: String,
    #[serde(default)]
    planting_id: String,
    #[serde(default)]
    location_id: String,
    category: String,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    affected: String,
    observation: String,
    #[serde(default)]
    action_taken: String,
    #[serde(default)]
    follow_up_on: String,
}

async fn record(c: Can<cap::GardenWrite>, Form(f): Form<ObsForm>) -> UiResult {
    let o = c
        .store
        .record_observation(NewObservation {
            observed_on: req_date(&f.observed_on, "seen on", B)?,
            location_id: opt_uuid(&f.location_id, B)?,
            planting_id: opt_uuid(&f.planting_id, B)?,
            category: f.category.parse::<ObservationCategory>().back(B)?,
            observation: f.observation,
            severity: match f.severity.trim() {
                "" => Severity::Low,
                s => s.parse::<Severity>().back(B)?,
            },
            affected: f.affected,
            action_taken: f.action_taken,
            follow_up_on: opt_date(&f.follow_up_on, "follow up on", B)?,
        })
        .await
        .back(B)?;
    // To its page, where photos can be attached.
    Ok(see(&format!("/health/{}", o.id)))
}

async fn observation_page(c: Can<cap::GardenRead>, Path(id): Path<Uuid>) -> UiResult {
    let o = c.store.get_observation(id).await.back(B)?;
    let files = c.store.list_attachments(AttachmentOwner::CropObservation, id).await.back(B)?;
    let today = c.store.today().await.back(B)?;
    let here = format!("/health/{id}");
    Ok(layout(
        &format!("{} {}", o.category, o.observed_on),
        html! {
            p { (severity(o.severity)) " " (o.observation) }
            table {
                @if let Some(p) = o.planting_id { tr { th { "Planting" } td { a href={"/plantings/" (p)} { (o.crop.as_deref().unwrap_or("planting")) } } } }
                @if let (Some(l), Some(name)) = (o.location_id, &o.place) { tr { th { "Where" } td { a href={"/locations/" (l)} { (name) } } } }
                @if !o.affected.is_empty() { tr { th { "Affected" } td { (o.affected) } } }
                @if !o.action_taken.is_empty() { tr { th { "Done" } td { (o.action_taken) } } }
                @if let Some(f) = o.follow_up_on { tr { th { "Follow up" } td { (f) } } }
                @if let Some(r) = o.resolved_on { tr { th { "Closed" } td { (r) ": " (o.outcome) } } }
            }
            h2 { "Photos" }
            (crate::attachments::panel(&c.who, AttachmentOwner::CropObservation, id, &files, &here))
            @if o.resolved_on.is_none() && c.who.can(Capability::GardenWrite) {
                form.card method="post" action={(here) "/resolve"} {
                    strong { "Close it" }
                    label { "On" input type="date" name="resolved_on" required value=(today); }
                    label { "How it ended" input name="outcome" required placeholder="Gone after two sprays"; }
                    button { "Close" }
                }
            }
            p { a href="/health" { "← Crop health" } }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct ResolveForm {
    resolved_on: String,
    outcome: String,
}

async fn resolve(c: Can<cap::GardenWrite>, Path(id): Path<Uuid>, Form(f): Form<ResolveForm>) -> UiResult {
    let here = format!("/health/{id}");
    let on = crate::ui::opt_date(&f.resolved_on, "on", B)?.ok_or_else(|| UiError::invalid_at("on: required", &here))?;
    c.store.resolve_observation(id, Resolution { resolved_on: on, outcome: f.outcome }).await.back_to(&here)?;
    Ok(see(&here))
}

async fn api_list(c: Can<cap::GardenRead>) -> ApiResult {
    Ok(Json(c.store.observations(&ObservationFilter::default(), 200).await?).into_response())
}

async fn api_record(c: Can<cap::GardenWrite>, Json(b): Json<NewObservation>) -> ApiResult {
    Ok(created(c.store.record_observation(b).await?))
}

async fn api_resolve(c: Can<cap::GardenWrite>, Path(id): Path<Uuid>, Json(b): Json<Resolution>) -> ApiResult {
    Ok(Json(c.store.resolve_observation(id, b).await?).into_response())
}

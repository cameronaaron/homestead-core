//! Crop plans: what to sow, transplant and harvest now, each plan's
//! successions, and how their yields compared with the plan.

use axum::extract::Path;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Form, Json, Router};
use homestead_domain::HarvestUnit;
use homestead_domain::SowMethod;
use homestead_domain::auth::Capability;
use homestead_domain::crops::{Due, NewCropPlan, Succession, YieldComparison, succession_dates};
use maud::{Markup, html};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::api::{ApiResult, created};
use crate::auth::{Can, cap};
use crate::ui::{Back, UiError, UiResult, decimal, layout, options, req_date, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new().route("/crops", get(page).post(create)).route("/crops/{id}", get(plan_page))
}

pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/crop-plans", get(api_list).post(api_create))
        .route("/crop-plans/{id}", get(api_get))
        .route("/crops/agenda", get(api_agenda))
}

const B: &str = "/crops";

fn due_label(d: Due) -> Markup {
    match d {
        Due::SowOverdue => html! { span.pill.urgent { "sowing overdue" } },
        Due::SowThisWeek => html! { span.pill { "sow this week" } },
        Due::HarvestOverdue => html! { span.pill.urgent { "harvest overdue" } },
        Due::HarvestSoon => html! { span.pill { "harvest soon" } },
    }
}

/// "7 of 8 lb (88%)", with harvests in other units listed beside it.
pub(crate) fn yield_cell(y: &YieldComparison) -> Markup {
    html! {
        @match (y.actual, y.expected) {
            (Some((a, u)), Some((e, _))) => {
                (a.normalize()) " of " (e.normalize()) " " (u)
                @if let Some(p) = y.percent() { span.muted { " (" (p) "%)" } }
            }
            (Some((a, u)), None) => { (a.normalize()) " " (u) }
            (None, Some((e, u))) => span.muted { "expected " (e.normalize()) " " (u) },
            (None, None) => {}
        }
        @for (q, u) in &y.other { br; span.muted { "+ " (q.normalize()) " " (u) } }
    }
}

async fn page(c: Can<cap::GardenRead>) -> UiResult {
    let agenda = c.store.crop_agenda().await.back(B)?;
    let plans = c.store.list_crop_plans().await.back(B)?;
    let places = c.store.list_locations().await.back(B)?;
    let lots = c.store.list_seed_lots().await.back(B)?;
    let write = c.who.can(Capability::GardenWrite);
    let can_tick = c.who.can(Capability::TasksWrite);
    let (sow, harvest): (Vec<_>, Vec<_>) =
        agenda.plantings.iter().partition(|(_, _, d)| matches!(d, Due::SowOverdue | Due::SowThisWeek));
    Ok(layout(
        "Crops",
        html! {
            p.muted { "As of " (agenda.today) ". Ticking a sowing or transplant task records it on the planting." }
            h2 { "Sow" }
            @if sow.is_empty() { p.muted { "Nothing to sow this week." } } @else {
                table { tr { th { "Crop" } th { "Where" } th { "Planned for" } th {} }
                    @for (p, place, due) in &sow { tr {
                        td { (p.crop) @if !p.variety.is_empty() { " " span.muted { (p.variety) } } }
                        td { a href={"/locations/" (p.location_id)} { (place) } }
                        td { (crate::ui::Day(p.sown_on)) } td { (due_label(*due)) }
                    } }
                }
            }
            @if !agenda.transplants.is_empty() {
                h2 { "Transplant" }
                table { tr { th { "Crop" } th { "Into" } th { "Due" } th {} }
                    @for (task, due, p, place) in &agenda.transplants { tr {
                        td { (p.crop) } td { (place) }
                        td class=[(*due < agenda.today).then_some("warn")] { (due) }
                        td { @if can_tick { form.inline method="post" action={"/tasks/" (task) "/complete"} { button.small { "Done" } } } }
                    } }
                }
            }
            h2 { "Harvest" }
            @if harvest.is_empty() { p.muted { "Nothing expected in the next two weeks." } } @else {
                table { tr { th { "Crop" } th { "Where" } th { "Expected" } th {} }
                    @for (p, place, due) in &harvest { tr {
                        td { (p.crop) } td { a href={"/locations/" (p.location_id)} { (place) } }
                        td { @if let Some(f) = p.expected_harvest_on { (f) } @if let Some(u) = p.expected_harvest_until { " – " (u) } }
                        td { (due_label(*due)) }
                    } }
                }
            }
            h2 { "Plans" }
            @if plans.is_empty() { p.muted { "No crop plans yet." } } @else {
                table { tr { th { "Crop" } th { "First sowing" } th { "Successions" } }
                    @for pl in &plans { tr {
                        td { a href={"/crops/" (pl.id)} { (pl.crop) } @if !pl.variety.is_empty() { " " span.muted { (pl.variety) } } }
                        td { @if let Some(d) = pl.first_sow_on { (crate::ui::Day(d)) } }
                        td { (pl.successions) span.muted { " · " (pl.planned) " planned, " (pl.in_ground) " in the ground, " (pl.over) " done" } }
                    } }
                }
            }
            @if write {
                form.card method="post" action="/crops" {
                    strong { "Plan a crop" }
                    div.grid {
                        label { "Crop" input name="crop" required placeholder="Lettuce"; }
                        label { "Variety" input name="variety"; }
                        label { "Family" input name="family" placeholder="aster, brassica…"; }
                    }
                    div.grid {
                        label { "Where" select name="location_id" required {
                            @for l in places.iter().filter(|l| l.active) { option value=(l.id) { (l.name) } }
                        } }
                        label { "First sowing" input type="date" name="first_sow_on" required; }
                        label { "Sow again every (days)" input name="every_days" inputmode="numeric" value="14"; }
                        label { "How many sowings" input name="count" inputmode="numeric" value="1"; }
                    }
                    div.grid {
                        label { "Method" select name="method" { (options(SowMethod::ALL, Some(SowMethod::Direct))) } }
                        label { "Days to transplant" input name="days_to_transplant" inputmode="numeric" placeholder="transplants only"; }
                        label { "Days to first harvest" input name="days_to_harvest" inputmode="numeric"; }
                        label { "Harvest window (days)" input name="harvest_window_days" inputmode="numeric"; }
                    }
                    div.grid {
                        label { "Seed lot" select name="seed_lot_id" {
                            option value="" { "—" }
                            @for l in &lots { option value=(l.id) { (l.label()) } }
                        } }
                        label { "Expected yield per sowing" input name="expected_yield" inputmode="decimal"; }
                        label { "Unit" select name="expected_yield_unit" {
                            option value="" { "—" } (options(HarvestUnit::ALL, None))
                        } }
                    }
                    label { "Notes" input name="notes"; }
                    button { "Plan" }
                }
            }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct PlanForm {
    crop: String,
    #[serde(default)]
    variety: String,
    #[serde(default)]
    family: String,
    #[serde(default)]
    notes: String,
    location_id: String,
    first_sow_on: String,
    #[serde(default)]
    every_days: String,
    #[serde(default)]
    count: String,
    method: String,
    #[serde(default)]
    days_to_transplant: String,
    #[serde(default)]
    days_to_harvest: String,
    #[serde(default)]
    harvest_window_days: String,
    #[serde(default)]
    seed_lot_id: String,
    #[serde(default)]
    expected_yield: String,
    #[serde(default)]
    expected_yield_unit: String,
}

fn opt_num<T: std::str::FromStr>(s: &str, field: &str) -> Result<Option<T>, UiError> {
    match s.trim() {
        "" => Ok(None),
        n => n.parse().map(Some).map_err(|_| UiError::invalid(format!("{field}: expected a whole number"), B)),
    }
}

async fn create(c: Can<cap::GardenWrite>, Form(f): Form<PlanForm>) -> UiResult {
    let location_id = crate::ui::opt_uuid(&f.location_id, B)?.ok_or_else(|| UiError::invalid("choose a place", B))?;
    let first = req_date(&f.first_sow_on, "first sowing", B)?;
    let every: u16 = opt_num(&f.every_days, "every")?.unwrap_or(14);
    let count: u8 = opt_num(&f.count, "how many")?.unwrap_or(1);
    if count > 52 {
        return Err(UiError::invalid("at most 52 sowings in one plan", B));
    }
    if count > 1 && every == 0 {
        return Err(UiError::invalid("sow again every: at least one day", B));
    }
    let input = NewCropPlan {
        crop: f.crop,
        variety: f.variety,
        family: f.family,
        notes: f.notes,
        method: f.method.parse::<SowMethod>().back(B)?,
        days_to_transplant: opt_num(&f.days_to_transplant, "days to transplant")?,
        days_to_harvest: opt_num(&f.days_to_harvest, "days to harvest")?,
        harvest_window_days: opt_num(&f.harvest_window_days, "harvest window")?,
        seed_lot_id: crate::ui::opt_uuid(&f.seed_lot_id, B)?,
        expected_yield: match f.expected_yield.trim() {
            "" => None,
            q => Some(decimal(q, "expected yield", B)?),
        },
        expected_yield_unit: match f.expected_yield_unit.trim() {
            "" => None,
            u => Some(u.parse::<HarvestUnit>().back(B)?),
        },
        successions: succession_dates(first, every, count)
            .into_iter()
            .map(|sow_on| Succession { location_id, sow_on })
            .collect(),
    };
    let plan = c.store.create_crop_plan(input).await.back(B)?;
    Ok(see(&format!("/crops/{}", plan.id)))
}

async fn plan_page(c: Can<cap::GardenRead>, Path(id): Path<Uuid>) -> UiResult {
    let (plan, successions) = c.store.get_crop_plan(id).await.back(B)?;
    let lot = match plan.seed_lot_id {
        Some(l) => Some(c.store.get_seed_lot(l).await.back(B)?),
        None => None,
    };
    let title = if plan.variety.is_empty() { plan.crop.clone() } else { format!("{} '{}'", plan.crop, plan.variety) };
    Ok(layout(
        &title,
        html! {
            p.muted {
                (plan.method)
                @if !plan.family.is_empty() { " · " (plan.family) }
                @if let Some(d) = plan.days_to_transplant { " · transplant after " (d) " days" }
                @if let Some(d) = plan.days_to_harvest { " · harvest from day " (d) }
                @if let Some(d) = plan.harvest_window_days { " for " (d) " days" }
                @if let (Some(q), Some(u)) = (plan.expected_yield, plan.expected_yield_unit) { " · expecting " (q.normalize()) " " (u) " a sowing" }
            }
            @if let Some(l) = &lot { p { "Seed: " a href={"/seeds/" (l.id)} { (l.label()) } } }
            @if !plan.notes.is_empty() { p { (plan.notes) } }
            h2 { "Successions" }
            table { tr { th { "Where" } th { "Sow" } th { "Transplant" } th { "Harvest" } th { "Status" } th { "Yield" } }
                @for s in &successions { tr {
                    td { a href={"/locations/" (s.planting.location_id)} { (s.place) } }
                    td { (crate::ui::Day(s.planting.sown_on)) }
                    td { @if let Some(t) = s.planting.transplant_on { (t) } }
                    td { @if let Some(f) = s.planting.expected_harvest_on { (f) } @if let Some(u) = s.planting.expected_harvest_until { " – " (u) } }
                    td { (s.planting.status) }
                    td { (yield_cell(&s.yields)) }
                } }
            }
            p { a href="/crops" { "← All crops" } }
        },
    )
    .into_response())
}

async fn api_create(c: Can<cap::GardenWrite>, Json(b): Json<NewCropPlan>) -> ApiResult {
    Ok(created(c.store.create_crop_plan(b).await?))
}

async fn api_list(c: Can<cap::GardenRead>) -> ApiResult {
    let plans = c.store.list_crop_plans().await?;
    let out: Vec<_> = plans
        .into_iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id, "crop": p.crop, "variety": p.variety, "first_sow_on": p.first_sow_on,
                "successions": p.successions, "planned": p.planned, "in_ground": p.in_ground, "over": p.over,
            })
        })
        .collect();
    Ok(Json(out).into_response())
}

async fn api_get(c: Can<cap::GardenRead>, Path(id): Path<Uuid>) -> ApiResult {
    let (plan, successions) = c.store.get_crop_plan(id).await?;
    let successions: Vec<_> = successions
        .into_iter()
        .map(|s| {
            let mut v = serde_json::to_value(&s.planting).unwrap_or_default();
            v["location_name"] = serde_json::json!(s.place);
            v["harvested"] = serde_json::json!(
                s.yields
                    .actual
                    .into_iter()
                    .chain(s.yields.other)
                    .map(|(q, u)| serde_json::json!({"quantity": q, "unit": u}))
                    .collect::<Vec<_>>()
            );
            v
        })
        .collect();
    Ok(Json(serde_json::json!({ "plan": plan, "successions": successions })).into_response())
}

async fn api_agenda(c: Can<cap::GardenRead>) -> ApiResult {
    let a = c.store.crop_agenda().await?;
    let due = |d: Due| match d {
        Due::SowOverdue => "sow_overdue",
        Due::SowThisWeek => "sow_this_week",
        Due::HarvestOverdue => "harvest_overdue",
        Due::HarvestSoon => "harvest_soon",
    };
    let plantings: Vec<_> = a
        .plantings
        .into_iter()
        .map(|(p, place, d)| {
            let mut v = serde_json::to_value(&p).unwrap_or_default();
            v["location_name"] = serde_json::json!(place);
            v["due"] = serde_json::json!(due(d));
            v
        })
        .collect();
    let transplants: Vec<_> = a
        .transplants
        .into_iter()
        .map(|(task, due_on, p, place)| {
            serde_json::json!({"task_id": task, "due_on": due_on, "planting_id": p.id, "crop": p.crop, "location_name": place})
        })
        .collect();
    Ok(Json(serde_json::json!({ "today": a.today, "plantings": plantings, "transplants": transplants }))
        .into_response())
}

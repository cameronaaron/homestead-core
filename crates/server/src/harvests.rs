//! A harvest's trace (seed → planting → place → harvest → where it went),
//! a planting's page, and produce donations with their export.

use axum::extract::{Path, Query};
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use homestead_domain::HarvestUnit;
use homestead_domain::auth::Capability;
use homestead_domain::harvests::{DispositionKind, NewDisposition, NewProduceDonation, shares};
use homestead_domain::reports::FiscalYear;
use maud::html;
use serde::Deserialize;
use time::Date;
use uuid::Uuid;

use crate::AppState;
use crate::api::{ApiResult, created};
use crate::auth::{Can, cap};
use crate::contacts::csv_cell;
use crate::ui::{Back, UiError, UiResult, decimal, layout, opt_date, opt_money, options, req_date, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/harvests/{id}", get(harvest_page))
        .route("/harvests/{id}/dispositions", post(split))
        .route("/dispositions/{id}/remove", post(remove))
        .route("/plantings/{id}", get(planting_page))
        .route("/donations", get(donations_page).post(donate))
        .route("/donations/export.csv", get(export))
}

pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/harvests/{id}/dispositions", post(api_split))
        .route("/donations", get(api_donations).post(api_donate))
}

async fn harvest_page(c: Can<cap::GardenRead>, Path(id): Path<Uuid>) -> UiResult {
    let back = format!("/harvests/{id}");
    let t = c.store.get_harvest(id).await.back("/garden")?;
    let h = &t.harvest;
    let parts: Vec<_> = t.dispositions.iter().map(|d| (d.kind, d.quantity)).collect();
    let s = shares(h.quantity, &parts);
    let write = c.who.can(Capability::GardenWrite);
    let today = c.store.today().await.back_to(&back)?;
    Ok(layout(
        &format!("{} {} of {}", h.quantity.normalize(), h.unit, t.planting.crop),
        html! {
            p.muted { "Harvested " (crate::ui::Day(h.harvested_on)) @if !h.notes.is_empty() { " · " (h.notes) } }
            h2 { "Where it came from" }
            ul {
                @if let Some(lot) = &t.seed_lot {
                    li { "Seed: " a href={"/seeds/" (lot.id)} { (lot.label()) }
                        @if let Some(v) = &lot.vendor_name { " — bought from " (v) } }
                }
                @if let Some(plan) = &t.plan { li { "Plan: " a href={"/crops/" (plan.id)} { (plan.crop) } } }
                li { "Planting: " a href={"/plantings/" (t.planting.id)} { (t.planting.crop) " sown " (crate::ui::Day(t.planting.sown_on)) } }
                li { "Grown in " a href={"/locations/" (t.planting.location_id)} { (t.place) }
                    span.muted { " (its soil, watering and history are on the place's page)" } }
            }
            h2 { "Where it went" }
            @if t.dispositions.is_empty() { p.muted { "Not recorded yet." } } @else {
                table {
                    @for d in &t.dispositions { tr {
                        td { (d.kind.label()) } td { (d.quantity.normalize()) " " (d.unit) }
                        td {
                            @if let Some((_, to)) = &d.donation { "to " (to) " " }
                            @if d.stand_sale_id.is_some() { span.muted { "farm-stand sale " } }
                            span.muted { (d.notes) }
                        }
                        td { @if write {
                            form.inline method="post" action={"/dispositions/" (d.id) "/remove"} { button.small { "Remove" } }
                        } }
                    } }
                }
                p {
                    @for (fate, q, pct) in &s.parts { (fate.label()) " " (q.normalize()) " " (h.unit) " (" (pct.normalize()) "%) · " }
                    @if !s.unrecorded.is_zero() { span.muted { (s.unrecorded.normalize()) " " (h.unit) " not yet recorded" } }
                }
            }
            @if write && !s.unrecorded.is_zero() {
                form.card method="post" action={"/harvests/" (h.id) "/dispositions"} {
                    strong { "Record where some went" }
                    p.muted { "In " (h.unit) ", the harvest's unit; at most " (s.unrecorded.normalize()) " more. A sale at the stand can instead be recorded on the stand page against this harvest." }
                    div.grid {
                        label { "Went to" select name="kind" { (options(DispositionKind::ALL, Some(DispositionKind::Sale))) } }
                        label { "Quantity (" (h.unit) ")" input name="quantity" inputmode="decimal" required; }
                    }
                    div.grid {
                        label { "Recipient (donations)" input name="recipient" placeholder="Food pantry…"; }
                        label { "Donated on" input type="date" name="donated_on" value=(today); }
                        label { "Estimated value (donations)" input name="operational_value" inputmode="decimal" placeholder="operational estimate"; }
                    }
                    label { "Notes" input name="notes"; }
                    button { "Record" }
                }
            }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct SplitForm {
    kind: String,
    quantity: String,
    #[serde(default)]
    recipient: String,
    #[serde(default)]
    donated_on: String,
    #[serde(default)]
    operational_value: String,
    #[serde(default)]
    notes: String,
}

async fn split(c: Can<cap::GardenWrite>, Path(id): Path<Uuid>, Form(f): Form<SplitForm>) -> UiResult {
    let back = format!("/harvests/{id}");
    let kind = f.kind.parse::<DispositionKind>().back_to(&back)?;
    let donation = kind == DispositionKind::Donation;
    let input = NewDisposition {
        kind,
        quantity: f.quantity.trim().parse().map_err(|_| UiError::invalid_at("quantity: expected a number", &back))?,
        notes: f.notes,
        recipient: if donation { f.recipient } else { String::new() },
        operational_value: if donation {
            match f.operational_value.trim().trim_start_matches('$') {
                "" => None,
                v => Some(v.parse().map_err(|_| UiError::invalid_at("value: expected a number", &back))?),
            }
        } else {
            None
        },
        donated_on: if donation {
            match f.donated_on.trim() {
                "" => None,
                d => Some(
                    Date::parse(d, time::macros::format_description!("[year]-[month]-[day]"))
                        .map_err(|_| UiError::invalid_at("donated on: expected a date", &back))?,
                ),
            }
        } else {
            None
        },
    };
    c.store.split_harvest(id, input).await.back_to(&back)?;
    Ok(see(&back))
}

async fn remove(c: Can<cap::GardenWrite>, Path(id): Path<Uuid>) -> UiResult {
    let harvest = c.store.remove_disposition(id).await.back("/garden")?;
    Ok(see(&format!("/harvests/{harvest}")))
}

async fn planting_page(c: Can<cap::GardenRead>, Path(id): Path<Uuid>) -> UiResult {
    const B: &str = "/garden";
    let (p, place) = c.store.get_planting(id).await.back(B)?;
    let harvests = c.store.harvests_of_planting(id).await.back(B)?;
    let lot = match p.seed_lot_id {
        Some(l) => Some(c.store.get_seed_lot(l).await.back(B)?),
        None => None,
    };
    let title = if p.variety.is_empty() { p.crop.clone() } else { format!("{} '{}'", p.crop, p.variety) };
    Ok(layout(
        &title,
        html! {
            p.muted {
                (p.status) " · " a href={"/locations/" (p.location_id)} { (place) }
                @if !p.family.is_empty() { " · " (p.family) } " · " (p.method)
            }
            table {
                tr { th { "Sown" } td { (crate::ui::Day(p.sown_on)) } }
                @if let Some(t) = p.transplant_on { tr { th { "Transplanted" } td { (t) } } }
                @if let Some(f) = p.expected_harvest_on { tr { th { "Expected harvest" } td { (f) @if let Some(u) = p.expected_harvest_until { " – " (u) } } } }
                @if let Some(e) = p.ended_on { tr { th { "Ended" } td { (e) } } }
                @if let Some(l) = &lot { tr { th { "Seed" } td { a href={"/seeds/" (l.id)} { (l.label()) } } } }
                @if let Some(plan) = p.crop_plan_id { tr { th { "Plan" } td { a href={"/crops/" (plan)} { "crop plan" } } } }
            }
            @if !p.notes.is_empty() { p { (p.notes) } }
            h2 { "Harvests" }
            @if harvests.is_empty() { p.muted { "None yet." } } @else {
                table { @for hv in &harvests { tr {
                    td { a href={"/harvests/" (hv.id)} { (crate::ui::Day(hv.harvested_on)) } } td { (hv.quantity.normalize()) " " (hv.unit) }
                } } }
            }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct RangeQuery {
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
}

/// `[from, to]` from the query, defaulting to this fiscal year so far.
async fn range(c: &Can<cap::GardenRead>, q: &RangeQuery) -> Result<(Date, Date), UiError> {
    const B: &str = "/donations";
    let today = c.store.today().await.back(B)?;
    let from = opt_date(&q.from, "from", B)?.unwrap_or_else(|| FiscalYear::containing(today).first_day());
    let to = opt_date(&q.to, "to", B)?.unwrap_or(today);
    if to < from {
        return Err(UiError::invalid("to: before from", B));
    }
    Ok((from, to))
}

async fn donations_page(c: Can<cap::GardenRead>, Query(q): Query<RangeQuery>) -> UiResult {
    const B: &str = "/donations";
    let (from, to) = range(&c, &q).await?;
    let rows = c.store.donations_between(from, to).await.back(B)?;
    let mut totals: Vec<(HarvestUnit, rust_decimal::Decimal)> = Vec::new();
    for r in &rows {
        match totals.iter_mut().find(|(u, _)| *u == r.unit) {
            Some((_, q)) => *q += r.quantity,
            None => totals.push((r.unit, r.quantity)),
        }
    }
    let valued: rust_decimal::Decimal = rows.iter().filter_map(|r| r.operational_value).sum();
    let unvalued = rows.iter().filter(|r| r.operational_value.is_none()).count();
    Ok(layout(
        "Produce donations",
        html! {
            form.inline method="get" action="/donations" {
                "From " input type="date" name="from" value=(from); " to " input type="date" name="to" value=(to);
                button.small { "Show" }
                a href={"/donations/export.csv?from=" (from) "&to=" (to)} { "CSV" }
            }
            p {
                @if totals.is_empty() { span.muted { "Nothing donated in this period." } }
                @for (i, (u, q)) in totals.iter().enumerate() { @if i > 0 { " · " } strong { (q.normalize()) " " (u) } }
            }
            @if !valued.is_zero() {
                p.muted { "Estimated value $" (valued.round_dp(2)) " across " (rows.len() - unvalued) " donations"
                    @if unvalued > 0 { " (" (unvalued) " not valued)" }
                    ". An operational estimate, not a tax or accounting value." }
            }
            @if !rows.is_empty() {
                table { tr { th { "Date" } th { "To" } th { "What" } th { "Est. value" } }
                    @for r in &rows { tr {
                        td { (crate::ui::Day(r.donated_on)) } td { (r.recipient) }
                        td { (r.quantity.normalize()) " " (r.unit) " " (r.product)
                             @if let Some(h) = r.harvest_id { " " a.muted href={"/harvests/" (h)} { "harvest" } } }
                        td { @if let Some(v) = r.operational_value { "$" (v.round_dp(2)) } }
                    } }
                }
            }
            @if c.who.can(Capability::GardenWrite) {
                form.card method="post" action="/donations" {
                    strong { "Record a donation" }
                    p.muted { "For produce split from a logged harvest, record it on the harvest's page instead, so it's traced." }
                    div.grid {
                        label { "Date" input type="date" name="donated_on" required value=(to); }
                        label { "Recipient" input name="recipient" required; }
                        label { "Product" input name="product" required placeholder="Eggs"; }
                        label { "Quantity" input name="quantity" inputmode="decimal" required; }
                        label { "Unit" select name="unit" { (options(HarvestUnit::ALL, Some(HarvestUnit::Lb))) } }
                        label { "Estimated value" input name="operational_value" inputmode="decimal"; }
                    }
                    label { "Notes" input name="notes"; }
                    button { "Record donation" }
                }
            }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct DonationForm {
    donated_on: String,
    recipient: String,
    product: String,
    quantity: String,
    unit: String,
    #[serde(default)]
    operational_value: String,
    #[serde(default)]
    notes: String,
}

async fn donate(c: Can<cap::GardenWrite>, Form(f): Form<DonationForm>) -> UiResult {
    const B: &str = "/donations";
    c.store
        .donate_produce(NewProduceDonation {
            donated_on: req_date(&f.donated_on, "date", B)?,
            recipient: f.recipient,
            product: f.product,
            quantity: decimal(&f.quantity, "quantity", B)?,
            unit: f.unit.parse::<HarvestUnit>().back(B)?,
            operational_value: opt_money(&f.operational_value, "value", B)?,
            notes: f.notes,
        })
        .await
        .back(B)?;
    Ok(see(B))
}

async fn export(c: Can<cap::GardenRead>, Query(q): Query<RangeQuery>) -> UiResult {
    const B: &str = "/donations";
    let (from, to) = range(&c, &q).await?;
    let rows = c.store.donations_between(from, to).await.back(B)?;
    let mut w = csv::Writer::from_writer(vec![]);
    let bad = |e: csv::Error| UiError::invalid(format!("CSV: {e}"), B);
    w.write_record(["date", "recipient", "product", "quantity", "unit", "estimated_value", "notes", "harvest_id"])
        .map_err(bad)?;
    for r in &rows {
        w.write_record([
            r.donated_on.to_string(),
            csv_cell(&r.recipient),
            csv_cell(&r.product),
            r.quantity.normalize().to_string(),
            r.unit.to_string(),
            r.operational_value.map(|v| v.round_dp(2).to_string()).unwrap_or_default(),
            csv_cell(&r.notes),
            r.harvest_id.map(|h| h.to_string()).unwrap_or_default(),
        ])
        .map_err(bad)?;
    }
    let bytes = w.into_inner().map_err(|e| UiError::invalid(format!("CSV: {e}"), B))?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"donations-{from}-to-{to}.csv\"")),
        ],
        bytes,
    )
        .into_response())
}

async fn api_split(c: Can<cap::GardenWrite>, Path(id): Path<Uuid>, Json(b): Json<NewDisposition>) -> ApiResult {
    Ok(created(c.store.split_harvest(id, b).await?))
}

#[derive(Deserialize)]
struct ApiRange {
    from: Date,
    to: Date,
}

async fn api_donations(c: Can<cap::GardenRead>, Query(q): Query<ApiRange>) -> ApiResult {
    Ok(Json(c.store.donations_between(q.from, q.to).await?).into_response())
}

async fn api_donate(c: Can<cap::GardenWrite>, Json(b): Json<NewProduceDonation>) -> ApiResult {
    Ok(created(c.store.donate_produce(b).await?))
}

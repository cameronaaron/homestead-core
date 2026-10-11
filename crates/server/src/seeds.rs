//! Seed lots: the Garden page's list, a page per lot, and the API.

use axum::extract::Path;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use homestead_domain::attachments::AttachmentOwner;
use homestead_domain::auth::Capability;
use homestead_domain::seeds::{NewSeedLot, SeedLot, SeedUnit};
use maud::{Markup, html};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::api::{ApiResult, created};
use crate::auth::{Can, Principal, cap};
use crate::ui::{Back, UiError, UiResult, decimal, layout, opt_date, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new().route("/seeds", post(create)).route("/seeds/{id}", get(page))
}

pub fn api_routes() -> Router<AppState> {
    Router::new().route("/seed-lots", get(api_list).post(api_create)).route("/seed-lots/{id}", get(api_get))
}

const B: &str = "/garden";

fn amount(l: &SeedLot) -> Markup {
    html! {
        @match (l.quantity, l.unit, l.remaining()) {
            (Some(q), Some(u), Some(left)) => { (left.normalize()) " of " (q.normalize()) " " (u) " left" }
            _ => span.muted { "not counted" },
        }
    }
}

/// The seed lots list and the form to add one, for the Garden page.
pub fn section(who: &Principal, lots: &[SeedLot]) -> Markup {
    html! {
        h2 { "Seed lots" }
        @if lots.is_empty() { p.muted { "No seed lots recorded yet." } } @else {
            table {
                tr { th { "Seed" } th { "From" } th { "Got" } th { "Left" } th { "Sown" } }
                @for l in lots {
                    tr {
                        td { a href={"/seeds/" (l.id)} { (l.crop) @if !l.variety.is_empty() { " '" (l.variety) "'" } } }
                        td { (l.vendor_name.as_deref().unwrap_or(&l.supplier)) @if !l.supplier_lot.is_empty() { span.muted { " lot " (l.supplier_lot) } } }
                        td.muted { @if let Some(d) = l.acquired_on { (crate::ui::Day(d)) } }
                        td { (amount(l)) }
                        td.muted { (l.plantings) " planting" @if l.plantings != 1 { "s" } }
                    }
                }
            }
        }
        @if who.can(Capability::GardenWrite) {
            form.card method="post" action="/seeds" {
                strong { "Add a seed lot" }
                div.grid {
                    label { "Crop" input name="crop" required placeholder="Tomato"; }
                    label { "Variety" input name="variety" placeholder="Sungold"; }
                    label { "From" input name="supplier" placeholder="Baker Creek, a member's saved seed…"; }
                    label { "Their lot number" input name="supplier_lot"; }
                    label { "Got it on" input name="acquired_on" type="date"; }
                    label { "Packed for (year)" input name="packed_for_year" type="number" min="1900" max="3000"; }
                    label { "How much (optional)" input name="quantity" inputmode="decimal"; }
                    label { "Unit" select name="unit" { option value="" { "—" } @for u in SeedUnit::ALL { option value=(u) { (u) } } } }
                }
                label.inline { input type="checkbox" name="organic"; "Certified organic" }
                label { "Notes" input name="notes"; }
                button { "Add lot" }
            }
        }
    }
}

#[derive(Deserialize)]
struct LotForm {
    crop: String,
    #[serde(default)]
    variety: String,
    #[serde(default)]
    supplier: String,
    #[serde(default)]
    supplier_lot: String,
    #[serde(default)]
    acquired_on: String,
    #[serde(default)]
    packed_for_year: String,
    #[serde(default)]
    quantity: String,
    #[serde(default)]
    unit: String,
    organic: Option<String>,
    #[serde(default)]
    notes: String,
}

async fn create(c: Can<cap::GardenWrite>, Form(f): Form<LotForm>) -> UiResult {
    let input = NewSeedLot {
        crop: f.crop,
        variety: f.variety,
        supplier: f.supplier,
        supplier_lot: f.supplier_lot,
        vendor_id: None,
        acquired_on: opt_date(&f.acquired_on, "got it on", B)?,
        packed_for_year: match f.packed_for_year.trim() {
            "" => None,
            y => Some(y.parse().map_err(|_| UiError::invalid("packed for: expected a year", B))?),
        },
        organic: f.organic.map(|_| true),
        quantity: match f.quantity.trim() {
            "" => None,
            q => Some(decimal(q, "how much", B)?),
        },
        unit: match f.unit.trim() {
            "" => None,
            u => Some(u.parse::<SeedUnit>().back(B)?),
        },
        notes: f.notes,
    };
    let lot = c.store.create_seed_lot(input).await.back(B)?;
    Ok(see(&format!("/seeds/{}", lot.id)))
}

async fn page(c: Can<cap::GardenRead>, Path(id): Path<Uuid>) -> UiResult {
    let lot = c.store.get_seed_lot(id).await.back(B)?;
    let plantings = c.store.plantings_from_lot(id).await.back(B)?;
    let files = c.store.list_attachments(AttachmentOwner::SeedLot, id).await.back(B)?;
    let harvests = c.store.harvests_from_lot(id).await.back(B)?;
    Ok(layout(
        &lot.label(),
        html! {
            p {
                "From " strong { (lot.vendor_name.as_deref().unwrap_or(if lot.supplier.is_empty() { "unknown" } else { &lot.supplier })) }
                @if !lot.supplier_lot.is_empty() { ", lot " code { (lot.supplier_lot) } }
                @if let Some(d) = lot.acquired_on { ", got " (crate::ui::Day(d)) }
                @if let Some(y) = lot.packed_for_year { ", packed for " (y) }
                @if lot.organic == Some(true) { ", certified organic" }
                "."
            }
            p { (amount(&lot)) }
            @if !lot.notes.is_empty() { p.muted { (lot.notes) } }
            h2 { "Sown from this lot" }
            @if plantings.is_empty() { p.muted { "Nothing yet." } } @else {
                table {
                    tr { th { "Sown" } th { "Where" } th { "Status" } th { "Seed used" } }
                    @for (p, loc, used) in &plantings {
                        tr {
                            td { a href={"/plantings/" (p.id)} { (crate::ui::Day(p.sown_on)) } } td { (loc) } td { (p.status) }
                            td { @if let Some(u) = used { (u.normalize()) " " (lot.unit.map(|u| u.as_str()).unwrap_or("")) } }
                        }
                    }
                }
            }
            h2 { "Harvested from this lot" }
            @if harvests.is_empty() { p.muted { "Nothing yet." } } @else {
                table { @for hv in &harvests { tr {
                    td { a href={"/harvests/" (hv.id)} { (crate::ui::Day(hv.harvested_on)) } } td { (hv.crop) } td { (hv.place) }
                    td { (hv.quantity.normalize()) " " (hv.unit) }
                } } }
            }
            h2 { "Packet photos, certificates" }
            (crate::attachments::panel(&c.who, AttachmentOwner::SeedLot, id, &files, &format!("/seeds/{id}")))
            p { a href="/garden" { "← Garden" } }
        },
    )
    .into_response())
}

async fn api_list(c: Can<cap::GardenRead>) -> ApiResult {
    Ok(Json(c.store.list_seed_lots().await?).into_response())
}

async fn api_create(c: Can<cap::GardenWrite>, Json(b): Json<NewSeedLot>) -> ApiResult {
    Ok(created(c.store.create_seed_lot(b).await?))
}

async fn api_get(c: Can<cap::GardenRead>, Path(id): Path<Uuid>) -> ApiResult {
    let plantings: Vec<_> = c
        .store
        .plantings_from_lot(id)
        .await?
        .into_iter()
        .map(|(p, location_name, used)| serde_json::json!({ "planting": p, "location_name": location_name, "seed_quantity_used": used }))
        .collect();
    Ok(Json(serde_json::json!({ "lot": c.store.get_seed_lot(id).await?, "plantings": plantings })).into_response())
}

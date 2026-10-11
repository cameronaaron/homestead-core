//! Stock detail (the ledger for one item) and purchasing: vendors,
//! purchases, their lines, and receiving them into stock.

use axum::extract::Path;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use homestead_domain::attachments::AttachmentOwner;
use homestead_domain::auth::Capability;
use homestead_domain::inventory::{
    Currency, MovementKind, NewMovement, NewPurchase, NewPurchaseLine, NewVendor, Transfer,
};
use maud::{Markup, html};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::api::{ApiResult, created};
use crate::auth::{Can, cap};
use crate::ui::{Back, UiError, UiResult, decimal, layout, opt_money, options, req_date, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/inventory/{id}", get(item_page))
        .route("/inventory/{id}/movements", post(record_movement))
        .route("/inventory/{id}/transfer", post(transfer))
        .route("/purchases", get(purchases_page).post(create_purchase))
        .route("/purchases/{id}", get(purchase_page))
        .route("/purchases/{id}/lines", post(add_line))
        .route("/purchase-lines/{id}/receive", post(receive_line))
        .route("/vendors", post(create_vendor))
}

pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/inventory/{id}/movements", get(api_movements).post(api_record_movement))
        .route("/inventory/{id}/transfer", post(api_transfer))
        .route("/vendors", get(api_vendors).post(api_create_vendor))
        .route("/purchases", get(api_purchases).post(api_create_purchase))
        .route("/purchases/{id}", get(api_purchase))
        .route("/purchases/{id}/lines", post(api_add_line))
        .route("/purchase-lines/{id}/receive", post(api_receive_line))
}

fn opt_uuid(s: &str, back: &str) -> Result<Option<Uuid>, UiError> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    s.parse().map(Some).map_err(|_| UiError::invalid_at("unknown selection", back))
}

fn location_select(name: &str, locations: &[homestead_domain::Location], blank: &str) -> Markup {
    html! {
        select name=(name) {
            option value="" { (blank) }
            @for l in locations { option value=(l.id) { (l.name) } }
        }
    }
}

// ── One item's ledger ──────────────────────────────────────────────────────

async fn item_page(c: Can<cap::InventoryRead>, Path(id): Path<Uuid>) -> UiResult {
    let here = format!("/inventory/{id}");
    let b = here.as_str();
    let item = c.store.get_inventory_item(id).await.back_to(b)?;
    let balances = c.store.item_balances(id).await.back_to(b)?;
    let pace = c.store.usage_paces().await.back_to(b)?.get(&id).copied();
    let ledger = c.store.item_movements(id, 100).await.back_to(b)?;
    let locations = c.store.list_locations().await.back_to(b)?;
    let bought =
        if c.who.can(Capability::PurchasesRead) { c.store.item_purchase_history(id).await.back_to(b)? } else { vec![] };
    let can_move = c.who.can(Capability::InventoryAdjust);
    Ok(layout(
        &item.name,
        html! {
            p {
                span.stat class=[item.is_low().then_some("warn")] { (item.on_hand.normalize()) " " (item.unit) }
                span.muted { " on hand · reorder at " (item.reorder_level.normalize()) }
            }
            @if let Some(p) = pace.filter(|p| p.used > rust_decimal::Decimal::ZERO) {
                p.muted {
                    (p.used.normalize()) " " (item.unit) " used in the last " (p.over_days) " days: that lasts "
                    (crate::ui::lasts(p.days_left(item.on_hand)))
                    "."
                }
            }
            (crate::tags::make_button(&c.who, homestead_domain::tags::TagKind::InventoryItem, id))
            @if balances.len() > 1 || balances.iter().any(|b| b.location_id.is_some()) {
                table { @for bal in &balances {
                    tr { td { (bal.location_name.as_deref().unwrap_or("(not put anywhere in particular)")) } td { (bal.on_hand.normalize()) " " (item.unit) } }
                } }
            }
            @if can_move {
                div.grid {
                    form.card method="post" action={(here) "/movements"} {
                        strong { "Record a change" }
                        label { "What happened" select name="kind" { (options(MovementKind::RECORDABLE, Some(MovementKind::Consumption))) } }
                        label { "How much (" (item.unit) "; a correction can be negative)" input name="quantity" inputmode="decimal" required; }
                        label { "Where" (location_select("location_id", &locations, "— the only place it's kept —")) }
                        label { "Note" input name="note" placeholder="fed the flock, mouse got in, recount…"; }
                        button { "Record" }
                    }
                    form.card method="post" action={(here) "/transfer"} {
                        strong { "Move it somewhere else" }
                        label { "How much" input name="quantity" inputmode="decimal" required; }
                        label { "From" (location_select("from_location_id", &locations, "— not put anywhere yet —")) }
                        label { "To" (location_select("to_location_id", &locations, "— not anywhere in particular —")) }
                        label { "Note" input name="note"; }
                        button { "Move" }
                    }
                }
            }
            @if !bought.is_empty() {
                h2 { "Where it came from" }
                table {
                    tr { th { "Bought" } th { "From" } th { "How much" } th { "Each" } th {} }
                    @for (p, l) in &bought {
                        tr {
                            td { a href={"/purchases/" (p.id)} { (crate::ui::Day(p.purchased_on)) } }
                            td { (p.vendor_name.as_deref().unwrap_or("—")) }
                            td { (l.quantity.normalize()) " " (item.unit) }
                            td { @if let Some(u) = l.unit_cost { "$" (u.normalize()) } }
                            td.muted { @if l.received_at.is_some() { "received" } @else { "not received yet" } }
                        }
                    }
                }
            }
            h2 { "Ledger" }
            @if ledger.is_empty() { p.muted { "Nothing recorded yet." } } @else {
                table {
                    tr { th { "When" } th { "What" } th { "Change" } th { "Where" } th { "Who" } th { "Note" } }
                    @for m in &ledger {
                        tr {
                            td.muted { (m.at.date()) }
                            td { (m.kind.label()) }
                            td class=[m.delta.is_sign_negative().then_some("warn")] { @if m.delta.is_sign_positive() { "+" } (m.delta.normalize()) }
                            td { (m.location_name.as_deref().unwrap_or("—")) }
                            td { (m.recorded_by) }
                            td.muted { (m.note) @if let Some(pl) = m.purchase_line_id { " " code { (pl.to_string()[..8]) } } }
                        }
                    }
                }
            }
            p { a href="/inventory" { "← Inventory" } }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct MovementForm {
    kind: String,
    quantity: String,
    #[serde(default)]
    location_id: String,
    #[serde(default)]
    note: String,
}

async fn record_movement(c: Can<cap::InventoryAdjust>, Path(id): Path<Uuid>, Form(f): Form<MovementForm>) -> UiResult {
    let here = format!("/inventory/{id}");
    let input = NewMovement {
        kind: f.kind.parse::<MovementKind>().back_to(&here)?,
        quantity: decimal(f.quantity.trim().trim_start_matches('+'), "quantity", "/inventory")?,
        location_id: opt_uuid(&f.location_id, &here)?,
        note: f.note,
    };
    c.store.record_movement(id, input).await.back_to(&here)?;
    Ok(see(&here))
}

#[derive(Deserialize)]
struct TransferForm {
    quantity: String,
    #[serde(default)]
    from_location_id: String,
    #[serde(default)]
    to_location_id: String,
    #[serde(default)]
    note: String,
}

async fn transfer(c: Can<cap::InventoryAdjust>, Path(id): Path<Uuid>, Form(f): Form<TransferForm>) -> UiResult {
    let here = format!("/inventory/{id}");
    let input = Transfer {
        quantity: decimal(&f.quantity, "quantity", "/inventory")?,
        from_location_id: opt_uuid(&f.from_location_id, &here)?,
        to_location_id: opt_uuid(&f.to_location_id, &here)?,
        note: f.note,
    };
    c.store.transfer_stock(id, input).await.back_to(&here)?;
    Ok(see(&here))
}

// ── Purchases ──────────────────────────────────────────────────────────────

const P: &str = "/purchases";

async fn purchases_page(c: Can<cap::PurchasesRead>) -> UiResult {
    let purchases = c.store.list_purchases(100).await.back(P)?;
    let vendors = c.store.list_vendors().await.back(P)?;
    let can_write = c.who.can(Capability::PurchasesWrite);
    Ok(layout(
        "Purchases",
        html! {
            @if purchases.is_empty() { p.muted { "No purchases recorded yet." } } @else {
                table {
                    tr { th { "Date" } th { "From" } th { "What" } th { "Amount" } }
                    @for p in &purchases {
                        tr {
                            td { a href={"/purchases/" (p.id)} { (crate::ui::Day(p.purchased_on)) } }
                            td { (p.vendor_name.as_deref().unwrap_or("—")) }
                            td { (p.category) @if !p.description.is_empty() { " · " span.muted { (p.description) } } }
                            td { "$" (p.amount) @if p.currency != Currency::Usd { " " (p.currency) } }
                        }
                    }
                }
            }
            @if can_write {
                div.grid {
                    form.card method="post" action="/purchases" {
                        strong { "Record a purchase" }
                        label { "From" select name="vendor_id" {
                            option value="" { "—" }
                            @for v in &vendors { option value=(v.id) { (v.name) } }
                        } }
                        label { "Date" input name="purchased_on" type="date" required; }
                        label { "Total paid (USD, tax and delivery included)" input name="amount" inputmode="decimal" required; }
                        label { "Category" input name="category" required placeholder="feed, seed, tools, vet, repairs…"; }
                        label { "What for" input name="description"; }
                        label { "Invoice / order #" input name="external_reference"; }
                        button { "Record" }
                        p.muted { "Then add what was on it, attach the receipt, and receive items into stock." }
                    }
                    form.card method="post" action="/vendors" {
                        strong { "Add a vendor" }
                        label { "Name" input name="name" required; }
                        label { "Email" input name="email" type="email"; }
                        label { "Phone" input name="phone"; }
                        label { "Website" input name="website"; }
                        label { "Notes" input name="notes"; }
                        button { "Add" }
                    }
                }
            }
            @if !vendors.is_empty() {
                h2 { "Vendors" }
                table { @for v in &vendors {
                    tr { td { (v.name) } td.muted { (v.email) " " (v.phone) } td.muted { (v.notes) } }
                } }
            }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct PurchaseForm {
    #[serde(default)]
    vendor_id: String,
    purchased_on: String,
    amount: String,
    category: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    external_reference: String,
}

async fn create_purchase(c: Can<cap::PurchasesWrite>, Form(f): Form<PurchaseForm>) -> UiResult {
    let input = NewPurchase {
        vendor_id: opt_uuid(&f.vendor_id, P)?,
        purchased_on: req_date(&f.purchased_on, "date", P)?,
        amount: opt_money(&f.amount, "amount", P)?.ok_or_else(|| UiError::invalid("amount is required", P))?,
        currency: Currency::Usd,
        category: f.category,
        description: f.description,
        external_reference: Some(f.external_reference),
    };
    let p = c.store.create_purchase(input).await.back(P)?;
    Ok(see(&format!("/purchases/{}", p.id)))
}

async fn purchase_page(c: Can<cap::PurchasesRead>, Path(id): Path<Uuid>) -> UiResult {
    let p = c.store.get_purchase(id).await.back(P)?;
    let lines = c.store.purchase_lines(id).await.back(P)?;
    let files = c.store.list_attachments(AttachmentOwner::Purchase, id).await.back(P)?;
    let (can_write, can_receive) = (
        c.who.can(Capability::PurchasesWrite),
        c.who.can(Capability::PurchasesWrite) && c.who.can(Capability::InventoryAdjust),
    );
    let items = if can_write { c.store.list_inventory().await.back(P)? } else { vec![] };
    let locations = if can_receive { c.store.list_locations().await.back(P)? } else { vec![] };
    let here = format!("/purchases/{id}");
    let lines_total: rust_decimal::Decimal = lines.iter().filter_map(|l| l.unit_cost.map(|u| u * l.quantity)).sum();
    Ok(layout(
        &format!("{} · {}", p.vendor_name.as_deref().unwrap_or("Purchase"), p.purchased_on),
        html! {
            p { strong { "$" (p.amount) } " · " (p.category)
                @if !p.description.is_empty() { " · " (p.description) }
                @if let Some(r) = &p.external_reference { " · " code { (r) } } }
            h2 { "On the receipt" }
            @if lines.is_empty() { p.muted { "No lines yet." } } @else {
                table {
                    tr { th { "What" } th { "How much" } th { "Each" } th { "Stock" } }
                    @for l in &lines {
                        tr {
                            td { @if let (Some(item), Some(name)) = (l.item_id, &l.item_name) { a href={"/inventory/" (item)} { (name) } } (l.description) }
                            td { (l.quantity.normalize()) " " (l.unit.as_deref().unwrap_or("")) }
                            td { @if let Some(u) = l.unit_cost { "$" (u.normalize()) } }
                            td {
                                @if l.item_id.is_none() { span.muted { "—" } }
                                @else if l.received_at.is_some() { span.ok { "received" } }
                                @else if can_receive {
                                    form.inline method="post" action={"/purchase-lines/" (l.id) "/receive"} {
                                        select name="location_id" {
                                            option value="" { "— where? —" }
                                            @for loc in &locations { option value=(loc.id) { (loc.name) } }
                                        }
                                        button.small { "Receive" }
                                    }
                                } @else { span.muted { "not received" } }
                            }
                        }
                    }
                }
                @if !lines_total.is_zero() && lines_total != p.amount {
                    p.muted { "Lines add up to $" (lines_total.normalize()) "; the total paid was $" (p.amount) " (tax, delivery or discounts make the difference)." }
                }
            }
            @if can_write {
                form.card method="post" action={(here) "/lines"} {
                    strong { "Add a line" }
                    label { "Inventory item (leave blank for a service or fee)" select name="item_id" {
                        option value="" { "—" }
                        @for i in &items { option value=(i.id) { (i.name) " (" (i.unit) ")" } }
                    } }
                    label { "Description" input name="description"; }
                    div.grid {
                        label { "Quantity" input name="quantity" inputmode="decimal" required value="1"; }
                        label { "Cost each (USD)" input name="unit_cost" inputmode="decimal"; }
                    }
                    button { "Add" }
                }
            }
            h2 { "Receipt, invoice" }
            (crate::attachments::panel(&c.who, AttachmentOwner::Purchase, id, &files, &here))
            p { a href="/purchases" { "← All purchases" } }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct LineForm {
    #[serde(default)]
    item_id: String,
    #[serde(default)]
    description: String,
    quantity: String,
    #[serde(default)]
    unit_cost: String,
}

async fn add_line(c: Can<cap::PurchasesWrite>, Path(id): Path<Uuid>, Form(f): Form<LineForm>) -> UiResult {
    let here = format!("/purchases/{id}");
    let input = NewPurchaseLine {
        item_id: opt_uuid(&f.item_id, &here)?,
        description: f.description,
        quantity: decimal(&f.quantity, "quantity", P)?,
        unit_cost: opt_money(&f.unit_cost, "cost each", P)?,
    };
    c.store.add_purchase_line(id, input).await.back_to(&here)?;
    Ok(see(&here))
}

#[derive(Deserialize)]
struct ReceiveForm {
    #[serde(default)]
    location_id: String,
}

async fn receive_line(c: Can<cap::PurchasesWrite>, Path(id): Path<Uuid>, Form(f): Form<ReceiveForm>) -> UiResult {
    if !c.who.can(Capability::InventoryAdjust) {
        return Ok(crate::auth::deny(&c.who, Capability::InventoryAdjust));
    }
    let line = c.store.receive_purchase_line(id, opt_uuid(&f.location_id, P)?).await.back(P)?;
    Ok(see(&format!("/purchases/{}", line.purchase_id)))
}

#[derive(Deserialize)]
struct VendorForm {
    name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    phone: String,
    #[serde(default)]
    website: String,
    #[serde(default)]
    notes: String,
}

async fn create_vendor(c: Can<cap::PurchasesWrite>, Form(f): Form<VendorForm>) -> UiResult {
    let input = NewVendor { name: f.name, email: f.email, phone: f.phone, website: f.website, notes: f.notes };
    c.store.create_vendor(input).await.back(P)?;
    Ok(see(P))
}

// ── API ────────────────────────────────────────────────────────────────────

async fn api_movements(c: Can<cap::InventoryRead>, Path(id): Path<Uuid>) -> ApiResult {
    Ok(Json(serde_json::json!({
        "item": c.store.get_inventory_item(id).await?,
        "balances": c.store.item_balances(id).await?,
        "movements": c.store.item_movements(id, 500).await?,
    }))
    .into_response())
}

async fn api_record_movement(
    c: Can<cap::InventoryAdjust>,
    Path(id): Path<Uuid>,
    Json(b): Json<NewMovement>,
) -> ApiResult {
    Ok(created(c.store.record_movement(id, b).await?))
}

async fn api_transfer(c: Can<cap::InventoryAdjust>, Path(id): Path<Uuid>, Json(b): Json<Transfer>) -> ApiResult {
    Ok(created(c.store.transfer_stock(id, b).await?))
}

async fn api_vendors(c: Can<cap::PurchasesRead>) -> ApiResult {
    Ok(Json(c.store.list_vendors().await?).into_response())
}

async fn api_create_vendor(c: Can<cap::PurchasesWrite>, Json(b): Json<NewVendor>) -> ApiResult {
    Ok(created(c.store.create_vendor(b).await?))
}

async fn api_purchases(c: Can<cap::PurchasesRead>) -> ApiResult {
    Ok(Json(c.store.list_purchases(500).await?).into_response())
}

async fn api_create_purchase(c: Can<cap::PurchasesWrite>, Json(b): Json<NewPurchase>) -> ApiResult {
    Ok(created(c.store.create_purchase(b).await?))
}

async fn api_purchase(c: Can<cap::PurchasesRead>, Path(id): Path<Uuid>) -> ApiResult {
    Ok(Json(serde_json::json!({
        "purchase": c.store.get_purchase(id).await?,
        "lines": c.store.purchase_lines(id).await?,
    }))
    .into_response())
}

async fn api_add_line(c: Can<cap::PurchasesWrite>, Path(id): Path<Uuid>, Json(b): Json<NewPurchaseLine>) -> ApiResult {
    Ok(created(c.store.add_purchase_line(id, b).await?))
}

#[derive(Deserialize)]
struct ReceiveBody {
    #[serde(default)]
    location_id: Option<Uuid>,
}

async fn api_receive_line(c: Can<cap::PurchasesWrite>, Path(id): Path<Uuid>, Json(b): Json<ReceiveBody>) -> ApiResult {
    if !c.who.can(Capability::InventoryAdjust) {
        return Ok(crate::auth::deny(&c.who, Capability::InventoryAdjust));
    }
    Ok(Json(c.store.receive_purchase_line(id, b.location_id).await?).into_response())
}

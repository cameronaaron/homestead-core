//! Farm stand sales and compost drop-offs.

use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Router};
use homestead_domain::reports::FiscalYear;
use homestead_domain::stand::{NewCompostDropoff, NewStandSale, Payment, SaleUnit};
use maud::html;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::AppState;
use crate::auth::{Can, cap};
use crate::ui::{Back, UiResult, decimal, layout, opt_money, options, req_date, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/stand", get(page))
        .route("/stand/sales", post(record_sale))
        .route("/stand/compost", post(record_compost))
        .route("/stand/egg-labels", get(egg_labels))
        .route("/stand/sign", get(sign))
}

/// What the stand sells, per sunbrewedhomestead.org (checked 2026-09-28):
/// item, unit, price. Offered as suggestions; any item can be typed.
const PRICE_LIST: &[(&str, SaleUnit, &str)] = &[
    ("Eggs", SaleUnit::Dozen, "7"),
    ("Kale", SaleUnit::Bunch, "4"),
    ("Chard", SaleUnit::Bunch, "4"),
    ("Celery", SaleUnit::Bunch, "3"),
    ("Basil", SaleUnit::Bunch, "4"),
    ("Cilantro", SaleUnit::Bunch, "3"),
    ("Parsley", SaleUnit::Bunch, "3"),
    ("Basil seeds", SaleUnit::Packet, "3"),
    ("Kale seeds", SaleUnit::Packet, "3"),
    ("Black gold compost", SaleUnit::Bag, "3"),
];

async fn page(c: Can<cap::StandRead>) -> UiResult {
    const B: &str = "/stand";
    let today = c.store.today().await.back(B)?;
    let fy = FiscalYear::containing(today).first_day();
    let month = today.replace_day(1).expect("day 1 exists");
    let this_month = c.store.sales_totals(month, today).await.back(B)?;
    let this_year = c.store.sales_totals(fy, today).await.back(B)?;
    let recent = c.store.recent_sales(20).await.back(B)?;
    // Only people who can see the garden can see (and pick) its harvests.
    let harvests = if c.who.can(homestead_domain::auth::Capability::GardenRead) {
        c.store.recent_harvests(30).await.back(B)?
    } else {
        Vec::new()
    };
    let (dropoffs, pounds) = c.store.compost_totals(fy, today).await.back(B)?;
    let sum = |t: &[homestead_domain::stand::ItemTotal]| t.iter().map(|i| i.amount).sum::<Decimal>();
    Ok(layout(
        "Farm stand",
        html! {
            p { a href="/stand/egg-labels" { "Egg carton labels" } " · " a href="/stand/sign" { "Stand sign" } }
            div.grid {
                div.card { div.stat { "$" (sum(&this_month).round_dp(2)) } div.muted { "this month" } }
                div.card { div.stat { "$" (sum(&this_year).round_dp(2)) } div.muted { "this fiscal year (since " (fy) ")" } }
                div.card { div.stat { (dropoffs) } div.muted { "compost drop-offs this year · " (pounds.normalize()) " lb weighed" } }
            }
            p.muted { "Selling eggs requires CDFA egg-handler registration, and eggs must be kept at 45 °F or below. See docs/COMPLIANCE.md." }
            @if c.who.can(homestead_domain::auth::Capability::StandWrite) {
                form.card method="post" action="/stand/sales" {
                    strong { "Record a sale" }
                    datalist #items { @for (item, _, _) in PRICE_LIST { option value=(item); } }
                    label { "Item" input name="item" list="items" required placeholder="Eggs"; }
                    label { "How many" input name="quantity" inputmode="decimal" required value="1"; }
                    label { "Unit" select name="unit" { (options(SaleUnit::ALL, Some(SaleUnit::Dozen))) } }
                    label { "Paid (USD)" input name="amount" inputmode="decimal" required; }
                    label { "Payment" select name="payment" { (options(Payment::ALL, Some(Payment::Cash))) } }
                    label { "Date" input name="sold_on" type="date" required value=(today); }
                    @if !harvests.is_empty() {
                        label { "From harvest (optional, same unit)" select name="harvest_id" {
                            option value="" { "—" }
                            @for h in &harvests { option value=(h.id) { (h.crop) " · " (h.harvested_on) " · " (h.quantity.normalize()) " " (h.unit) } }
                        } }
                    }
                    p.muted { "Prices: " @for (i, (item, unit, price)) in PRICE_LIST.iter().enumerate() { @if i > 0 { " · " } (item) " $" (price) "/" (unit) } }
                    button { "Record" }
                }
            }
            @if !this_year.is_empty() {
                h2 { "This fiscal year by item" }
                table {
                    tr { th { "Item" } th { "Sold" } th { "Takings" } }
                    @for t in &this_year { tr { td { (t.item) } td { (t.quantity.normalize()) " " (t.unit) } td { "$" (t.amount.round_dp(2)) } } }
                }
            }
            @if !recent.is_empty() {
                h2 { "Recent sales" }
                table {
                    tr { th { "Date" } th { "Item" } th { "Paid" } }
                    @for r in &recent { tr { td { (crate::ui::Day(r.sold_on)) } td { (r.quantity.normalize()) " " (r.unit) " " (r.item) } td { "$" (r.amount.round_dp(2)) " " span.muted { (r.payment) } } } }
                }
            }
            @if c.who.can(homestead_domain::auth::Capability::StandWrite) {
                form.card method="post" action="/stand/compost" {
                    strong { "Log a compost drop-off" }
                    label { "From (optional)" input name="contributor"; }
                    label { "Weight in lb (if weighed)" input name="weight_lb" inputmode="decimal"; }
                    label { "Date" input name="received_on" type="date" required value=(today); }
                    label { "Notes" input name="notes" placeholder="Coffee grounds from…"; }
                    button { "Log" }
                }
            }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct SaleForm {
    item: String,
    quantity: String,
    unit: String,
    amount: String,
    payment: String,
    sold_on: String,
    #[serde(default)]
    harvest_id: String,
}

async fn record_sale(c: Can<cap::StandWrite>, Form(f): Form<SaleForm>) -> UiResult {
    const B: &str = "/stand";
    let input = NewStandSale {
        sold_on: req_date(&f.sold_on, "date", B)?,
        item: f.item,
        quantity: decimal(&f.quantity, "how many", B)?,
        unit: f.unit.parse::<SaleUnit>().back(B)?,
        amount: decimal(f.amount.trim().trim_start_matches('$'), "paid", B)?,
        payment: f.payment.parse::<Payment>().back(B)?,
        notes: String::new(),
        harvest_id: crate::ui::opt_uuid(&f.harvest_id, B)?,
    };
    c.store.record_sale(input).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct CompostForm {
    #[serde(default)]
    contributor: String,
    #[serde(default)]
    weight_lb: String,
    received_on: String,
    #[serde(default)]
    notes: String,
}

async fn record_compost(c: Can<cap::StandWrite>, Form(f): Form<CompostForm>) -> UiResult {
    const B: &str = "/stand";
    let input = NewCompostDropoff {
        received_on: req_date(&f.received_on, "date", B)?,
        contributor: f.contributor,
        weight_lb: opt_money(&f.weight_lb, "weight", B)?,
        notes: f.notes,
    };
    c.store.record_compost_dropoff(input).await.back(B)?;
    Ok(see(B))
}

// ── Egg carton labels ───────────────────────────────────────────────────────

const LABEL_CSS: &str = r"
.egg-labels{display:grid;grid-template-columns:repeat(auto-fill,3.6in);gap:.15in;margin:1rem 0}
.egg{width:3.6in;min-height:2.4in;padding:.12in;background:#fff;color:#000;border:1px dashed #999;border-radius:6px;
  font:9pt/1.25 system-ui,sans-serif;break-inside:avoid;display:flex;flex-direction:column;gap:.04in}
/* CDFA: size, grade and the compliance line at least 1/4 inch tall. 0.3in
   type puts capitals comfortably over it. */
.egg .big{font-size:.3in;line-height:1.1;font-weight:800}
.egg .who{font-weight:700}
@media print{
  header.appbar,nav,.noprint,#field-queue,.banner,.skip,h1{display:none!important}
  body,main{background:#fff!important;margin:0!important;padding:0!important}
  .egg{border:1px solid #bbb}
}";

#[derive(Deserialize)]
struct LabelQuery {
    #[serde(default)]
    packed_on: String,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    grade: Option<String>,
    #[serde(default)]
    sell_by_days: Option<i64>,
    #[serde(default)]
    count: Option<u8>,
    #[serde(default)]
    copies: Option<u8>,
}

async fn egg_labels(c: Can<cap::StandRead>, axum::extract::Query(q): axum::extract::Query<LabelQuery>) -> UiResult {
    use homestead_domain::gifts::{ORG_LEGAL_NAME, ORG_MAILING_ADDRESS};
    use homestead_domain::stand::{
        DEFAULT_SELL_BY_DAYS, EggGrade, EggLabel, EggSize, SAFE_HANDLING, SEFS_COMPLIANT, julian,
    };
    const B: &str = "/stand/egg-labels";
    let today = c.store.today().await.back(B)?;
    let packed_on = crate::ui::opt_date(&q.packed_on, "packed on", B)?.unwrap_or(today);
    let size = q.size.as_deref().map(str::parse::<EggSize>).transpose().back(B)?.unwrap_or(EggSize::Large);
    let grade = q.grade.as_deref().map(str::parse::<EggGrade>).transpose().back(B)?.unwrap_or(EggGrade::A);
    let sell_by_days = q.sell_by_days.unwrap_or(DEFAULT_SELL_BY_DAYS);
    let label = EggLabel::new(packed_on, sell_by_days, size, grade, q.count.unwrap_or(12)).back(B)?;
    let copies = q.copies.unwrap_or(8).clamp(1, 60);
    Ok(layout(
        "Egg carton labels",
        html! {
            style { (maud::PreEscaped(LABEL_CSS)) }
            div.noprint {
                p {
                    "Labels with what California's egg rules ask for on each carton. Candle and size the eggs first: "
                    "the size and grade printed must be true. See "
                    a href="https://www.cdfa.ca.gov/ahfss/mpes/pdfs/Quick_Reference_Guide.pdf" { "CDFA's quick reference" }
                    " and the egg section of the compliance notes before selling."
                }
                form.card method="get" action=(B) {
                    div.grid {
                        label { "Packed on" input name="packed_on" type="date" value=(packed_on); }
                        label { "Size" select name="size" { @for s in EggSize::ALL { option value=(s) selected[*s == size] { (s.label()) } } } }
                        label { "Grade" select name="grade" { @for g in EggGrade::ALL { option value=(g) selected[*g == grade] { (g.label()) } } } }
                        label { "Sell by (days after packing)" input name="sell_by_days" type="number" min="1" max="45" value=(sell_by_days); }
                        label { "Eggs per carton" input name="count" type="number" min="1" max="30" value=(label.count); }
                        label { "How many labels" input name="copies" type="number" min="1" max="60" value=(copies); }
                    }
                    button { "Update" } " " button type="button" onclick="print()" { "Print" }
                }
            }
            div.egg-labels {
                @for _ in 0..copies {
                    div.egg {
                        div.big { (label.size.label()) " " (label.grade.label()) }
                        div.big { (SEFS_COMPLIANT) }
                        div { strong { (label.quantity()) } " · Packed " (julian(label.packed_on)) " · Sell by " (label.sell_by) }
                        div.who { (ORG_LEGAL_NAME) }
                        div { (ORG_MAILING_ADDRESS) }
                        div { strong { "KEEP REFRIGERATED" } }
                        div { (SAFE_HANDLING) }
                    }
                }
            }
        },
    )
    .into_response())
}

// ── The stand's sign ────────────────────────────────────────────────────────

const SIGN_CSS: &str = r"
.sign{max-width:7.5in;background:#fff;color:#000;padding:.4in;border:1px dashed #999;border-radius:8px}
.sign h2{font-size:.45in;margin:0 0 .15in}
.sign table{width:100%;font-size:.22in;border-collapse:collapse}
.sign td{padding:.05in 0;border-bottom:1px solid #ddd}
.sign td:last-child{text-align:right;white-space:nowrap}
.pay{display:flex;flex-wrap:wrap;gap:.3in;margin-top:.3in}
.pay figure{margin:0;text-align:center;font-size:.18in}
.pay svg{display:block;width:1.8in;height:1.8in}
@media print{
  header.appbar,nav,.noprint,#field-queue,.banner,.skip,h1{display:none!important}
  body,main{background:#fff!important;margin:0!important;padding:0!important}
  .sign{border:0}
}";

#[derive(Deserialize)]
struct SignQuery {
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    grade: Option<String>,
}

/// A printable price list with scan-to-pay codes. Eggs name their size and
/// grade, as California requires of any sign giving an egg price.
async fn sign(
    // Only the check: the sign reads nothing from the database.
    _c: Can<cap::StandRead>,
    axum::extract::State(s): axum::extract::State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<SignQuery>,
) -> UiResult {
    use homestead_domain::stand::{EggGrade, EggSize};
    const B: &str = "/stand/sign";
    let size = q.size.as_deref().map(str::parse::<EggSize>).transpose().back(B)?.unwrap_or(EggSize::Large);
    let grade = q.grade.as_deref().map(str::parse::<EggGrade>).transpose().back(B)?.unwrap_or(EggGrade::A);
    let adopt = format!("{}/adopt", crate::tags::base_url(&s, &headers));
    Ok(layout(
        "Stand sign",
        html! {
            style { (maud::PreEscaped(SIGN_CSS)) }
            form.inline.noprint method="get" action=(B) {
                label { "Eggs are " select name="size" { @for z in EggSize::ALL { option value=(z) selected[*z == size] { (z.label()) } } } }
                " " select name="grade" aria-label="Grade" { @for g in EggGrade::ALL { option value=(g) selected[*g == grade] { (g.label()) } } }
                " " button.small { "Update" } " " button.small type="button" onclick="print()" { "Print" }
            }
            @if s.pay_links.is_empty() {
                p.noprint.warn { "No pay links are set, so the sign has no scan-to-pay codes. Add the stand's real links under [stand] in homestead.toml." }
            }
            div.sign {
                h2 { "Farm stand" }
                table {
                    @for (item, unit, price) in PRICE_LIST {
                        tr {
                            td { (item) @if *item == "Eggs" { ", " (size.label()) ", " (grade.label()) } }
                            td { "$" (price) " / " (unit) }
                        }
                    }
                }
                div.pay {
                    @for (label, url) in s.pay_links.iter() {
                        figure { (crate::tags::qr(url, 200)) figcaption { "Pay: " (label) } }
                    }
                    figure { (crate::tags::qr(&adopt, 200)) figcaption { "Adopt a cat" } }
                }
            }
        },
    )
    .into_response())
}

//! Gifts outside Zeffy: sponsors, in-kind donations, cash at the stand; and
//! the IRS acknowledgment letter for each gift of $250 or more.

use axum::extract::Path;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Router};
use homestead_domain::attachments::AttachmentOwner;
use homestead_domain::gifts::{ACKNOWLEDGMENT_THRESHOLD, GiftKind, NewGift, ORG_LEGAL_NAME, ORG_MAILING_ADDRESS};
use maud::html;
use rust_decimal::Decimal;
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Can, cap};
use crate::ui::{Back, UiResult, layout, opt_money, options, req_date, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/gifts", get(page).post(record))
        .route("/gifts/{id}/letter", get(letter))
        .route("/gifts/{id}/acknowledged", post(acknowledged))
}

async fn page(c: Can<cap::GiftsRead>) -> UiResult {
    const B: &str = "/gifts";
    let gifts = c.store.list_gifts().await.back(B)?;
    let (owed, rest): (Vec<_>, Vec<_>) = gifts.iter().partition(|g| g.needs_acknowledgment());
    let cash: Decimal = gifts.iter().filter_map(|g| g.amount).sum();
    let in_kind = gifts.iter().filter(|g| g.kind == GiftKind::InKind).count();
    Ok(layout(
        "Gifts",
        html! {
            p.muted {
                "Sponsors, in-kind donations and money handed over in person. Zeffy donations are receipted by Zeffy. "
                "Any single gift of $" (ACKNOWLEDGMENT_THRESHOLD) " or more needs a written acknowledgment (IRS Publication 1771)."
            }
            div.grid {
                div.card { div.stat { (owed.len()) } div.muted { "letters owed" } }
                div.card { div.stat { "$" (cash.round_dp(2)) } div.muted { "cash and sponsorships recorded" } }
                div.card { div.stat { (in_kind) } div.muted { "in-kind gifts" } }
            }
            @if !owed.is_empty() {
                h2.warn { "Owed a thank-you letter" }
                (table(&owed))
            }
            @if c.who.can(homestead_domain::auth::Capability::GiftsWrite) {
                form.card method="post" action="/gifts" {
                    strong { "Record a gift" }
                    label { "From (person or organization)" input name="donor_name" required; }
                    label { "Their email (optional)" input name="donor_email" type="email"; }
                    label { "Kind" select name="kind" { (options(GiftKind::ALL, Some(GiftKind::InKind))) } }
                    label { "Received on" input name="received_on" type="date" required; }
                    label { "Amount in USD (cash and sponsorships only)" input name="amount" inputmode="decimal"; }
                    label { "What it was (required for in-kind)" input name="description" placeholder="40 vegetable starts; 12 pallets of lumber"; }
                    label { "Their estimate of its value (in-kind, for our books; never printed)" input name="estimated_value" inputmode="decimal"; }
                    label { "Anything we gave in return" input name="goods_or_services" placeholder="Leave empty if nothing; e.g. logo on our banner"; }
                    label { "Its good-faith value (USD)" input name="goods_value" inputmode="decimal"; }
                    label { "Designated for" input name="fund" placeholder="Kiki's Cat Club, Chicken coop…"; }
                    label { "Notes" input name="notes"; }
                    button { "Record" }
                }
            }
            @if !rest.is_empty() {
                h2 { "All other gifts" }
                (table(&rest))
            }
        },
    )
    .into_response())
}

fn table(gifts: &[&homestead_domain::gifts::Gift]) -> maud::Markup {
    html! {
        table {
            tr { th { "Received" } th { "From" } th { "What" } th { "Letter" } }
            @for g in gifts {
                tr {
                    td { (crate::ui::Day(g.received_on)) }
                    td { (g.donor_name) br; span.muted { (g.kind) } }
                    td {
                        @if let Some(a) = g.amount { "$" (a.round_dp(2)) " " }
                        (g.description)
                        @if !g.fund.is_empty() { br; span.muted { "for " (g.fund) } }
                    }
                    td {
                        a href={"/gifts/" (g.id) "/letter"} { "Letter" }
                        @if let Some(on) = g.acknowledged_on { br; span.ok { "sent " (on) } }
                    }
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct GiftForm {
    donor_name: String,
    #[serde(default)]
    donor_email: String,
    kind: String,
    received_on: String,
    #[serde(default)]
    amount: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    estimated_value: String,
    #[serde(default)]
    goods_or_services: String,
    #[serde(default)]
    goods_value: String,
    #[serde(default)]
    fund: String,
    #[serde(default)]
    notes: String,
}

async fn record(c: Can<cap::GiftsWrite>, Form(f): Form<GiftForm>) -> UiResult {
    const B: &str = "/gifts";
    let input = NewGift {
        donor_name: f.donor_name,
        donor_email: f.donor_email,
        kind: f.kind.parse::<GiftKind>().back(B)?,
        received_on: req_date(&f.received_on, "received on", B)?,
        amount: opt_money(&f.amount, "amount", B)?,
        description: f.description,
        estimated_value: opt_money(&f.estimated_value, "estimated value", B)?,
        goods_or_services: f.goods_or_services,
        goods_value: opt_money(&f.goods_value, "value of what we gave", B)?,
        fund: f.fund,
        notes: f.notes,
    };
    c.store.record_gift(input).await.back(B)?;
    Ok(see(B))
}

async fn letter(c: Can<cap::GiftsRead>, Path(id): Path<Uuid>) -> UiResult {
    const B: &str = "/gifts";
    let g = c.store.get_gift(id).await.back(B)?;
    let files = c.store.list_attachments(AttachmentOwner::Gift, id).await.back(B)?;
    let today = c.store.today().await.back(B)?;
    Ok(layout(
        "Acknowledgment",
        html! {
            div.card.letter {
                p { strong { (ORG_LEGAL_NAME) } br; (ORG_MAILING_ADDRESS) br; "sunbrewedhomestead.org" }
                p { (today) }
                @for para in g.acknowledgment() { p { (para) } }
                p { "With gratitude," br; br; "Sun Brewed Homestead" }
            }
            div.noprint {
                p.muted { "Print or save as PDF (⌘P) and send it" @if !g.donor_email.is_empty() { " to " (g.donor_email) } ", then record that it went out." }
                @if let Some(on) = g.acknowledged_on { p.ok { "Sent " (on) "." } } @else {
                    form.inline method="post" action={"/gifts/" (g.id) "/acknowledged"} {
                        input name="on" type="date" value=(today) required;
                        button.small { "Mark as sent" }
                    }
                }
                h2 { "Files" }
                p.muted { "Receipts, the donor's own appraisal, photos of what came in." }
                (crate::attachments::panel(&c.who, AttachmentOwner::Gift, id, &files, &format!("/gifts/{id}/letter")))
                p { a href="/gifts" { "← All gifts" } }
            }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct AckForm {
    on: String,
}

async fn acknowledged(c: Can<cap::GiftsWrite>, Path(id): Path<Uuid>, Form(f): Form<AckForm>) -> UiResult {
    const B: &str = "/gifts";
    c.store.mark_gift_acknowledged(id, req_date(&f.on, "date", B)?).await.back(B)?;
    Ok(see(&format!("/gifts/{id}/letter")))
}

//! The public adoption page, and the applications it collects.
//!
//! `/adopt` needs no account: it's the page to link from Instagram. It
//! shows adoptable animals only by name, species, sex, age and the blurb
//! written for the page (never notes or vet records), and takes an
//! application. Applicants' details are read on `/applications` by those
//! who can change animals.

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Router};
use homestead_domain::adoptions::{ApplicationStatus, NewApplication};
use maud::{PreEscaped, html};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Can, cap};
use crate::ui::{Back, UiResult, layout, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/applications", get(applications))
        .route("/applications/{id}/decide", post(decide))
        .route("/animals/{id}/blurb", post(set_blurb))
}

/// Reachable without signing in; see the module docs.
pub fn public_routes() -> Router<AppState> {
    Router::new().route("/adopt", get(page).post(apply))
}

/// Hides the honeypot from people (and screen readers) but not from bots
/// that fill every field.
const HONEYPOT_CSS: &str = ".hp{position:absolute;left:-10000px;width:1px;height:1px;overflow:hidden}";

#[derive(Deserialize)]
struct PageQuery {
    #[serde(default)]
    sent: Option<String>,
    #[serde(default)]
    animal: Option<Uuid>,
}

async fn page(State(s): State<AppState>, Query(q): Query<PageQuery>) -> UiResult {
    const B: &str = "/adopt";
    let store = s.public_store("adoption page");
    let animals = store.adoptable().await.back(B)?;
    let today = store.today().await.back(B)?;
    Ok(layout(
        "Adopt",
        html! {
            style { (PreEscaped(HONEYPOT_CSS)) }
            @if q.sent.is_some() {
                p.banner role="status" { "Thank you! We read every application and will get back to you by email." }
            }
            p { "Kiki's Cat Club at Sun Brewed Homestead finds homes for the cats in our care. Meet who's looking for one:" }
            @if animals.is_empty() { p.muted { "Nobody is up for adoption right now. Check back soon, or apply below and we'll be in touch." } }
            div.grid {
                @for a in &animals {
                    div.card {
                        strong { (a.name) }
                        p.muted { (a.species) " · " (a.sex) @if let Some(age) = a.age(today) { " · " (age) } }
                        @if !a.blurb.is_empty() { p { (a.blurb) } }
                        a href={"/adopt?animal=" (a.id) "#apply"} { "Apply for " (a.name) }
                    }
                }
            }
            h2 id="apply" { "Apply to adopt" }
            form.card method="post" action="/adopt" {
                label { "Who you'd like to adopt" select name="animal_id" {
                    option value="" { "Any who'd fit my home" }
                    @for a in &animals { option value=(a.id) selected[q.animal == Some(a.id)] { (a.name) " (" (a.species) ")" } }
                } }
                label { "Your name" input name="applicant_name" required maxlength="80" autocomplete="name"; }
                label { "Email" input name="email" type="email" required maxlength="254" autocomplete="email"; }
                label { "Phone (optional)" input name="phone" type="tel" maxlength="40" autocomplete="tel"; }
                label { "Tell us about your home"
                    textarea name="about" rows="5" maxlength="4000"
                        placeholder="House or apartment, other pets, kids, whether your landlord allows cats…" {} }
                div.hp aria-hidden="true" { label { "Leave this empty" input name="website" tabindex="-1" autocomplete="off"; } }
                button { "Send application" }
                p.muted { "We use your details only to talk with you about adopting." }
            }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct ApplyForm {
    #[serde(default)]
    animal_id: String,
    applicant_name: String,
    email: String,
    #[serde(default)]
    phone: String,
    #[serde(default)]
    about: String,
    /// The honeypot: people never see it, so anything in it is a bot.
    #[serde(default)]
    website: String,
}

async fn apply(State(s): State<AppState>, Form(f): Form<ApplyForm>) -> UiResult {
    const B: &str = "/adopt";
    if !f.website.is_empty() {
        // Looks accepted, so the bot learns nothing.
        return Ok(see("/adopt?sent=1"));
    }
    let animal_id = crate::ui::opt_uuid(&f.animal_id, B)?;
    let input =
        NewApplication { animal_id, applicant_name: f.applicant_name, email: f.email, phone: f.phone, about: f.about };
    s.public_store("adoption page").apply_to_adopt(input).await.back(B)?;
    Ok(see("/adopt?sent=1"))
}

async fn applications(c: Can<cap::AnimalsWrite>) -> UiResult {
    const B: &str = "/applications";
    let apps = c.store.list_applications(200).await.back(B)?;
    Ok(layout(
        "Adoption applications",
        html! {
            p.muted {
                "From the public page, " a href="/adopt" { "/adopt" } ": link it from Instagram. "
                "After approving, send the cat adoption agreement from "
                @if c.who.can(homestead_domain::auth::Capability::ReleasesRead) { a href="/releases" { "Releases" } } @else { "Releases (ask a lead)" }
                ", then record the adoption on the animal's page."
            }
            @if apps.is_empty() { p.muted { "No applications yet." } }
            @for a in &apps {
                div.card {
                    strong { (a.applicant_name) }
                    " " span.pill { (a.status) }
                    " for " @match (&a.animal_name, a.animal_id) {
                        (Some(n), Some(id)) => { a href={"/animals/" (id)} { (n) } }
                        _ => { "any animal" }
                    }
                    p.muted {
                        a href={"mailto:" (a.email)} { (a.email) }
                        @if !a.phone.is_empty() { " · " (a.phone) }
                        " · " (a.received_at.date())
                    }
                    @if !a.about.is_empty() { p style="white-space:pre-wrap" { (a.about) } }
                    @if a.status == ApplicationStatus::New {
                        form.inline method="post" action={"/applications/" (a.id) "/decide"} {
                            button.small name="status" value="approved" { "Approve" }
                            " " button.small name="status" value="declined" { "Decline" }
                            " " button.small name="status" value="withdrawn" { "Withdrawn" }
                        }
                    }
                }
            }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct DecideForm {
    status: String,
}

async fn decide(c: Can<cap::AnimalsWrite>, Path(id): Path<Uuid>, Form(f): Form<DecideForm>) -> UiResult {
    const B: &str = "/applications";
    c.store.decide_application(id, f.status.parse::<ApplicationStatus>().back(B)?).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct BlurbForm {
    #[serde(default)]
    blurb: String,
}

async fn set_blurb(c: Can<cap::AnimalsWrite>, Path(id): Path<Uuid>, Form(f): Form<BlurbForm>) -> UiResult {
    let here = format!("/animals/{id}");
    c.store.set_adoption_blurb(id, &f.blurb).await.back_to(&here)?;
    Ok(see(&here))
}

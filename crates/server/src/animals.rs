//! Animals and Kiki's Cat Club: who's here, their care, and where they went.

use axum::extract::Path;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Router};
use homestead_domain::animals::{
    AnimalOrigin, AnimalStatus, CareKind, ChipRegistrant, Identity, NewAnimal, NewCareEntry, NewPlacement,
    PlacementKind, Sex, Species,
};
use homestead_domain::attachments::AttachmentOwner;
use maud::html;
use serde::Deserialize;
use time::macros::date;
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Can, cap};
use crate::ui::{Back, UiError, UiResult, layout, opt_date, opt_money, options, req_date, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/animals", get(page).post(create))
        .route("/animals/chip", get(chip_lookup))
        .route("/animals/{id}", get(detail))
        .route("/animals/{id}/identity", post(set_identity))
        .route("/animals/{id}/home", post(set_home))
        .route("/animals/{id}/tnr", post(set_tnr))
        .route("/animals/{id}/schedules", post(add_schedule))
        .route("/care-schedules/{id}/stop", post(stop_schedule))
        .route("/colonies", post(create_colony))
        .route("/colonies/{id}/estimate", post(set_estimate))
        .route("/locations/{id}/capacity", post(set_capacity))
        .route("/animals/{id}/status", post(set_status))
        .route("/animals/{id}/care", post(log_care))
        .route("/animals/{id}/placements", post(place))
}

/// Kiki's Cat Club counts cover everything recorded here. The website's
/// own tally (since August 2024) predates the app and isn't imported.
const CAT_CLUB_SINCE: time::Date = date!(2000 - 01 - 01);

async fn page(c: Can<cap::AnimalsRead>) -> UiResult {
    const B: &str = "/animals";
    let animals = c.store.list_animals().await.back(B)?;
    let cats = c.store.cat_club_numbers(CAT_CLUB_SINCE).await.back(B)?;
    let housing = c.store.housing().await.back(B)?;
    let colonies = c.store.colony_progress().await.back(B)?;
    let can_write = c.who.can(homestead_domain::auth::Capability::AnimalsWrite);
    let (here, gone): (Vec<_>, Vec<_>) =
        animals.iter().partition(|a| a.status.in_our_care() || a.status == AnimalStatus::Fostered);
    let head: i32 = here.iter().filter(|a| a.species == Species::Chicken).map(|a| a.head_count).sum();
    Ok(layout(
        "Animals",
        html! {
            (chip_box(""))
            div.grid {
                div.card { div.stat { (head) } div.muted { "chickens" } }
                div.card { div.stat { (cats.in_our_care) " + " (cats.fostered_now) } div.muted { "cats with us + in foster" } }
                div.card { div.stat { (cats.adoptions) } div.muted { "cat adoptions recorded" } }
                div.card { div.stat { (cats.returned_to_colony) } div.muted { "cats returned to their colony (TNR) recorded" } }
            }
            @if !housing.is_empty() {
                h2 { "Where they live" }
                table {
                    tr { th { "Place" } th { "Living there" } th { "Holds" } th { "Room" } }
                    @for h in &housing {
                        tr class=[h.over_capacity().then_some("warn")] {
                            td { a href={"/locations/" (h.location_id)} { (h.name) } }
                            td { (h.head) }
                            td { @match h.capacity { Some(n) => { (n) } None => { span.muted { "not set" } } } }
                            td { @match h.room() {
                                Some(r) if r < 0 => { (-r) " too many" }
                                Some(r) => { (r) " more" }
                                None => {}
                            } }
                        }
                    }
                }
            }
            (colony_section(&colonies, can_write))
            h2 { "Here now" }
            @if here.is_empty() { p.muted { "Nobody recorded yet. Add the flock and each cat below." } } @else {
                table {
                    tr { th { "Name" } th { "Species" } th { "Status" } th { "Since" } }
                    @for a in &here {
                        tr {
                            td { a href={"/animals/" (a.id)} { (a.name) } @if a.head_count > 1 { " ×" (a.head_count) } }
                            td { (a.species) } td { span.pill { (a.status) } } td.muted { (crate::ui::Day(a.arrived_on)) }
                        }
                    }
                }
            }
            @if c.who.can(homestead_domain::auth::Capability::AnimalsWrite) {
                form.card method="post" action="/animals" {
                    strong { "Add an animal (or a flock)" }
                    label { "Name" input name="name" required placeholder="Mochi, or Laying flock"; }
                    label { "Species" select name="species" { (options(Species::ALL, Some(Species::Cat))) } }
                    label { "How many (flocks only; cats and goats one at a time)" input name="head_count" type="number" min="1" value="1"; }
                    label { "Sex" select name="sex" { (options(Sex::ALL, Some(Sex::Unknown))) } }
                    label { "Status" select name="status" { (options(AnimalStatus::ALL, Some(AnimalStatus::InCare))) } }
                    label { "Came from" select name="origin" { (options(AnimalOrigin::ALL, Some(AnimalOrigin::Trapped))) } }
                    label { "Arrived on" input name="arrived_on" type="date" required; }
                    label { "Born on (if known)" input name="born_on" type="date"; }
                    label { "Microchip" input name="microchip"; }
                    label { "Notes" textarea name="notes" rows="2" {} }
                    button { "Add" }
                }
            }
            @if !gone.is_empty() {
                h2 { "Placed, released or passed" }
                table {
                    tr { th { "Name" } th { "Species" } th { "Status" } }
                    @for a in &gone {
                        tr { td { a href={"/animals/" (a.id)} { (a.name) } } td { (a.species) } td.muted { (a.status) } }
                    }
                }
            }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct AnimalForm {
    name: String,
    species: String,
    #[serde(default)]
    head_count: String,
    sex: String,
    status: String,
    origin: String,
    arrived_on: String,
    #[serde(default)]
    born_on: String,
    #[serde(default)]
    microchip: String,
    #[serde(default)]
    notes: String,
}

async fn create(c: Can<cap::AnimalsWrite>, Form(f): Form<AnimalForm>) -> UiResult {
    const B: &str = "/animals";
    let head_count = match f.head_count.trim() {
        "" => 1,
        n => n.parse().map_err(|_| UiError::invalid("how many: expected a whole number", B))?,
    };
    let input = NewAnimal {
        species: f.species.parse::<Species>().back(B)?,
        name: f.name,
        head_count,
        sex: f.sex.parse::<Sex>().back(B)?,
        status: f.status.parse::<AnimalStatus>().back(B)?,
        origin: f.origin.parse::<AnimalOrigin>().back(B)?,
        arrived_on: req_date(&f.arrived_on, "arrived on", B)?,
        born_on: opt_date(&f.born_on, "born on", B)?,
        microchip: f.microchip,
        notes: f.notes,
    };
    let a = c.store.create_animal(input).await.back(B)?;
    Ok(see(&format!("/animals/{}", a.id)))
}

async fn detail(c: Can<cap::AnimalsRead>, Path(id): Path<Uuid>) -> UiResult {
    const B: &str = "/animals";
    let a = c.store.get_animal(id).await.back(B)?;
    // Care is the animal's veterinary record: animals.medical only.
    let medical = c.who.can(homestead_domain::auth::Capability::AnimalsMedical);
    let care = if medical { c.store.list_care(id).await.back(B)? } else { Vec::new() };
    let schedules = if medical { c.store.care_schedules(Some(id), None).await.back(B)? } else { Vec::new() };
    let photos = c.store.list_attachments(AttachmentOwner::Animal, id).await.back(B)?;
    let mut vet_files = Vec::with_capacity(care.len());
    for entry in &care {
        vet_files.push(c.store.list_attachments(AttachmentOwner::AnimalCare, entry.id).await.back(B)?);
    }
    let placements = c.store.list_placements(id).await.back(B)?;
    let today = c.store.today().await.back(B)?;
    let can_write = c.who.can(homestead_domain::auth::Capability::AnimalsWrite);
    let places = if can_write { c.store.list_locations().await.back(B)? } else { Vec::new() };
    let moved = crate::movements::section(&c.who, &c.store, homestead_domain::movements::Mover::Animal(id), &places)
        .await
        .back(B)?;
    let colonies = if a.species == Species::Cat && can_write {
        c.store.colony_progress().await.back(B)?.into_iter().map(|p| p.colony).collect()
    } else {
        Vec::new()
    };
    let colony_name = match a.colony_id {
        Some(id) => {
            c.store.colony_progress().await.back(B)?.into_iter().find(|p| p.colony.id == id).map(|p| p.colony.name)
        }
        None => None,
    };
    let home = match a.home_location_id {
        Some(l) => Some(c.store.get_location(l).await.back(B)?.name),
        None => None,
    };
    let here = format!("/animals/{id}");
    Ok(layout(
        &a.name,
        html! {
            p {
                span.pill { (a.species) } " " span.pill { (a.status) } " "
                @if a.head_count > 1 { (a.head_count) " birds · " }
                (a.sex) " · came from " (a.origin) " · arrived " (crate::ui::Day(a.arrived_on))
                @if let Some(b) = a.born_on { " · born " (b) }
                @if !a.microchip.is_empty() { " · chip " code { (a.microchip) } }
                @if !a.band.is_empty() { " · band " (a.band) }
                @if let Some(h) = &home { " · lives in " (h) }
                @if let Some(col) = &colony_name { " · from the " (col) " colony" }
                @if a.ear_tipped { " · ear-tipped" }
            }
            @if !a.microchip.is_empty() {
                p class=[chip_needs_transfer(&a).then_some("warn")] {
                    "Chip registered to " strong { (registrant(a.chip_registered_to)) }
                    @if !a.chip_registry.is_empty() { " with " (a.chip_registry) }
                    @if chip_needs_transfer(&a) { " — adopted, so it should be registered to the adopter." }
                }
            }
            @if !a.notes.is_empty() { p.muted { (a.notes) } }
            (crate::tags::make_button(&c.who, homestead_domain::tags::TagKind::Animal, id))
            (crate::attachments::panel(&c.who, AttachmentOwner::Animal, id, &photos, &here))
            @if c.who.can(homestead_domain::auth::Capability::AnimalsWrite) {
                form.inline method="post" action={(here) "/status"} {
                    select name="status" { (options(AnimalStatus::ALL, Some(a.status))) }
                    button.small { "Set status" }
                }
            }
            @if medical {
                h2 { "Coming up" }
                @if schedules.is_empty() { p.muted { "Nothing scheduled." } } @else {
                    table {
                        tr { th { "Next due" } th { "What" } th { "Every" } th { "Until" } th {} }
                        @for s in &schedules {
                            tr {
                                td class=[(s.next_due_on <= today).then_some("warn")] { (crate::ui::Day(s.next_due_on)) }
                                td { (s.kind) @if !s.details.is_empty() { br; span.muted { (s.details) } } }
                                td { (s.every_days) " days" }
                                td { @if let Some(e) = s.ends_on { (e) } }
                                td { form.inline method="post" action={"/care-schedules/" (s.id) "/stop"} { button.small { "Stop" } } }
                            }
                        }
                    }
                }
                details {
                    summary { "Schedule care" }
                    form.card method="post" action={(here) "/schedules"} {
                        div.grid {
                            label { "What" select name="kind" { (options(CareKind::ALL, Some(CareKind::Medication))) } }
                            label { "Every (days)" input name="every_days" type="number" min="1" max="366" value="1" required; }
                            label { "First due" input name="first_due_on" type="date" value=(today) required; }
                            label { "Until (optional)" input name="ends_on" type="date"; }
                        }
                        label { "Details" input name="details" maxlength="200" placeholder="Clavamox 0.5 ml twice a day, FVRCP booster…"; }
                        p.muted { "Logging care of the same kind moves the next due date on; the last dose ends it." }
                        button { "Schedule" }
                    }
                }
                h2 { "Care" }
            }
            @if !medical {} @else if care.is_empty() { p.muted { "No care recorded." } } @else {
                table {
                    tr { th { "Date" } th { "What" } th { "Details" } th { "Cost" } }
                    @for (entry, files) in care.iter().zip(&vet_files) {
                        tr {
                            td { (crate::ui::Day(entry.occurred_on)) } td { (entry.kind) }
                            td {
                                (entry.details) @if !entry.provider.is_empty() { br; span.muted { (entry.provider) } }
                                (crate::attachments::panel(&c.who, AttachmentOwner::AnimalCare, entry.id, files, &here))
                            }
                            td { @if let Some(cost) = entry.cost { "$" (cost) } }
                        }
                    }
                }
            }
            (moved)
            h2 { "Placements" }
            @if placements.is_empty() { p.muted { "Not placed anywhere yet." } } @else {
                table {
                    tr { th { "Date" } th { "Kind" } th { "With" } th { "Fee" } }
                    @for p in &placements {
                        tr {
                            td { (crate::ui::Day(p.started_on)) } td { (p.kind) } td { (p.person_name) }
                            td { @if let Some(fee) = p.fee { "$" (fee) } }
                        }
                    }
                }
            }
            div.grid {
                @if medical { form.card method="post" action={(here) "/care"} {
                    strong { "Log care" }
                    label { "Date" input name="occurred_on" type="date" required; }
                    label { "What" select name="kind" { (options(CareKind::ALL, Some(CareKind::Vaccine))) } }
                    label { "Details" input name="details" placeholder="FVRCP #1, 2.1 kg…"; }
                    label { "Provider" input name="provider" placeholder="Clinic, County TNR program…"; }
                    label { "Cost (USD)" input name="cost" inputmode="decimal"; }
                    button { "Log" }
                } }
                @if c.who.can(homestead_domain::auth::Capability::AnimalsWrite) {
                    form.card method="post" action={(here) "/placements"} {
                        strong { "Place" }
                        label { "Kind" select name="kind" { (options(PlacementKind::ALL, Some(PlacementKind::Adoption))) } }
                        label { "Adopter, foster or rescue (not needed to return to a colony)" input name="person_name"; }
                        label { "Date" input name="started_on" type="date" required; }
                        label { "Adoption fee (USD)" input name="fee" inputmode="decimal"; }
                        label { "Notes" input name="notes"; }
                        p.muted { "Send the signed agreement from Releases. Adoptions are announced in Discord by the animal's name only." }
                        button { "Record" }
                    }
                }
            }
            @if can_write {
                form.inline method="post" action={(here) "/home"} {
                    label { "Lives in " select name="home" {
                        option value="" { "—" }
                        @for l in &places { option value=(l.id) selected[Some(l.id) == a.home_location_id] { (l.name) } }
                    } }
                    button.small { "Set" }
                }
                @if a.species == Species::Cat {
                    form.inline method="post" action={(here) "/tnr"} {
                        label { "Colony " select name="colony" {
                            option value="" { "—" }
                            @for col in &colonies { option value=(col.id) selected[Some(col.id) == a.colony_id] { (col.name) } }
                        } }
                        label.inline { input type="checkbox" name="ear_tipped" checked[a.ear_tipped]; " Ear-tipped" }
                        button.small { "Set" }
                    }
                }
                details open[a.status == AnimalStatus::Adoptable && a.adoption_blurb.is_empty()] {
                    summary { "For the adoption page" }
                    form.card method="post" action={(here) "/blurb"} {
                        label { "What adopters read (shown on " a href="/adopt" { "/adopt" } " while " (AnimalStatus::Adoptable) ")"
                            textarea name="blurb" rows="3" maxlength="1000" placeholder="Shy at first, loves a sunny windowsill…" { (a.adoption_blurb) } }
                        button { "Save" }
                    }
                }
                details {
                    summary { "Microchip and band" }
                    form.card method="post" action={(here) "/identity"} {
                        label { "Microchip (scan or type)" input name="microchip" value=(a.microchip) inputmode="text" autocomplete="off"; }
                        label { "Registry" input name="chip_registry" value=(a.chip_registry) placeholder="Found.org, 24Petwatch, AKC Reunite…"; }
                        label { "Registered to" select name="chip_registered_to" {
                            @for r in ChipRegistrant::ALL { option value=(r) selected[*r == a.chip_registered_to] { (registrant(*r)) } }
                        } }
                        label { "Band, ear tag or collar" input name="band" value=(a.band) maxlength="40" placeholder="Blue spiral band, left leg"; }
                        button { "Save" }
                    }
                }
            }
            p { a href="/animals" { "← All animals" } }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct StatusForm {
    status: String,
}

async fn set_status(c: Can<cap::AnimalsWrite>, Path(id): Path<Uuid>, Form(f): Form<StatusForm>) -> UiResult {
    const B: &str = "/animals";
    c.store.set_animal_status(id, f.status.parse::<AnimalStatus>().back(B)?).await.back(B)?;
    Ok(see(&format!("/animals/{id}")))
}

#[derive(Deserialize)]
struct CareForm {
    occurred_on: String,
    kind: String,
    #[serde(default)]
    details: String,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    cost: String,
}

async fn log_care(c: Can<cap::AnimalsMedical>, Path(id): Path<Uuid>, Form(f): Form<CareForm>) -> UiResult {
    const B: &str = "/animals";
    let input = NewCareEntry {
        occurred_on: req_date(&f.occurred_on, "date", B)?,
        kind: f.kind.parse::<CareKind>().back(B)?,
        details: f.details,
        provider: f.provider,
        cost: opt_money(&f.cost, "cost", B)?,
    };
    c.store.log_care(id, input).await.back(B)?;
    Ok(see(&format!("/animals/{id}")))
}

#[derive(Deserialize)]
struct PlacementForm {
    kind: String,
    #[serde(default)]
    person_name: String,
    started_on: String,
    #[serde(default)]
    fee: String,
    #[serde(default)]
    notes: String,
}

async fn place(c: Can<cap::AnimalsWrite>, Path(id): Path<Uuid>, Form(f): Form<PlacementForm>) -> UiResult {
    const B: &str = "/animals";
    let input = NewPlacement {
        kind: f.kind.parse::<PlacementKind>().back(B)?,
        person_name: f.person_name,
        started_on: req_date(&f.started_on, "date", B)?,
        fee: opt_money(&f.fee, "fee", B)?,
        notes: f.notes,
    };
    c.store.place_animal(id, input).await.back(B)?;
    Ok(see(&format!("/animals/{id}")))
}

fn registrant(r: ChipRegistrant) -> &'static str {
    match r {
        ChipRegistrant::Nobody => "nobody yet",
        ChipRegistrant::Us => "us",
        ChipRegistrant::Adopter => "the adopter",
        ChipRegistrant::Other => "someone else",
    }
}

/// Adopted, but a scan would still lead somewhere other than the adopter.
fn chip_needs_transfer(a: &homestead_domain::animals::Animal) -> bool {
    !a.microchip.is_empty() && a.status == AnimalStatus::Adopted && a.chip_registered_to != ChipRegistrant::Adopter
}

/// Where a scanner types a chip number. USB and Bluetooth chip readers in
/// keyboard mode type the number and press Enter, so this works with any
/// of them, and with a person typing off a vet's form.
fn chip_box(value: &str) -> maud::Markup {
    html! {
        form.inline.card method="get" action="/animals/chip" role="search" {
            label { "Microchip " input name="number" value=(value) required autocomplete="off" placeholder="Scan or type the number"; }
            button.small { "Look up" }
        }
    }
}

/// The AAHA Universal Pet Microchip Lookup: free, and it names the
/// registries that know a chip (not the owner; you call the registry).
const AAHA_LOOKUP: &str = "https://www.petmicrochiplookup.org/";

#[derive(Deserialize)]
struct ChipQuery {
    #[serde(default)]
    number: String,
}

async fn chip_lookup(c: Can<cap::AnimalsRead>, axum::extract::Query(q): axum::extract::Query<ChipQuery>) -> UiResult {
    const B: &str = "/animals";
    let chip = homestead_domain::animals::normalise_chip(&q.number).back(B)?;
    if chip.is_empty() {
        return Ok(see(B));
    }
    if let Some(a) = c.store.find_by_chip(&chip).await.back(B)? {
        return Ok(see(&format!("/animals/{}", a.id)));
    }
    let today = c.store.today().await.back(B)?;
    Ok(layout(
        "Not one of ours",
        html! {
            (chip_box(&chip))
            p { "No animal here has chip " code { (chip) } "." }
            p {
                "To find its owner, enter the number at the "
                a href=(AAHA_LOOKUP) rel="noopener noreferrer" target="_blank" { "AAHA Universal Pet Microchip Lookup" }
                ". It lists the registries that know the chip; call them, and they contact the owner."
            }
            @if c.who.can(homestead_domain::auth::Capability::AnimalsWrite) {
                form.card method="post" action="/animals" {
                    strong { "Record it as a new arrival" }
                    input type="hidden" name="microchip" value=(chip);
                    input type="hidden" name="head_count" value="1";
                    label { "Name" input name="name" required placeholder="What we'll call them"; }
                    label { "Species" select name="species" { (options(Species::ALL, Some(Species::Cat))) } }
                    label { "Sex" select name="sex" { (options(Sex::ALL, Some(Sex::Unknown))) } }
                    label { "Status" select name="status" { (options(AnimalStatus::ALL, Some(AnimalStatus::InCare))) } }
                    label { "Came from" select name="origin" { (options(AnimalOrigin::ALL, Some(AnimalOrigin::Stray))) } }
                    label { "Arrived on" input name="arrived_on" type="date" value=(today) required; }
                    button { "Add" }
                }
            }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct IdentityForm {
    #[serde(default)]
    microchip: String,
    #[serde(default)]
    chip_registry: String,
    chip_registered_to: String,
    #[serde(default)]
    band: String,
}

async fn set_identity(c: Can<cap::AnimalsWrite>, Path(id): Path<Uuid>, Form(f): Form<IdentityForm>) -> UiResult {
    let here = format!("/animals/{id}");
    let input = Identity {
        microchip: f.microchip,
        chip_registry: f.chip_registry,
        chip_registered_to: f.chip_registered_to.parse::<ChipRegistrant>().back_to(&here)?,
        band: f.band,
    };
    c.store.set_identity(id, input).await.back_to(&here)?;
    Ok(see(&here))
}

#[derive(Deserialize)]
struct HomeForm {
    #[serde(default)]
    home: String,
}

async fn set_home(c: Can<cap::AnimalsWrite>, Path(id): Path<Uuid>, Form(f): Form<HomeForm>) -> UiResult {
    let here = format!("/animals/{id}");
    let home = match f.home.trim() {
        "" => None,
        h => Some(h.parse().map_err(|_| UiError::invalid_at("unknown place", &here))?),
    };
    c.store.set_animal_home(id, home).await.back_to(&here)?;
    Ok(see(&here))
}

#[derive(Deserialize)]
struct CapacityForm {
    #[serde(default)]
    capacity: String,
}

async fn set_capacity(c: Can<cap::AnimalsWrite>, Path(id): Path<Uuid>, Form(f): Form<CapacityForm>) -> UiResult {
    let here = format!("/locations/{id}");
    let capacity = match f.capacity.trim() {
        "" => None,
        n => Some(n.parse().map_err(|_| UiError::invalid_at("holds: a whole number", &here))?),
    };
    c.store.set_capacity(id, capacity).await.back_to(&here)?;
    Ok(see(&here))
}

// ── TNR colonies ─────────────────────────────────────────────────────────────

fn colony_section(colonies: &[homestead_domain::animals::ColonyProgress], can_write: bool) -> maud::Markup {
    html! {
        h2 { "Colonies (TNR)" }
        @if colonies.is_empty() { p.muted { "No community cat colonies recorded." } } @else {
            table {
                tr { th { "Colony" } th { "Fixed" } th { "Left to fix" } @if can_write { th { "Caretaker" } th { "Count" } } }
                @for p in colonies {
                    tr {
                        td { (p.colony.name) @if !p.colony.area.is_empty() { br; span.muted { (p.colony.area) } } }
                        td { (p.fixed) " of " (p.recorded) " recorded" }
                        td class=[p.left_to_fix().is_some_and(|n| n > 0).then_some("warn")] {
                            @match (p.left_to_fix(), p.colony.estimated_size) {
                                (Some(n), Some(of)) => { (n) " of about " (of) }
                                _ => { span.muted { "no count yet" } }
                            }
                        }
                        @if can_write {
                            td { (p.colony.caretaker_name) @if !p.colony.caretaker_contact.is_empty() { br; span.muted { (p.colony.caretaker_contact) } } }
                            td {
                                form.inline method="post" action={"/colonies/" (p.colony.id) "/estimate"} {
                                    input name="estimated_size" type="number" min="0" value=[p.colony.estimated_size] style="width:5rem" aria-label="Cats in the colony";
                                    button.small { "Update" }
                                }
                            }
                        }
                    }
                }
            }
        }
        @if can_write {
            details {
                summary { "Add a colony" }
                form.card method="post" action="/colonies" {
                    label { "Name" input name="name" required maxlength="80" placeholder="Laundromat lot"; }
                    label { "Area (a block or landmark, not an address)" input name="area" maxlength="200"; }
                    div.grid {
                        label { "Caretaker" input name="caretaker_name" maxlength="80"; }
                        label { "Their phone or email" input name="caretaker_contact" maxlength="120"; }
                        label { "About how many cats" input name="estimated_size" type="number" min="0"; }
                    }
                    label { "Notes" input name="notes" maxlength="2000" placeholder="Feeding times, where to set traps…"; }
                    button { "Add" }
                }
            }
        }
    }
}

fn opt_count(s: &str, back: &str) -> Result<Option<i32>, UiError> {
    match s.trim() {
        "" => Ok(None),
        n => n.parse().map(Some).map_err(|_| UiError::invalid_at("expected a whole number", back)),
    }
}

#[derive(Deserialize)]
struct ColonyForm {
    name: String,
    #[serde(default)]
    area: String,
    #[serde(default)]
    caretaker_name: String,
    #[serde(default)]
    caretaker_contact: String,
    #[serde(default)]
    estimated_size: String,
    #[serde(default)]
    notes: String,
}

async fn create_colony(c: Can<cap::AnimalsWrite>, Form(f): Form<ColonyForm>) -> UiResult {
    const B: &str = "/animals";
    let input = homestead_domain::animals::NewColony {
        name: f.name,
        area: f.area,
        caretaker_name: f.caretaker_name,
        caretaker_contact: f.caretaker_contact,
        estimated_size: opt_count(&f.estimated_size, B)?,
        notes: f.notes,
    };
    c.store.create_colony(input).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct EstimateForm {
    #[serde(default)]
    estimated_size: String,
}

async fn set_estimate(c: Can<cap::AnimalsWrite>, Path(id): Path<Uuid>, Form(f): Form<EstimateForm>) -> UiResult {
    const B: &str = "/animals";
    c.store.set_colony_estimate(id, opt_count(&f.estimated_size, B)?).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct TnrForm {
    #[serde(default)]
    colony: String,
    ear_tipped: Option<String>,
}

async fn set_tnr(c: Can<cap::AnimalsWrite>, Path(id): Path<Uuid>, Form(f): Form<TnrForm>) -> UiResult {
    let here = format!("/animals/{id}");
    let colony = match f.colony.trim() {
        "" => None,
        v => Some(v.parse().map_err(|_| UiError::invalid_at("unknown colony", &here))?),
    };
    c.store.set_tnr(id, colony, f.ear_tipped.is_some()).await.back_to(&here)?;
    Ok(see(&here))
}

#[derive(Deserialize)]
struct ScheduleForm {
    kind: String,
    #[serde(default)]
    details: String,
    every_days: String,
    first_due_on: String,
    #[serde(default)]
    ends_on: String,
}

async fn add_schedule(c: Can<cap::AnimalsMedical>, Path(id): Path<Uuid>, Form(f): Form<ScheduleForm>) -> UiResult {
    const B: &str = "/animals";
    let here = format!("/animals/{id}");
    let input = homestead_domain::animals::NewCareSchedule {
        kind: f.kind.parse::<CareKind>().back_to(&here)?,
        details: f.details,
        every_days: f.every_days.trim().parse().map_err(|_| UiError::invalid_at("every: a number of days", &here))?,
        first_due_on: req_date(&f.first_due_on, "first due", B)?,
        ends_on: opt_date(&f.ends_on, "until", B)?,
    };
    c.store.add_care_schedule(id, input).await.back_to(&here)?;
    Ok(see(&here))
}

async fn stop_schedule(c: Can<cap::AnimalsMedical>, Path(id): Path<Uuid>) -> UiResult {
    const B: &str = "/animals";
    let animal =
        c.store.care_schedules(None, None).await.back(B)?.into_iter().find(|s| s.id == id).map(|s| s.animal_id);
    c.store.stop_care_schedule(id).await.back(B)?;
    Ok(see(&animal.map_or_else(|| B.to_owned(), |a| format!("/animals/{a}"))))
}

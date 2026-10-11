//! Where an animal or a piece of equipment has been, and moving it: a
//! section on its page, and what passed through a place on the place's.

use axum::extract::Path;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Form, Router};
use homestead_domain::Location;
use homestead_domain::auth::Capability;
use homestead_domain::movements::{Movement, Mover, NewMove, stays};
use homestead_store::Store;
use maud::{Markup, html};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Can, Principal, cap};
use crate::ui::{Back, UiError, UiResult, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new().route("/animals/{id}/move", post(move_animal)).route("/equipment/{id}/move", post(move_equipment))
}

/// Its stays, newest first, and for people who may, the form to move it.
pub(crate) async fn section(
    who: &Principal,
    store: &Store,
    mover: Mover,
    places: &[Location],
) -> Result<Markup, homestead_store::StoreError> {
    let moves = store.movements_of(mover).await?;
    let times = store.farm_times(&moves.iter().map(|m| m.moved_at).collect::<Vec<_>>()).await?;
    let now = time::OffsetDateTime::now_utc();
    let s = stays(&moves);
    let (action, cap) = match mover {
        Mover::Animal(id) => (format!("/animals/{id}/move"), Capability::AnimalsWrite),
        Mover::Equipment(id) => (format!("/equipment/{id}/move"), Capability::EquipmentWrite),
    };
    // Stays are newest first like the moves, and each begins at its move.
    let mut at = times.iter();
    Ok(html! {
        h2 { "Where it's been" }
        @if s.is_empty() { p.muted { "No moves recorded." } } @else {
            table {
                tr { th { "Place" } th { "From" } th { "For" } }
                @for stay in &s { tr {
                    td {
                        @match &stay.place { Some((id, name)) => a href={"/locations/" (id)} { (name) }, None => span.muted { "not on the farm's places" } }
                        @if !stay.note.is_empty() { br; span.muted { (stay.note) } }
                    }
                    td { @if let Some(t) = at.next() { (t.date()) } }
                    td { (stay.days(now)) " day" @if stay.days(now) != 1 { "s" } @if stay.until.is_none() { span.muted { " so far" } } }
                } }
            }
        }
        @if who.can(cap) {
            details.card {
                summary { "Move it" }
                form method="post" action=(action) style="display:grid;gap:.85rem" {
                    div.grid {
                        label { "To" select name="to" {
                            option value="" { "Somewhere not listed" }
                            @for l in places.iter().filter(|l| l.active) { option value=(l.id) { (l.name) } }
                        } }
                        label { "When" input name="at" type="datetime-local"; }
                    }
                    label { "Note" input name="note" maxlength="200" placeholder="Pasture A grazed down; out for repair"; }
                    p.muted { "Leave “When” empty for now. Record moves in order: one earlier than the last is refused." }
                    button { "Move" }
                }
            }
        }
    })
}

/// What arrived at or left a place (or anywhere inside it) lately.
pub(crate) async fn at_place(
    who: &Principal,
    store: &Store,
    location_id: Uuid,
) -> Result<Markup, homestead_store::StoreError> {
    let (animals, equipment) = (who.can(Capability::AnimalsRead), who.can(Capability::EquipmentRead));
    if !animals && !equipment {
        return Ok(html! {});
    }
    let moves = store.movements_at(location_id, animals, equipment, 15).await?;
    if moves.is_empty() {
        return Ok(html! {});
    }
    let times = store.farm_times(&moves.iter().map(|m| m.moved_at).collect::<Vec<_>>()).await?;
    Ok(html! {
        h2 { "Moved in and out" }
        table { @for (m, t) in moves.iter().zip(&times) { tr {
            td { (t.date()) }
            td { a href=(link(m)) { (m.mover_name) } }
            td { (direction(m)) }
        } } }
    })
}

fn link(m: &Movement) -> String {
    match m.mover {
        Mover::Animal(id) => format!("/animals/{id}"),
        Mover::Equipment(id) => format!("/equipment/{id}"),
    }
}

fn direction(m: &Movement) -> String {
    let name = |p: &Option<(Uuid, String)>| p.as_ref().map_or("somewhere not listed".to_owned(), |p| p.1.clone());
    match (&m.from, &m.to) {
        (None, Some(_)) if m.note.starts_with("where it was when") => format!("in {}", name(&m.to)),
        (None, Some(_)) => format!("arrived in {}", name(&m.to)),
        _ => format!("{} → {}", name(&m.from), name(&m.to)),
    }
}

#[derive(Deserialize)]
struct MoveForm {
    #[serde(default)]
    to: String,
    #[serde(default)]
    at: String,
    #[serde(default)]
    note: String,
}

async fn parse(store: &Store, f: MoveForm, back: &str) -> Result<NewMove, UiError> {
    let to = match f.to.trim() {
        "" => None,
        id => Some(id.parse().map_err(|_| UiError::invalid_at("unknown place", back))?),
    };
    let at = match f.at.trim() {
        "" => None,
        s => Some(store.farm_time(crate::ui::local_time_at(s, "when", back)?).await.back_to(back)?),
    };
    Ok(NewMove { to, at, note: f.note })
}

async fn move_animal(c: Can<cap::AnimalsWrite>, Path(id): Path<Uuid>, Form(f): Form<MoveForm>) -> UiResult {
    let back = format!("/animals/{id}");
    let m = parse(&c.store, f, &back).await?;
    c.store.move_to(Mover::Animal(id), m).await.back_to(&back)?;
    Ok(see(&back))
}

async fn move_equipment(c: Can<cap::EquipmentWrite>, Path(id): Path<Uuid>, Form(f): Form<MoveForm>) -> UiResult {
    let back = format!("/equipment/{id}");
    let m = parse(&c.store, f, &back).await?;
    c.store.move_to(Mover::Equipment(id), m).await.back_to(&back)?;
    Ok(see(&back).into_response())
}

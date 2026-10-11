//! People and service accounts (stewards only), your own account, and the
//! audit log.

use axum::extract::Path;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Router};
use homestead_domain::animals::{AnimalOrigin, AnimalStatus, NewAnimal, Sex, Species};
use homestead_domain::auth::{ActorKind, ActorRecord, Capability, NewActor, Role};
use homestead_domain::{Event, LocationKind, NewLocation};
use maud::{Markup, html};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Authed, Can, SETUP_LINK_TTL, cap, new_secret};
use crate::ui::{Back, UiError, UiResult, layout, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .route("/me/notifications", post(save_notices))
        .route("/welcome", get(welcome).post(finish_welcome))
        .route("/welcome/place", post(welcome_place))
        .route("/welcome/flock", post(welcome_flock))
        .route("/welcome/checklist", post(welcome_list))
        .route("/audit", get(audit))
        .route("/people", get(page).post(add))
        .route("/people/{id}/roles", post(set_roles))
        .route("/people/{id}/disable", post(disable))
        .route("/people/{id}/enable", post(enable))
        .route("/people/{id}/setup-link", post(setup_link))
        .route("/people/{id}/discord", post(link_discord))
        .route("/people/{id}/tokens", post(new_token))
        .route("/tokens/{id}/revoke", post(revoke_token))
}

const B: &str = "/people";

fn roles_line(a: &ActorRecord) -> String {
    a.roles.iter().map(|r| r.title()).collect::<Vec<_>>().join(", ")
}

async fn me(a: Authed) -> UiResult {
    let mine = a.store.audit_by_actor(a.who.actor.id, a.who.caps, 25).await.back("/")?;
    let notes = notice_form(&a).await?;
    Ok(layout(
        &a.who.actor.display_name,
        html! {
            p { "Signed in as " strong { (a.who.actor.email.as_deref().unwrap_or_default()) } " · " (roles_line(&a.who.actor)) }
            h2 { "What you can do" }
            p.muted { @for (i, c) in a.who.caps.iter().enumerate() { @if i > 0 { " · " } code { (c) } } }
            p.muted { "To change your password, ask a steward for a new setup link." }
            (notes)
            h2 { "Your recent actions" }
            (audit_table(&mine))
        },
    )
    .into_response())
}

fn welcome_lines(who: &crate::auth::Principal) -> Vec<&'static str> {
    use homestead_domain::auth::Capability as C;
    let mut lines = Vec::new();
    if who.can(C::TasksWrite) {
        lines.push("You can add tasks and mark them done.");
    } else if who.can(C::TasksRead) {
        lines.push("You can see the task list.");
    }
    if who.can(C::GardenWrite) {
        lines.push("You can log watering, harvests, and losses from Today.");
    } else if who.can(C::GardenRead) {
        lines.push("You can see the beds, plantings, and what is due in them.");
    }
    if who.can(C::AnimalsWrite) {
        lines.push("You can record eggs and other animal care.");
    }
    if who.can(C::InventoryRead) {
        lines.push("You can see when something is low on stock.");
    }
    if who.can(C::IncidentsReport) {
        lines.push("You can file a safety report. Reports stay off search.");
    }
    lines
}

async fn welcome(a: Authed) -> UiResult {
    use Capability as C;
    let lines = welcome_lines(&a.who);
    let garden = a.who.can(C::GardenWrite);
    let animals = a.who.can(C::AnimalsWrite);
    let tasks = a.who.can(C::TasksWrite);
    let places = if garden { a.store.list_locations().await.back("/welcome")? } else { Vec::new() };
    let flocks = if animals { a.store.list_animals().await.back("/welcome")? } else { Vec::new() };
    let lists = if tasks { a.store.list_checklist_templates().await.back("/welcome")? } else { Vec::new() };
    Ok(layout(
        "Welcome",
        html! {
            p { "Hi " (a.who.actor.display_name) ". You sign in as " strong { (a.who.actor.email.as_deref().unwrap_or_default()) } "." }
            p { "Your roles: " (roles_line(&a.who.actor)) "." }
            h2 { "Today" }
            p { "The first page is the day's work: what is late, what is due, and the water, harvest, eggs, and checklists you can record without opening another page." }
            h2 { "What you can do" }
            ul { @for line in &lines { li { (line) } } }
            @if garden && places.is_empty() {
                form.card method="post" action="/welcome/place" {
                    strong { "First place" }
                    label { "Place name" input name="name" required placeholder="North bed"; }
                    label { "Kind" select name="kind" required { (crate::ui::options(LocationKind::ALL, Some(LocationKind::Bed))) } }
                    button type="submit" { "Add the place" }
                }
            } @else if garden {
                p.muted { "Places are already recorded." }
            }
            @if animals && flocks.is_empty() {
                form.card method="post" action="/welcome/flock" {
                    strong { "First flock" }
                    label { "Flock name" input name="name" required placeholder="Laying flock"; }
                    label { "Birds" input name="head_count" type="number" min="1" value="1" inputmode="numeric" required; }
                    p.muted { "This records a chicken flock. A goat or a cat is added one at a time on Animals." }
                    button type="submit" { "Add the flock" }
                }
            } @else if animals {
                p.muted { "A flock is already recorded." }
            }
            @if tasks && lists.is_empty() {
                form.card method="post" action="/welcome/checklist" {
                    strong { "First checklist" }
                    label { "Checklist name" input name="name" required placeholder="Morning round"; }
                    label { "Items, one per line" textarea name="items" required rows="4" { "Latch the gate\nWater\nCollect eggs" } }
                    button type="submit" { "Save and start today" }
                }
            } @else if tasks {
                p.muted { "A checklist is already recorded." }
            }
            form method="post" action="/welcome" { button type="submit" { "Go to Today" } }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct PlaceForm {
    name: String,
    kind: String,
}

async fn welcome_place(c: Can<cap::GardenWrite>, Form(f): Form<PlaceForm>) -> UiResult {
    let kind = f.kind.parse::<LocationKind>().map_err(|_| UiError::invalid("choose a kind of place", "/welcome"))?;
    c.store.create_location(NewLocation::named(f.name, kind)).await.back("/welcome")?;
    Ok(see("/welcome"))
}

#[derive(Deserialize)]
struct FlockForm {
    name: String,
    head_count: String,
}

async fn welcome_flock(c: Can<cap::AnimalsWrite>, Form(f): Form<FlockForm>) -> UiResult {
    let n: i32 = f.head_count.trim().parse().map_err(|_| UiError::invalid("birds: expected a number", "/welcome"))?;
    if n < 1 {
        return Err(UiError::invalid("birds: expected a number", "/welcome"));
    }
    let today = c.store.today().await.back("/welcome")?;
    c.store
        .create_animal(NewAnimal {
            species: Species::Chicken,
            name: f.name,
            head_count: n,
            sex: Sex::Female,
            status: AnimalStatus::InCare,
            origin: AnimalOrigin::Purchased,
            arrived_on: today,
            born_on: None,
            microchip: String::new(),
            notes: String::new(),
        })
        .await
        .back("/welcome")?;
    Ok(see("/welcome"))
}

#[derive(Deserialize)]
struct ListForm {
    name: String,
    items: String,
}

async fn welcome_list(c: Can<cap::TasksWrite>, Form(f): Form<ListForm>) -> UiResult {
    let items: Vec<String> = f.items.lines().map(|l| l.trim().to_owned()).filter(|l| !l.is_empty()).collect();
    let id = c.store.create_checklist(&f.name, &items).await.back("/welcome")?;
    let today = c.store.today().await.back("/welcome")?;
    c.store.start_checklist(id, today, &c.who.actor.display_name).await.back("/welcome")?;
    Ok(see("/welcome"))
}

async fn finish_welcome(a: Authed) -> UiResult {
    a.store.mark_welcomed(a.who.actor.id).await.back("/welcome")?;
    Ok(see("/"))
}

async fn notice_form(a: &Authed) -> Result<Markup, UiError> {
    let prefs = a.store.notification_prefs(a.who.actor.id).await.back("/me")?;
    let linked = a.who.actor.external_subject.as_deref().is_some_and(|s| s.starts_with("discord:"));
    let on =
        |channel: &str, kind: &str| prefs.get(&(channel.into(), kind.into())).copied().unwrap_or(channel == "today");
    Ok(html! {
        h2 { "Notifications" }
        p.muted { "Today lists recent farm events you can already see. A Discord ping is added to the farm channel only for the ones you tick here, and only when a steward has linked your Discord user. Turning one off does not stop the channel message or a Linear issue." }
        form.card method="post" action="/me/notifications" {
            table {
                tr { th { "Event" } th { "On Today" } th { "Discord ping" } }
                @for kind in Event::KINDS {
                    @if Event::audience(kind).is_some_and(|c| a.who.caps.has(c)) {
                        tr {
                            td { (Event::notice(kind)) }
                            td { label.inline { input type="checkbox" name="on" value={"today|" (kind)} checked[on("today", kind)]; " Show" } }
                            td {
                                @if linked {
                                    label.inline { input type="checkbox" name="on" value={"discord|" (kind)} checked[on("discord", kind)]; " Ping me" }
                                } @else {
                                    span.muted { "Link Discord first" }
                                }
                            }
                        }
                    }
                }
            }
            button type="submit" { "Save notifications" }
        }
    })
}

async fn save_notices(a: Authed, body: String) -> UiResult {
    let chosen: std::collections::HashSet<(String, String)> = form_pairs(&body)
        .into_iter()
        .filter(|(k, _)| k == "on")
        .filter_map(|(_, v)| {
            let (channel, kind) = v.split_once('|')?;
            Some((channel.to_owned(), kind.to_owned()))
        })
        .collect();
    let linked = a.who.actor.external_subject.as_deref().is_some_and(|s| s.starts_with("discord:"));
    for kind in Event::KINDS {
        let Some(cap) = Event::audience(kind) else { continue };
        if !a.who.caps.has(cap) {
            continue;
        }
        let today_on = chosen.contains(&("today".into(), (*kind).into()));
        a.store.set_notification(a.who.actor.id, "today", kind, today_on).await.back("/me")?;
        if linked {
            let ping = chosen.contains(&("discord".into(), (*kind).into()));
            a.store.set_notification(a.who.actor.id, "discord", kind, ping).await.back("/me")?;
        }
    }
    Ok(see("/me"))
}

async fn audit(c: Can<cap::AuditRead>) -> UiResult {
    let entries = c.store.recent_audit_entries(c.who.caps, 200).await.back("/")?;
    Ok(layout(
        "Audit log",
        html! { p.muted { "Every change, newest first, and who made it." } (audit_table(&entries)) },
    )
    .into_response())
}

fn audit_table(entries: &[homestead_store::AuditEntry]) -> Markup {
    html! {
        @if entries.is_empty() { p.muted { "Nothing yet." } } @else {
            table {
                tr { th { "When" } th { "Who" } th { "Did" } th { "To" } }
                @for e in entries {
                    tr {
                        td.muted { (e.occurred_at.date()) " " (e.occurred_at.time().hour()) ":" (format!("{:02}", e.occurred_at.time().minute())) }
                        td { (e.actor_label) }
                        td { (e.action) }
                        td { (e.resource_type) @if let Some(id) = &e.resource_id { " " code { (id.chars().take(8).collect::<String>()) } } }
                    }
                }
            }
        }
    }
}

fn role_boxes(checked: &[Role]) -> Markup {
    html! {
        fieldset.chips {
            legend { "Roles" }
            @for r in Role::ALL.iter().filter(|r| **r != Role::Service) {
                label.inline { input type="checkbox" name="role" value=(r) checked[checked.contains(r)]; (r.title()) }
            }
        }
    }
}

async fn page(c: Can<cap::SystemsAdmin>) -> UiResult {
    let people = c.store.list_actors().await.back(B)?;
    let tokens = c.store.list_api_tokens().await.back(B)?;
    Ok(layout(
        "People",
        html! {
            p.muted { "Roles decide what each person can see and do. Changes take effect on their next page." }
            @for a in &people {
                div.card {
                    strong { (a.display_name) } " "
                    @if a.disabled_at.is_some() { span.pill.warn { "disabled" } }
                    @if a.kind == ActorKind::Service { span.pill { "service" } }
                    br;
                    span.muted {
                        @if let Some(e) = &a.email { (e) " · " }
                        (roles_line(a))
                        @if a.kind == ActorKind::Human && !a.has_password { " · no password yet" }
                        @if let Some(x) = &a.external_subject { " · linked " code { (x) } }
                    }
                    @if a.kind == ActorKind::Human {
                        form.roles method="post" action={"/people/" (a.id) "/roles"} { (role_boxes(&a.roles)) button.small { "Save roles" } }
                    }
                    div.row {
                    @if a.kind == ActorKind::Human {
                        form.inline method="post" action={"/people/" (a.id) "/setup-link"} { button.small { "New setup link" } }
                        form.inline method="post" action={"/people/" (a.id) "/discord"} {
                            input name="discord_id" placeholder="Discord user id" style="width:11rem";
                            button.small { "Link Discord" }
                        }
                    } @else {
                        form.inline method="post" action={"/people/" (a.id) "/tokens"} {
                            input name="name" placeholder="what uses it" required style="width:11rem";
                            button.small { "New API token" }
                        }
                    }
                    @if a.disabled_at.is_some() {
                        form.inline method="post" action={"/people/" (a.id) "/enable"} { button.small { "Enable" } }
                    } @else {
                        form.inline method="post" action={"/people/" (a.id) "/disable"} { button.small { "Disable" } }
                    }
                    }
                }
            }
            div.grid {
                form.card method="post" action="/people" {
                    strong { "Add a person" }
                    input type="hidden" name="kind" value="human";
                    label { "Name" input name="display_name" required; }
                    label { "Email (they sign in with it)" input name="email" type="email" required; }
                    (role_boxes(&[Role::Volunteer]))
                    button { "Add and make a setup link" }
                }
                form.card method="post" action="/people" {
                    strong { "Add a service account" }
                    p.muted { "For scripts and other systems calling the API. It gets the service role only." }
                    input type="hidden" name="kind" value="service";
                    input type="hidden" name="role" value="service";
                    label { "Name" input name="display_name" required placeholder="Weather station"; }
                    button { "Add" }
                }
            }
            h2 { "API tokens" }
            @if tokens.is_empty() { p.muted { "None." } } @else {
                table {
                    tr { th { "Name" } th { "Account" } th { "Last used" } th {} }
                    @for t in &tokens {
                        tr {
                            td { (t.name) } td { (t.owner) }
                            td.muted { @if let Some(u) = t.last_used_at { (u.date()) } @else { "never" } }
                            td { @if t.revoked_at.is_some() { span.muted { "revoked" } } @else {
                                form.inline method="post" action={"/tokens/" (t.id) "/revoke"} { button.small { "Revoke" } }
                            } }
                        }
                    }
                }
            }
        },
    )
    .into_response())
}

/// Shows a secret exactly once. Nothing stores it in the clear.
fn reveal(title: &str, what: &str, secret: &str) -> UiResult {
    Ok((
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        layout(
            title,
            html! {
                p { (what) }
                p { code { (secret) } }
                p.warn { "This is shown once. Copy it now; if it's lost, make a new one." }
                p { a href=(B) { "← People" } }
            },
        ),
    )
        .into_response())
}

/// Form posts repeat `role` once per ticked box.
fn form_pairs(body: &str) -> Vec<(String, String)> {
    form_urlencoded::parse(body.as_bytes()).into_owned().collect()
}

fn roles_from(pairs: &[(String, String)]) -> Result<Vec<Role>, UiError> {
    pairs.iter().filter(|(k, _)| k == "role").map(|(_, v)| v.parse::<Role>()).collect::<Result<_, _>>().back(B)
}

fn field(pairs: &[(String, String)], name: &str) -> String {
    pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap_or_default()
}

async fn add(c: Can<cap::SystemsAdmin>, body: String) -> UiResult {
    let pairs = form_pairs(&body);
    let kind = field(&pairs, "kind").parse::<ActorKind>().back(B)?;
    let email = field(&pairs, "email");
    let input = NewActor {
        display_name: field(&pairs, "display_name"),
        email: (kind == ActorKind::Human).then_some(email),
        kind,
        roles: roles_from(&pairs)?,
    };
    let actor = c.store.create_actor(input).await.back(B)?;
    if actor.kind == ActorKind::Service {
        return Ok(see(B));
    }
    issue_link(&c, actor.id, &actor.display_name).await
}

async fn issue_link(c: &Can<cap::SystemsAdmin>, id: Uuid, name: &str) -> UiResult {
    let (token, hash) = new_secret();
    c.store.issue_setup_link(id, &hash, SETUP_LINK_TTL).await.back(B)?;
    reveal(
        "Setup link",
        &format!(
            "Send this link to {name} (privately). It lets them set a password once, within {} hours. \
             Anyone who opens it first can use it.",
            SETUP_LINK_TTL.whole_hours()
        ),
        &format!("/setup/{token}"),
    )
}

async fn setup_link(c: Can<cap::SystemsAdmin>, Path(id): Path<Uuid>) -> UiResult {
    let a = c.store.get_actor(id).await.back(B)?;
    issue_link(&c, id, &a.display_name).await
}

async fn set_roles(c: Can<cap::SystemsAdmin>, Path(id): Path<Uuid>, body: String) -> UiResult {
    let roles = roles_from(&form_pairs(&body))?;
    c.store.set_roles(id, &roles).await.back(B)?;
    Ok(see(B))
}

async fn disable(c: Can<cap::SystemsAdmin>, Path(id): Path<Uuid>) -> UiResult {
    c.store.set_actor_disabled(id, true).await.back(B)?;
    Ok(see(B))
}

async fn enable(c: Can<cap::SystemsAdmin>, Path(id): Path<Uuid>) -> UiResult {
    c.store.set_actor_disabled(id, false).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct DiscordForm {
    #[serde(default)]
    discord_id: String,
}

async fn link_discord(c: Can<cap::SystemsAdmin>, Path(id): Path<Uuid>, Form(f): Form<DiscordForm>) -> UiResult {
    let raw = f.discord_id.trim();
    if !raw.is_empty() && !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(UiError::invalid("A Discord user id is a long number (Developer Mode → Copy User ID).", B));
    }
    let subject = (!raw.is_empty()).then(|| format!("discord:{raw}"));
    c.store.set_external_subject(id, subject.as_deref()).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct TokenForm {
    name: String,
}

async fn new_token(c: Can<cap::SystemsAdmin>, Path(id): Path<Uuid>, Form(f): Form<TokenForm>) -> UiResult {
    let (token, hash) = new_secret();
    c.store.create_api_token(id, &f.name, &hash).await.back(B)?;
    reveal("API token", "Use it as `Authorization: Bearer <token>` on /api/v1.", &token)
}

async fn revoke_token(c: Can<cap::SystemsAdmin>, Path(id): Path<Uuid>) -> UiResult {
    c.store.revoke_api_token(id).await.back(B)?;
    Ok(see(B))
}

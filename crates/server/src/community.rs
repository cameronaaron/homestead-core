//! Community events and volunteer hours.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Router};
use homestead_domain::community::{
    Availability, EventEdit, EventKind, EventStatus, NewCommunityEvent, NewVolunteerShift, WorkingGroup,
};
use homestead_domain::reports::FiscalYear;
use homestead_integrations::buffer::Social;
use homestead_integrations::discord::feed::Snapshot;
use maud::{Markup, html};
use rust_decimal::Decimal;
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Can, cap};
use crate::ui::{Back, UiError, UiResult, ago, decimal, layout, options, req_date, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/community", get(page))
        .route("/community/events", post(create_event))
        .route("/community/events/{id}", get(event_page).post(edit_event))
        .route("/community/events/{id}/status", post(event_status))
        .route("/community/events/{id}/rsvps", post(event_rsvp))
        .route("/community/events/{id}/tasks", post(event_task))
        .route("/community/events/{id}/inventory", post(event_inventory))
        .route("/community/events/{id}/equipment", post(event_equipment))
        .route("/community/events/{id}/{kind}/{target}/remove", post(remove_event_resource))
        .route("/community/events/{id}/attendance", post(attendance))
        .route("/community/hours", post(log_hours))
        .route("/community/volunteers", get(volunteers_page))
        .route("/community/volunteers/{id}", post(edit_volunteer))
        .route("/community/volunteers/{id}/merge", post(merge_volunteer))
}

async fn page(c: Can<cap::CommunityRead>, State(s): State<AppState>) -> UiResult {
    const B: &str = "/community";
    let (discord, social) = tokio::join!(async { Some(s.discord_feed.as_ref()?.read().await) }, async {
        Some(s.social_feed.as_ref()?.read().await)
    },);
    let now = OffsetDateTime::now_utc();
    let today = c.store.today().await.back(B)?;
    let fy = FiscalYear::containing(today).first_day();
    let events = c.store.list_community_events(today).await.back(B)?;
    let hours = c.store.volunteer_hours(fy, today).await.back(B)?;
    let shifts = c.store.recent_volunteer_shifts(25).await.back(B)?;
    let total: Decimal = hours.iter().map(|h| h.hours).sum();
    let on_site = c.store.on_site().await.back(B)?;
    let names: Vec<String> = if c.who.can(homestead_domain::auth::Capability::CommunityWrite) {
        c.store.list_volunteers(fy, today).await.back(B)?.into_iter().filter(|v| v.0.active).map(|v| v.0.name).collect()
    } else {
        Vec::new()
    };
    let (upcoming, past): (Vec<_>, Vec<_>) = events.iter().partition(|e| e.held_on >= today);
    Ok(layout(
        "Community",
        html! {
            h2 { "On site now" }
            @if on_site.is_empty() {
                p.muted { "Nobody is signed in at the gate. Print the gate's sign-in tag from " a href="/tags?kind=gate" { "QR tags" } "." }
            } @else {
                table {
                    tr { th { "Who" } th { "Since" } th { "Here for" } th { "Waiver" } }
                    @for (v, waiver) in &on_site {
                        tr {
                            td { (v.person_name) }
                            td { (crate::ui::ago(now - v.in_at)) }
                            td { (v.working_group.label()) }
                            td class=[(!waiver).then_some("warn")] { @if *waiver { "on file" } @else { "none on file under this name" } }
                        }
                    }
                }
            }
            h2 { "Coming up" }
            @if upcoming.is_empty() { p.muted { "Nothing scheduled." } } @else {
                table {
                    tr { th { "When" } th { "What" } th { "Where" } }
                    @for e in &upcoming {
                        tr {
                            td { (crate::ui::Day(e.held_on)) @if !e.time_note.is_empty() { br; span.muted { (e.time_note) } } }
                            td { a href={"/community/events/" (e.id)} { (e.title) } " " span.pill { (e.kind) } @if !e.description.is_empty() { br; span.muted { (e.description) } } }
                            td { (e.place) }
                        }
                    }
                }
            }
            @if let Some(d) = &discord { (discord_section(d, now)) }
            @if let Some(s) = &social { (social_section(s, now)) }
            @if c.who.can(homestead_domain::auth::Capability::CommunityWrite) {
                form.card method="post" action="/community/events" {
                    strong { "Add an event" }
                    label { "Title" input name="title" required placeholder="Work day: coop build"; }
                    label { "Kind" select name="kind" { (options(EventKind::ALL, Some(EventKind::WorkDay))) } }
                    label { "Date" input name="held_on" type="date" required; }
                    label { "Time" input name="time_note" placeholder="1pm–4pm"; }
                    label { "Where" input name="place" placeholder="The homestead"; }
                    label { "Details" textarea name="description" rows="2" {} }
                    button { "Add" }
                }
            }
            h2 { "Volunteer hours this fiscal year (since " (fy) ")" }
            p { a href="/community/volunteers" { "Volunteers" } ": who they are, their hours, and merging two names for one person." }
            div.grid {
                div.card { div.stat { (total.normalize()) } div.muted { "hours in total" } }
                @for h in &hours {
                    div.card { div.stat { (h.hours.normalize()) } div.muted { (h.working_group.label()) " · " (h.people) " people" } }
                }
            }
            @if c.who.can(homestead_domain::auth::Capability::CommunityWrite) {
                form.card method="post" action="/community/hours" {
                    strong { "Log volunteer hours" }
                    label { "Who" input name="person_name" required maxlength="80" list="volunteer-names" autocomplete="off"; }
                    datalist id="volunteer-names" { @for n in &names { option value=(n) {} } }
                    label { "Date" input name="worked_on" type="date" required; }
                    label { "Hours" input name="hours" inputmode="decimal" required placeholder="3.5"; }
                    label { "Working group" select name="working_group" {
                        @for g in WorkingGroup::ALL { option value=(g) selected[*g == WorkingGroup::FarmLabor] { (g.label()) } }
                    } }
                    label { "At an event (optional)" select name="event_id" {
                        option value="" { "—" }
                        @for e in events.iter().take(40) { option value=(e.id) { (e.held_on) " · " (e.title) } }
                    } }
                    label { "What they did" input name="notes"; }
                    button { "Log" }
                }
            }
            @if !shifts.is_empty() {
                h2 { "Recent shifts" }
                table {
                    tr { th { "Date" } th { "Who" } th { "Hours" } th { "Group" } }
                    @for v in &shifts {
                        tr { td { (crate::ui::Day(v.worked_on)) } td { (v.person_name) @if !v.notes.is_empty() { br; span.muted { (v.notes) } } } td { (v.hours.normalize()) } td { (v.working_group.label()) } }
                    }
                }
            }
            @if !past.is_empty() {
                h2 { "Past events" }
                table {
                    tr { th { "When" } th { "What" } th { "Attendance" } }
                    @for e in &past {
                        tr {
                            td { (crate::ui::Day(e.held_on)) } td { a href={"/community/events/" (e.id)} { (e.title) } " " span.pill { (e.kind) } @if e.status == EventStatus::Cancelled { " " span.pill.low { "cancelled" } } }
                            td {
                                form.inline method="post" action={"/community/events/" (e.id) "/attendance"} {
                                    input name="attendance" type="number" min="0" style="width:5rem" value=[e.attendance];
                                    button.small { "Save" }
                                }
                            }
                        }
                    }
                }
            }
        },
    )
    .into_response())
}

async fn event_page(c: Can<cap::CommunityRead>, Path(id): Path<Uuid>) -> UiResult {
    let plan = c.store.event_plan(id).await.back("/community")?;
    let can_write = c.who.can(homestead_domain::auth::Capability::CommunityWrite);
    let (volunteers, tasks, inventory, equipment) = if can_write {
        (
            c.store.active_volunteer_names().await.back("/community")?,
            c.store.list_open_tasks().await.back("/community")?,
            c.store.list_inventory().await.back("/community")?,
            c.store.list_equipment().await.back("/community")?,
        )
    } else {
        (Vec::new(), Vec::new(), Vec::new(), Vec::new())
    };
    let attending = plan.rsvps.iter().filter(|r| r.availability == Availability::Attending).count();
    let maybe = plan.rsvps.iter().filter(|r| r.availability == Availability::Maybe).count();
    Ok(layout(
        &plan.event.title,
        html! {
            p { a href="/community" { "← Community" } }
            p.lede {
                (crate::ui::Day(plan.event.held_on))
                @if !plan.event.time_note.is_empty() { " · " (plan.event.time_note) }
                @if !plan.event.place.is_empty() { " · " (plan.event.place) }
                " " span.pill { (plan.event.kind) }
                @if plan.event.status == EventStatus::Cancelled { " " span.pill.low { "cancelled" } }
            }
            @if !plan.event.description.is_empty() { p { (plan.event.description) } }

            h2 { "Availability" }
            p.muted { (attending) " attending · " (maybe) " maybe · " (plan.rsvps.len()) " responses" }
            @if plan.rsvps.is_empty() { p.muted { "No responses yet." } } @else {
                ul.work {
                    @for r in &plan.rsvps {
                        li { span { strong { (r.volunteer_name) } " · " (r.availability) @if !r.note.is_empty() { br; span.muted { (r.note) } } } }
                    }
                }
            }

            h2 { "Tasks" }
            @if plan.tasks.is_empty() { p.muted { "No tasks linked." } } @else {
                ul.work { @for t in &plan.tasks {
                    li { span { (t.title) @if let Some(d) = t.due_on { " · " span.muted { (crate::ui::Day(d)) } } } @if can_write { (remove_button(id, "task", t.id)) } }
                } }
            }

            h2 { "Supplies" }
            @if plan.inventory.is_empty() { p.muted { "No inventory required." } } @else {
                ul.work { @for i in &plan.inventory {
                    li { span { strong { (i.quantity.normalize()) " " (i.unit) " " (i.name) } " · " @if i.on_hand >= i.quantity { span.ok { (i.on_hand.normalize()) " on hand" } } @else { span.warn { "only " (i.on_hand.normalize()) " on hand" } } @if !i.note.is_empty() { br; span.muted { (i.note) } } } @if can_write { (remove_button(id, "inventory", i.item_id)) } }
                } }
            }

            h2 { "Equipment" }
            @if plan.equipment.is_empty() { p.muted { "No equipment required." } } @else {
                ul.work { @for e in &plan.equipment {
                    li { span { strong { (e.name) } @if !e.available { " " span.warn { "not in service" } } @if !e.note.is_empty() { br; span.muted { (e.note) } } } @if can_write { (remove_button(id, "equipment", e.equipment_id)) } }
                } }
            }

            @if can_write {
                h2 { "Plan this event" }
                div.grid {
                    form.card method="post" action={"/community/events/" (id) "/rsvps"} {
                        strong { "Add availability" }
                        label { "Volunteer" select name="volunteer_id" required { @for (vid, name) in &volunteers { option value=(vid) { (name) } } } }
                        label { "Availability" select name="availability" { (options(Availability::ALL, Some(Availability::Attending))) } }
                        label { "Note" input name="note" placeholder="Can arrive after 2pm"; }
                        button.small { "Save response" }
                    }
                    form.card method="post" action={"/community/events/" (id) "/tasks"} {
                        strong { "Link a task" }
                        label { "Task" select name="target_id" required { @for t in &tasks { option value=(t.id) { (t.title) } } } }
                        button.small { "Link task" }
                    }
                    form.card method="post" action={"/community/events/" (id) "/inventory"} {
                        strong { "Require supplies" }
                        label { "Item" select name="target_id" required { @for i in &inventory { option value=(i.id) { (i.name) " (" (i.on_hand.normalize()) " " (i.unit) " on hand)" } } } }
                        label { "Quantity" input name="quantity" inputmode="decimal" required; }
                        label { "Note" input name="note"; }
                        button.small { "Add supply" }
                    }
                    form.card method="post" action={"/community/events/" (id) "/equipment"} {
                        strong { "Require equipment" }
                        label { "Equipment" select name="target_id" required { @for e in &equipment { option value=(e.id) { (e.name) " · " (e.status) } } } }
                        label { "Note" input name="note"; }
                        button.small { "Add equipment" }
                    }
                }
                details.card {
                    summary { "Edit, reschedule, or cancel" }
                    form method="post" action={"/community/events/" (id)} {
                        label { "Title" input name="title" required value=(plan.event.title); }
                        label { "Kind" select name="kind" { (options(EventKind::ALL, Some(plan.event.kind))) } }
                        label { "Date" input name="held_on" type="date" required value=(plan.event.held_on); }
                        label { "Time" input name="time_note" value=(plan.event.time_note); }
                        label { "Where" input name="place" value=(plan.event.place); }
                        label { "Details" textarea name="description" rows="3" { (plan.event.description) } }
                        button.small { "Save changes" }
                    }
                    form.row method="post" action={"/community/events/" (id) "/status"} {
                        @if plan.event.status == EventStatus::Scheduled {
                            input type="hidden" name="status" value="cancelled";
                            button.small.secondary { "Cancel event" }
                        } @else {
                            input type="hidden" name="status" value="scheduled";
                            button.small { "Restore event" }
                        }
                    }
                }
            }
        },
    )
    .into_response())
}

fn remove_button(event_id: Uuid, kind: &str, target: Uuid) -> Markup {
    html! { form method="post" action={"/community/events/" (event_id) "/" (kind) "/" (target) "/remove"} { button.small.secondary aria-label={"Remove " (kind)} { "Remove" } } }
}

/// Upcoming Discord events and the latest posts in the feed channels.
fn discord_section(read: &Result<Arc<Snapshot>, String>, now: OffsetDateTime) -> Markup {
    html! {
        h2 { "On Discord" }
        @match read {
            Err(e) => p.warn { "Couldn't read Discord: " (e) },
            Ok(d) => {
                @if !d.events.is_empty() {
                    table {
                        caption.visually-hidden { "Discord events" }
                        tr { th scope="col" { "When" } th scope="col" { "What" } th scope="col" { "Where" } }
                        @for e in &d.events {
                            tr {
                                td { (e.starts_at.date()) }
                                td {
                                    a href=(e.url) target="_blank" rel="noopener" { (e.name) }
                                    @if let Some(n) = e.interested { span.muted { " · " (n) " interested" } }
                                    @if !e.description.is_empty() { br; span.muted { (e.description) } }
                                }
                                td { (e.place) }
                            }
                        }
                    }
                }
                @if !d.posts.is_empty() {
                    ul.work {
                        @for post in d.posts.iter().take(8) {
                            li {
                                span {
                                    strong { (post.author) } span.muted { " in #" (post.channel) " · " (ago(now - post.at)) }
                                    br; (post.excerpt)
                                }
                                a href=(post.url) target="_blank" rel="noopener" aria-label={"Open " (post.author) "'s post in Discord"} { "Open" }
                            }
                        }
                    }
                }
                @if !d.quiet.is_empty() {
                    p.muted {
                        "Nothing posted yet in "
                        @for (i, ch) in d.quiet.iter().enumerate() { @if i > 0 { ", " } "#" (ch) }
                        "."
                        @if d.events.is_empty() { " No events scheduled on Discord." }
                    }
                } @else if d.events.is_empty() { p.muted { "No events scheduled on Discord." } }
            }
        }
    }
}

/// Buffer: which accounts are connected, what's queued and posted, and the
/// drafts waiting for a photo.
fn social_section(read: &Result<Arc<Social>, String>, now: OffsetDateTime) -> Markup {
    html! {
        h2 { "Social media" }
        @match read {
            Err(e) => p.warn { "Couldn't read Buffer: " (e) },
            Ok(s) => {
                @if s.channels.is_empty() {
                    p {
                        "No Instagram or Facebook account is connected in Buffer yet, so there's nothing posted or queued to show. "
                        a href="/ops/social" { "How to connect them" }
                    }
                } @else {
                    p.muted {
                        @for (i, ch) in s.channels.iter().enumerate() {
                            @if i > 0 { " · " }
                            (ch.service) ": " (ch.name) @if ch.paused { span.warn { " (queue paused)" } }
                        }
                    }
                    @if s.posts.is_empty() { p.muted { "Nothing queued or posted recently." } } @else {
                        table {
                            caption.visually-hidden { "Social posts" }
                            tr { th scope="col" { "Status" } th scope="col" { "Post" } th scope="col" { "When" } }
                            @for post in &s.posts {
                                tr {
                                    td { span class={"pill" @if post.status == "error" { " urgent" }} { (post.status.replace('_', " ")) } }
                                    td {
                                        span.muted { (post.service) @if !post.channel.is_empty() { " · " (post.channel) } } br;
                                        @if let Some(link) = &post.link { a href=(link) target="_blank" rel="noopener" { (post.excerpt) } } @else { (post.excerpt) }
                                        @if let Some(e) = &post.error { br; span.warn { (e) } }
                                    }
                                    td.muted { @if let Some(at) = post.at { @if at > now { (at.date()) } @else { (ago(now - at)) } } }
                                }
                            }
                        }
                    }
                }
                @if !s.drafts.is_empty() {
                    p {
                        strong { (s.drafts.len()) @if s.drafts.len() == 1 { " draft" } @else { " drafts" } }
                        " from harvests and plantings wait for a photo in Buffer → Ideas:"
                    }
                    ul { @for d in s.drafts.iter().take(6) { li { (d.title) } } }
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct EventForm {
    title: String,
    kind: String,
    held_on: String,
    #[serde(default)]
    time_note: String,
    #[serde(default)]
    place: String,
    #[serde(default)]
    description: String,
}

async fn create_event(c: Can<cap::CommunityWrite>, Form(f): Form<EventForm>) -> UiResult {
    const B: &str = "/community";
    let input = NewCommunityEvent {
        title: f.title,
        kind: f.kind.parse::<EventKind>().back(B)?,
        held_on: req_date(&f.held_on, "date", B)?,
        time_note: f.time_note,
        place: f.place,
        description: f.description,
    };
    c.store.create_community_event(input).await.back(B)?;
    Ok(see(B))
}

async fn edit_event(c: Can<cap::CommunityWrite>, Path(id): Path<Uuid>, Form(f): Form<EventForm>) -> UiResult {
    let back = format!("/community/events/{id}");
    c.store
        .update_community_event(
            id,
            EventEdit {
                title: f.title,
                kind: f.kind.parse::<EventKind>().back("/community")?,
                held_on: req_date(&f.held_on, "date", "/community")?,
                time_note: f.time_note,
                place: f.place,
                description: f.description,
            },
        )
        .await
        .back("/community")?;
    Ok(see(&back))
}

#[derive(Deserialize)]
struct StatusForm {
    status: String,
}

async fn event_status(c: Can<cap::CommunityWrite>, Path(id): Path<Uuid>, Form(f): Form<StatusForm>) -> UiResult {
    let back = format!("/community/events/{id}");
    c.store
        .set_community_event_status(id, f.status.parse::<EventStatus>().back("/community")?)
        .await
        .back("/community")?;
    Ok(see(&back))
}

#[derive(Deserialize)]
struct RsvpForm {
    volunteer_id: String,
    availability: String,
    #[serde(default)]
    note: String,
}

async fn event_rsvp(c: Can<cap::CommunityWrite>, Path(id): Path<Uuid>, Form(f): Form<RsvpForm>) -> UiResult {
    let back = format!("/community/events/{id}");
    let volunteer = f.volunteer_id.parse::<Uuid>().map_err(|_| UiError::invalid("unknown volunteer", "/community"))?;
    let availability = f.availability.parse::<Availability>().back("/community")?;
    c.store.set_event_rsvp(id, volunteer, availability, &f.note).await.back("/community")?;
    Ok(see(&back))
}

#[derive(Deserialize)]
struct LinkForm {
    target_id: String,
    #[serde(default)]
    quantity: String,
    #[serde(default)]
    note: String,
}

fn link_target(f: &LinkForm, back: &'static str) -> Result<Uuid, UiError> {
    f.target_id.parse::<Uuid>().map_err(|_| UiError::invalid("unknown resource", back))
}

async fn event_task(c: Can<cap::CommunityWrite>, Path(id): Path<Uuid>, Form(f): Form<LinkForm>) -> UiResult {
    let back = format!("/community/events/{id}");
    c.store.link_event_task(id, link_target(&f, "/community")?).await.back("/community")?;
    Ok(see(&back))
}

async fn event_inventory(c: Can<cap::CommunityWrite>, Path(id): Path<Uuid>, Form(f): Form<LinkForm>) -> UiResult {
    let back = format!("/community/events/{id}");
    c.store
        .link_event_inventory(
            id,
            link_target(&f, "/community")?,
            decimal(&f.quantity, "quantity", "/community")?,
            &f.note,
        )
        .await
        .back("/community")?;
    Ok(see(&back))
}

async fn event_equipment(c: Can<cap::CommunityWrite>, Path(id): Path<Uuid>, Form(f): Form<LinkForm>) -> UiResult {
    let back = format!("/community/events/{id}");
    c.store.link_event_equipment(id, link_target(&f, "/community")?, &f.note).await.back("/community")?;
    Ok(see(&back))
}

async fn remove_event_resource(
    c: Can<cap::CommunityWrite>,
    Path((id, kind, target)): Path<(Uuid, String, Uuid)>,
) -> UiResult {
    let back = format!("/community/events/{id}");
    c.store.unlink_event_resource(id, &kind, target).await.back("/community")?;
    Ok(see(&back))
}

#[derive(Deserialize)]
struct AttendanceForm {
    attendance: String,
}

async fn attendance(c: Can<cap::CommunityWrite>, Path(id): Path<Uuid>, Form(f): Form<AttendanceForm>) -> UiResult {
    const B: &str = "/community";
    let n: i32 = f.attendance.trim().parse().map_err(|_| UiError::invalid("attendance: expected a whole number", B))?;
    c.store.set_attendance(id, n).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct HoursForm {
    person_name: String,
    worked_on: String,
    hours: String,
    working_group: String,
    #[serde(default)]
    event_id: String,
    #[serde(default)]
    notes: String,
}

async fn log_hours(c: Can<cap::CommunityWrite>, Form(f): Form<HoursForm>) -> UiResult {
    const B: &str = "/community";
    let event_id = match f.event_id.trim() {
        "" => None,
        id => Some(id.parse::<Uuid>().map_err(|_| UiError::invalid("unknown event", B))?),
    };
    let input = NewVolunteerShift {
        person_name: f.person_name,
        worked_on: req_date(&f.worked_on, "date", B)?,
        hours: decimal(&f.hours, "hours", B)?,
        working_group: f.working_group.parse::<WorkingGroup>().back(B)?,
        event_id,
        notes: f.notes,
    };
    c.store.log_volunteer_shift(input).await.back(B)?;
    Ok(see(B))
}

// ── Volunteers ───────────────────────────────────────────────────────────────

async fn volunteers_page(c: Can<cap::CommunityRead>) -> UiResult {
    const B: &str = "/community/volunteers";
    let today = c.store.today().await.back(B)?;
    let fy = FiscalYear::containing(today).first_day();
    let all = c.store.list_volunteers(fy, today).await.back(B)?;
    // Email and phone are for whoever manages volunteers, as in the audit log.
    let manage = c.who.can(homestead_domain::auth::Capability::CommunityWrite);
    Ok(layout(
        "Volunteers",
        html! {
            p.muted {
                "Everyone who has logged hours or signed in at the gate. Hours are this fiscal year (since " (crate::ui::Day(fy)) "). "
                "A name typed at the gate or in the hours form finds the volunteer who goes by it, and a new name adds a volunteer. "
                "If one person ended up as two (\"Sam\" and \"Sam R.\"), merge one into the other: "
                "the hours move, and the old name keeps finding them."
            }
            @if all.is_empty() { p.muted { "No volunteers yet." } } @else {
                table {
                    caption.visually-hidden { "Volunteers" }
                    tr { th scope="col" { "Name" } th scope="col" { "Hours" } th scope="col" { "Last worked" } @if manage { th scope="col" { "Details" } } }
                    @for (v, hours, last) in &all {
                        tr {
                            td {
                                (v.name) @if !v.active { " " span.pill { "inactive" } }
                                @if !v.other_names.is_empty() { br; span.muted { "also “" (v.other_names.join("”, “")) "”" } }
                            }
                            td { (hours.normalize()) }
                            td.muted { @if let Some(d) = last { (crate::ui::Day(*d)) } }
                            @if manage { td { (volunteer_forms(v, &all)) } }
                        }
                    }
                }
            }
        },
    )
    .into_response())
}

fn volunteer_forms(
    v: &homestead_domain::community::Volunteer,
    all: &[(homestead_domain::community::Volunteer, Decimal, Option<time::Date>)],
) -> Markup {
    html! {
        @if let Some(e) = &v.email { a href={"mailto:" (e)} { (e) } br; }
        @if !v.phone.is_empty() { (v.phone) br; }
        details.reassign {
            summary { "Edit" }
            form method="post" action={"/community/volunteers/" (v.id)} {
                label { "Name" input name="name" required maxlength="80" value=(v.name); }
                label { "Email" input name="email" type="email" value=[v.email.as_deref()]; }
                label { "Phone" input name="phone" type="tel" maxlength="40" value=(v.phone); }
                label.inline { input type="checkbox" name="active" checked[v.active]; "Still volunteering" }
                button.small { "Save" }
            }
            @if all.len() > 1 {
                form method="post" action={"/community/volunteers/" (v.id) "/merge"} {
                    label { "Same person as" select name="into" required {
                        option value="" { "—" }
                        @for (o, _, _) in all.iter().filter(|o| o.0.id != v.id) { option value=(o.id) { (o.name) } }
                    } }
                    button.small.secondary { "Merge " (v.name) " into them" }
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct VolunteerForm {
    name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    phone: String,
    active: Option<String>,
}

async fn edit_volunteer(c: Can<cap::CommunityWrite>, Path(id): Path<Uuid>, Form(f): Form<VolunteerForm>) -> UiResult {
    const B: &str = "/community/volunteers";
    let edit = homestead_domain::community::VolunteerEdit {
        name: f.name,
        email: Some(f.email),
        phone: f.phone,
        active: f.active.is_some(),
    };
    c.store.update_volunteer(id, edit).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct MergeForm {
    into: String,
}

async fn merge_volunteer(c: Can<cap::CommunityWrite>, Path(id): Path<Uuid>, Form(f): Form<MergeForm>) -> UiResult {
    const B: &str = "/community/volunteers";
    let into = crate::ui::opt_uuid(&f.into, B)?.ok_or_else(|| UiError::invalid("choose who they are", B))?;
    c.store.merge_volunteers(id, into).await.back(B)?;
    Ok(see(B))
}

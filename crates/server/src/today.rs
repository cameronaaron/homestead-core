//! The day's work, and the few records someone logs without leaving it.

use std::collections::{HashMap, HashSet};

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Router};
use homestead_domain::auth::Capability;
use homestead_domain::crops::Due;
use homestead_domain::incidents::IncidentStatus;
use homestead_domain::water::{IrrigationMethod, MANUAL, NewIrrigation};
use homestead_domain::{HarvestUnit, NewHarvest, Task, TaskPriority};
use homestead_store::field::FieldMemory;
use homestead_store::linear::IssueLink;
use maud::{Markup, html};
use rust_decimal::Decimal;
use serde::Deserialize;
use time::{Date, Duration, OffsetDateTime};
use uuid::Uuid;

use crate::auth::{Authed, Can, cap};
use crate::ui::{Back, UiError, UiResult, decimal, layout, opt_uuid, options, see};
use crate::{AppState, run_schedules};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(page))
        .route("/today/water", post(water))
        .route("/today/harvest", post(harvest))
        .route("/today/eggs", post(eggs))
        .route("/today/loss", post(loss))
        .route("/today/checklist", post(start_list))
        .route("/today/check", post(tick))
}

const B: &str = "/";

async fn page(a: Authed, State(s): State<AppState>) -> UiResult {
    use Capability as C;
    let (who, store) = (&a.who, &a.store);
    if who.can(C::TasksRead) {
        run_schedules(&s).await;
        crate::linear_pull::refresh(&s).await;
    }
    let today = store.today().await.back(B)?;
    let tasks = if who.can(C::TasksRead) { store.list_open_tasks().await.back(B)? } else { Vec::new() };
    let links = if who.can(C::TasksRead) { store.open_task_issue_links().await.back(B)? } else { HashMap::new() };
    let linear = match (&s.linear_pull, who.can(C::TasksRead)) {
        (Some(_), true) => Some(store.linear_sync_status().await.back(B)?),
        _ => None,
    };
    let water_fixes = if who.can(C::GardenRead) { store.open_irrigation_problems().await.back(B)? } else { Vec::new() };
    let crops = if who.can(C::GardenRead) { Some(store.crop_agenda().await.back(B)?) } else { None };
    let (low, soon) = if who.can(C::InventoryRead) {
        let paces = store.usage_paces().await.back(B)?;
        let (low, rest): (Vec<_>, Vec<_>) = store.list_inventory().await.back(B)?.into_iter().partition(|i| i.is_low());
        let soon: Vec<_> = rest
            .into_iter()
            .filter_map(|i| {
                let d = paces.get(&i.id)?.days_left(i.on_hand)?;
                (d <= homestead_domain::inventory::RUNS_OUT_SOON_DAYS).then_some((i, d))
            })
            .collect();
        (low, soon)
    } else {
        (Vec::new(), Vec::new())
    };
    let care_due =
        if who.can(C::AnimalsMedical) { store.care_schedules(None, Some(today)).await.back(B)? } else { Vec::new() };
    let crowded = if who.can(C::AnimalsRead) {
        store.housing().await.back(B)?.into_iter().filter(|h| h.over_capacity()).collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let incidents = if who.can(C::IncidentsRead) || who.can(C::IncidentsReport) {
        let mine = (!who.can(C::IncidentsRead)).then_some(who.actor.id);
        store
            .list_incidents(mine, 40)
            .await
            .back(B)?
            .into_iter()
            .filter(|i| i.status == IncidentStatus::Open)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let notices = store.notices_for(who.actor.id, who.caps, 6).await.back(B)?;

    let held: HashSet<Uuid> = water_fixes
        .iter()
        .map(|(_, id)| *id)
        .chain(crops.iter().flat_map(|a| a.transplants.iter().map(|t| t.0)))
        .collect();
    let names = if who.can(C::TasksRead) { store.person_names().await.back(B)? } else { HashMap::new() };
    // Your own tasks come first and aren't repeated below.
    let (yours, others): (Vec<Task>, Vec<Task>) =
        tasks.into_iter().filter(|t| !held.contains(&t.id)).partition(|t| t.assigned_to == Some(who.actor.id));
    let (overdue, due, later) = split_tasks(&others, today, &held);

    let places = if who.can(C::GardenWrite) { store.list_locations().await.back(B)? } else { Vec::new() };
    let plantings = if who.can(C::GardenWrite) { store.list_active_plantings().await.back(B)? } else { Vec::new() };
    let flocks = if who.can(C::AnimalsWrite) { store.list_animals().await.back(B)? } else { Vec::new() };
    let checks = if who.can(C::TasksRead) { store.checklist_items(today).await.back(B)? } else { Vec::new() };
    let templates = if who.can(C::TasksWrite) { store.list_checklist_templates().await.back(B)? } else { Vec::new() };
    let obligations = if who.can(C::TasksRead) {
        store.open_obligations(today + Duration::days(7)).await.back(B)?
    } else {
        Vec::new()
    };
    let service = if who.can(C::EquipmentRead) {
        store.list_equipment().await.back(B)?.into_iter().filter(|e| e.service_due(today)).collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let memory = if who.can(C::GardenWrite) || who.can(C::AnimalsWrite) {
        store.field_memory(who.actor.id).await.back(B)?
    } else {
        FieldMemory::default()
    };

    Ok(layout(
        "Today",
        html! {
            p.muted.lede { (today.format(time::macros::format_description!("[weekday], [month repr:long] [day padding:none]")).unwrap_or_else(|_| today.to_string())) }
            @if !notices.is_empty() {
                h2 { "Since you last looked" }
                ul.notices { @for n in &notices { li { (crate::ui::chat(n)) } } }
            }
            @if !yours.is_empty() {
                h2 { "Yours" }
                (work_list(&yours.iter().collect::<Vec<_>>(), today, &links, &names))
            }
            @if !overdue.is_empty() {
                h2 { "Overdue" }
                (work_list(&overdue, today, &links, &names))
            }
            @if !due.is_empty() {
                h2 { "Due today" }
                (work_list(&due, today, &links, &names))
            }
            @if let Some(agenda) = &crops {
                @if !agenda.plantings.is_empty() || !agenda.transplants.is_empty() {
                    h2 { "Crops" }
                    ul.work {
                        @for (p, place, due) in &agenda.plantings {
                            li { span { (crop_line(&p.crop, place, *due, p.sown_on)) } a href={"/plantings/" (p.id)} { "Open" } }
                        }
                        @for (task_id, due_on, p, place) in &agenda.transplants {
                            li {
                                span { "Transplant " (p.crop) " in " (place) @if *due_on < today { " · overdue" } @else if *due_on == today { " · due today" } }
                                (done(*task_id, &format!("Transplant {}", p.crop)))
                            }
                        }
                    }
                }
            }
            @if !water_fixes.is_empty() {
                h2 { "Irrigation" }
                ul.work {
                    @for (run, task) in &water_fixes {
                        li {
                            span {
                                "Fix " (run.problem.map(|p| p.label()).unwrap_or("a problem")) " at " (run.location_name)
                                @if !run.notes.is_empty() { span.muted { " · " (run.notes) } }
                            }
                            (done(*task, &format!("Fix irrigation at {}", run.location_name)))
                        }
                    }
                }
            }
            @if !care_due.is_empty() {
                h2 { "Care due" }
                ul.work { @for s in &care_due {
                    li {
                        span class=[(s.next_due_on < today).then_some("warn")] {
                            a href={"/animals/" (s.animal_id)} { (s.animal_name) }
                            @if s.animal_status == homestead_domain::animals::AnimalStatus::Fostered { " (with their foster)" }
                            ": " (s.kind) @if !s.details.is_empty() { " — " (s.details) }
                            @if s.next_due_on < today { " · due " (crate::ui::Day(s.next_due_on)) }
                        }
                        form.inline method="post" action={"/animals/" (s.animal_id) "/care"} {
                            input type="hidden" name="occurred_on" value=(today);
                            input type="hidden" name="kind" value=(s.kind);
                            input type="hidden" name="details" value=(s.details);
                            button.small { "Given" }
                        }
                    }
                } }
            }
            @if !crowded.is_empty() {
                h2 { "Over capacity" }
                ul.work { @for h in &crowded {
                    li { span.warn { (h.name) ": " (h.head) " living there, room for " (h.capacity.unwrap_or_default()) } a href="/animals" { "Animals" } }
                } }
            }
            @if !soon.is_empty() {
                h2 { "Runs out within a week" }
                ul.work { @for (i, d) in &soon {
                    li { span { (i.name) ": " (i.on_hand.normalize()) " " (i.unit) ", " (crate::ui::lasts(Some(*d))) " at this pace" }
                         a href={"/inventory/" (i.id)} { "Item" } }
                } }
            }
            @if !low.is_empty() {
                h2 { "Running low" }
                ul.work { @for i in &low { li { span { (i.name) ": " (i.on_hand.normalize()) " " (i.unit) } a href="/inventory" { "Inventory" } } } }
            }
            @if !incidents.is_empty() {
                h2 { "Open incidents" }
                ul.work {
                    @for i in &incidents {
                        li {
                            span {
                                a href={"/incidents/" (i.id)} { (i.reference()) }
                                " · " (i.kind.label())
                                @if i.follow_up_by.is_some_and(|d| d < today) { " · follow-up overdue" }
                            }
                        }
                    }
                }
            }
            @if !service.is_empty() {
                h2 { "Service due" }
                ul.work {
                    @for e in &service {
                        li {
                            span { (e.name) @if let Some(d) = e.next_service_on { span.muted { " · " (crate::ui::Day(d)) } } }
                            a href={"/equipment/" (e.id)} { "Open" }
                        }
                    }
                }
            }
            @if !obligations.is_empty() {
                h2 { "Obligations" }
                ul.work {
                    @for o in &obligations {
                        li {
                            span {
                                (o.requirement)
                                @if !o.authority.is_empty() { span.muted { " · " (o.authority) } }
                                @if let Some(d) = o.due_on { span.muted { " · " (crate::ui::Day(d)) } }
                            }
                            a href="/farm" { "Farm" }
                        }
                    }
                }
            }
            @if !later.is_empty() {
                h2 { "Coming up" }
                (work_list(&later[..later.len().min(8)], today, &links, &names))
                @if later.len() > 8 { p { a href="/tasks" { "All " (later.len()) " open tasks" } } }
            }
            @if let Some(status) = &linear {
                (crate::linear_pull::status_line(status.as_ref(), OffsetDateTime::now_utc()))
            }
            (field_forms(Field {
                garden: who.can(C::GardenWrite),
                animals: who.can(C::AnimalsWrite),
                tasks: who.can(C::TasksWrite),
                places: &places,
                plantings: &plantings,
                flocks: &flocks,
                checks: &checks,
                templates: &templates,
                memory: &memory,
            }))
        },
    )
    .into_response())
}

fn split_tasks<'a>(
    tasks: &'a [Task],
    today: Date,
    held: &HashSet<Uuid>,
) -> (Vec<&'a Task>, Vec<&'a Task>, Vec<&'a Task>) {
    let mut overdue = Vec::new();
    let mut due = Vec::new();
    let mut later = Vec::new();
    for t in tasks.iter().filter(|t| !held.contains(&t.id)) {
        match t.due_on {
            Some(d) if d < today => overdue.push(t),
            Some(d) if d == today => due.push(t),
            _ => later.push(t),
        }
    }
    (overdue, due, later)
}

fn crop_line(crop: &str, place: &str, due: Due, sown_on: Date) -> String {
    match due {
        Due::SowOverdue => format!("Sow {crop} in {place}, overdue (planned {sown_on})"),
        Due::SowThisWeek => format!("Sow {crop} in {place} on {sown_on}"),
        Due::HarvestOverdue => format!("Harvest {crop} in {place}, overdue"),
        Due::HarvestSoon => format!("Harvest {crop} in {place}"),
    }
}

fn work_list(tasks: &[&Task], today: Date, links: &HashMap<Uuid, IssueLink>, names: &HashMap<Uuid, String>) -> Markup {
    html! {
        ul.work {
            @for t in tasks {
                li {
                    span {
                        @if t.due_on.is_some_and(|d| d < today) { span.warn { "Overdue " } }
                        @if matches!(t.priority, TaskPriority::Urgent | TaskPriority::High) { span class={"pill " (t.priority)} { (t.priority) } " " }
                        (t.title)
                        @if let Some(d) = t.due_on { span.muted { " · " (crate::ui::Day(d)) } }
                        @if let Some(w) = crate::ui::who_has(t, names) { @if w != "you" { span.muted { " · " (w) } } }
                        (crate::linear_pull::chip(links.get(&t.id)))
                    }
                    (done(t.id, &t.title))
                }
            }
        }
    }
}

fn done(id: Uuid, title: &str) -> Markup {
    let can = crate::auth::VIEWER.try_with(|v| v.can(Capability::TasksWrite)).unwrap_or(false);
    html! {
        @if can {
            form method="post" action={"/tasks/" (id) "/complete"} {
                button type="submit" aria-label={"Mark done: " (title)} { "Done" }
            }
        }
    }
}

struct Field<'a> {
    garden: bool,
    animals: bool,
    tasks: bool,
    places: &'a [homestead_domain::Location],
    plantings: &'a [(homestead_domain::Planting, String)],
    flocks: &'a [homestead_domain::animals::Animal],
    checks: &'a [(Uuid, String, String, bool)],
    templates: &'a [(Uuid, String)],
    memory: &'a FieldMemory,
}

fn field_forms(f: Field<'_>) -> Markup {
    let Field { garden, animals, tasks, places, plantings, flocks, checks, templates, memory } = f;
    let open: Vec<_> = checks.iter().filter(|c| !c.3).collect();
    if !garden && !animals && !tasks {
        return html! {};
    }
    let again = memory.location_id.and_then(|id| places.iter().find(|l| l.id == id && l.active));
    let unit = memory.harvest_unit.as_deref().and_then(|u| u.parse().ok()).unwrap_or(HarvestUnit::Lb);
    html! {
        h2 id="log" { "Log it" }
        div.actions {
            @if garden {
                @if let Some(place) = again {
                    form.card method="post" action="/today/water" {
                        p { "Last time: " (place.name) }
                        input type="hidden" name="location_id" value=(place.id);
                        button type="submit" aria-label={"Watered " (place.name) " again"} { "Watered again" }
                    }
                }
                form.card method="post" action="/today/water" {
                    label { "Watered" select name="location_id" required {
                        option value="" { "Choose a place" }
                        @for l in places.iter().filter(|l| l.active) {
                            option value=(l.id) selected[memory.location_id == Some(l.id)] { (l.name) }
                        }
                    } }
                    button type="submit" { "Watered" }
                }
                @if !plantings.is_empty() {
                    form.card method="post" action="/today/harvest" {
                        label { "Harvest" select name="planting_id" required {
                            @for (p, loc) in plantings {
                                option value=(p.id) selected[memory.planting_id == Some(p.id)] { (p.crop) " — " (loc) }
                            }
                        } }
                        label { "Amount" input name="quantity" inputmode="decimal" required value=[memory.harvest_quantity.map(|q| q.normalize().to_string())]; }
                        label { "Unit" select name="unit" { (options(HarvestUnit::ALL, Some(unit))) } }
                        button type="submit" { "Log harvest" }
                    }
                }
                form.card method="post" action="/today/loss" {
                    label { "Loss" input name="reason" required placeholder="What was lost"; }
                    button type="submit" { "Record loss" }
                }
            }
            @if animals && !flocks.is_empty() {
                form.card method="post" action="/today/eggs" {
                    label { "Eggs from" select name="animal_id" required {
                        @for a in flocks {
                            option value=(a.id) selected[memory.animal_id == Some(a.id)] { (a.name) }
                        }
                    } }
                    label { "Collected" input name="collected" type="number" min="0" inputmode="numeric" required; }
                    button type="submit" { "Log eggs" }
                }
            }
            @if tasks {
                @if !open.is_empty() {
                    div.card {
                        strong { "Checklist" }
                        ul.work {
                            @for (id, name, label, _) in &open {
                                li {
                                    span { (name) ": " (label) }
                                    form method="post" action="/today/check" {
                                        input type="hidden" name="item_id" value=(id);
                                        button type="submit" aria-label={"Done: " (label)} { "Done" }
                                    }
                                }
                            }
                        }
                    }
                } @else if !templates.is_empty() {
                    form.card method="post" action="/today/checklist" {
                        label { "Checklist" select name="template_id" required {
                            @for (id, name) in templates { option value=(id) { (name) } }
                        } }
                        button type="submit" { "Start today's list" }
                    }
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct WaterForm {
    location_id: String,
}

async fn water(c: Can<cap::GardenWrite>, Form(f): Form<WaterForm>) -> UiResult {
    let location_id = opt_uuid(&f.location_id, B)?.ok_or_else(|| UiError::invalid("choose a place", B))?;
    c.store
        .record_irrigation(NewIrrigation {
            location_id: Some(location_id),
            planting_id: None,
            started_at: OffsetDateTime::now_utc(),
            ended_at: None,
            water_liters: None,
            method: IrrigationMethod::Hand,
            notes: String::new(),
            source: MANUAL.into(),
            external_id: None,
            zone_id: None,
            problem: None,
        })
        .await
        .back(B)?;
    c.store.remember_water(c.who.actor.id, location_id).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct HarvestForm {
    planting_id: String,
    quantity: String,
    unit: String,
}

async fn harvest(c: Can<cap::GardenWrite>, Form(f): Form<HarvestForm>) -> UiResult {
    let today = c.store.today().await.back(B)?;
    let input = NewHarvest {
        planting_id: f.planting_id.parse().map_err(|_| UiError::invalid("choose a planting", B))?,
        harvested_on: today,
        quantity: decimal(&f.quantity, "amount", B)?,
        unit: f.unit.parse::<HarvestUnit>().back(B)?,
        notes: String::new(),
    };
    let planting_id = input.planting_id;
    let quantity = input.quantity;
    let unit = input.unit.as_str().to_owned();
    c.store.log_harvest(input).await.back(B)?;
    c.store.remember_harvest(c.who.actor.id, planting_id, quantity, &unit).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct EggForm {
    animal_id: String,
    collected: String,
}

async fn eggs(c: Can<cap::AnimalsWrite>, Form(f): Form<EggForm>) -> UiResult {
    let n: i32 = f.collected.trim().parse().map_err(|_| UiError::invalid("collected: expected a number", B))?;
    if n < 0 {
        return Err(UiError::invalid("collected: expected a number", B));
    }
    let today = c.store.today().await.back(B)?;
    let animal_id = f.animal_id.parse().map_err(|_| UiError::invalid("choose a flock", B))?;
    c.store.record_eggs(animal_id, today, n, 0, 0, 0).await.back(B)?;
    c.store.remember_eggs(c.who.actor.id, animal_id).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct LossForm {
    reason: String,
}

async fn loss(c: Can<cap::GardenWrite>, Form(f): Form<LossForm>) -> UiResult {
    let today = c.store.today().await.back(B)?;
    c.store.record_loss(today, &f.reason, None::<Decimal>, "", None, None).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct ListForm {
    template_id: String,
}

async fn start_list(c: Can<cap::TasksWrite>, Form(f): Form<ListForm>) -> UiResult {
    let id = f.template_id.parse().map_err(|_| UiError::invalid("choose a checklist", B))?;
    let today = c.store.today().await.back(B)?;
    c.store.start_checklist(id, today, &c.who.actor.display_name).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct TickForm {
    item_id: String,
}

async fn tick(c: Can<cap::TasksWrite>, Form(f): Form<TickForm>) -> UiResult {
    let id = f.item_id.parse().map_err(|_| UiError::invalid("unknown checklist item", B))?;
    c.store.check_item(id, true, "").await.back(B)?;
    Ok(see(B))
}

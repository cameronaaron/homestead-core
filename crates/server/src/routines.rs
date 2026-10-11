//! Routines: recurring task templates, managed from the Tasks page.

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use homestead_domain::TaskPriority;
use homestead_domain::recurring::{NewTemplate, Schedule, Template, WeekdaySet};
use maud::{Markup, html};
use uuid::Uuid;

use crate::AppState;
use crate::api::{ApiResult, created};
use crate::auth::{Can, Principal, cap};
use crate::ui::{Back, UiError, UiResult, options, req_date, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/routines", post(create))
        .route("/routines/{id}/pause", post(pause))
        .route("/routines/{id}/resume", post(resume))
}

pub fn api_routes() -> Router<AppState> {
    Router::new().route("/recurring", get(api_list).post(api_create)).route("/recurring/run", post(api_run))
}

const B: &str = "/tasks";

/// The routines list and the form to add one, for the Tasks page.
pub fn section(who: &Principal, templates: &[Template], locations: &[homestead_domain::Location]) -> Markup {
    let can_write = who.can(homestead_domain::auth::Capability::TasksWrite);
    html! {
        h2 { "Routines" }
        @if templates.is_empty() { p.muted { "Nothing repeats yet." } } @else {
            table {
                @for t in templates {
                    tr {
                        td { "🔁 " (t.title) }
                        td.muted { (t.schedule.describe()) @if t.catch_up { " · every one counts" } }
                        td.muted { @if t.enabled { "next " (crate::ui::Day(t.next_due_on)) } @else { "paused" } }
                        td { @if can_write {
                            @if t.enabled {
                                form.inline method="post" action={"/routines/" (t.id) "/pause"} { button.small { "Pause" } }
                            } @else {
                                form.inline method="post" action={"/routines/" (t.id) "/resume"} { button.small { "Resume" } }
                            }
                        } }
                    }
                }
            }
        }
        @if can_write {
            form.card method="post" action="/routines" {
                strong { "Add a routine" }
                label { "Task" input name="title" required placeholder="Feed the hens"; }
                label { "Details" input name="details"; }
                div.grid {
                    label { "Repeats" select name="repeats" {
                        option value="days" { "every N days" }
                        option value="weekdays" { "on weekdays" }
                        option value="month_day" { "monthly on a date" }
                    } }
                    label { "N days (1 = daily)" input name="days" type="number" min="1" max="366" value="1"; }
                    label { "Day of the month" input name="day" type="number" min="1" max="31" value="1"; }
                }
                fieldset.chips { legend { "Weekdays" }
                    @for d in WeekdaySet::ALL_DAYS {
                        label.inline { input type="checkbox" name="weekday" value=(d.number_days_from_monday()); (d.to_string()[..3]) }
                    }
                }
                div.grid {
                    label { "Starting" input name="starts_on" type="date" required; }
                    label { "Priority" select name="priority" { (options(TaskPriority::ALL, Some(TaskPriority::Normal))) } }
                    label { "Where" select name="location_id" {
                        option value="" { "—" }
                        @for l in locations { option value=(l.id) { (l.name) } }
                    } }
                    label { "Show it this many days early" input name="lead_days" type="number" min="0" max="30" value="0"; }
                }
                label.inline { input type="checkbox" name="catch_up"; "Every occurrence matters (medication): don't skip missed ones" }
                button { "Add routine" }
            }
        }
    }
}

fn field<'a>(pairs: &'a [(String, String)], name: &str) -> &'a str {
    pairs.iter().find(|(k, _)| k == name).map_or("", |(_, v)| v.as_str()).trim()
}

fn number<T: std::str::FromStr>(s: &str, what: &str) -> Result<T, UiError> {
    s.parse().map_err(|_| UiError::invalid(format!("{what}: expected a whole number"), B))
}

async fn create(c: Can<cap::TasksWrite>, State(s): State<AppState>, body: String) -> UiResult {
    let pairs: Vec<(String, String)> = form_urlencoded::parse(body.as_bytes()).into_owned().collect();
    let schedule = match field(&pairs, "repeats") {
        "days" => Schedule::Days { days: number(field(&pairs, "days"), "N days")? },
        "weekdays" => {
            let mut mask = 0u8;
            for (_, v) in pairs.iter().filter(|(k, _)| k == "weekday") {
                let d: u8 = number(v, "weekday")?;
                mask |= 1u8.checked_shl(u32::from(d)).unwrap_or(0);
            }
            Schedule::Weekdays { on: WeekdaySet(mask) }
        }
        "month_day" => Schedule::MonthDay { day: number(field(&pairs, "day"), "day of the month")? },
        _ => return Err(UiError::invalid("choose how it repeats", B)),
    };
    let location_id = match field(&pairs, "location_id") {
        "" => None,
        s => Some(s.parse::<Uuid>().map_err(|_| UiError::invalid("unknown location", B))?),
    };
    let lead = field(&pairs, "lead_days");
    let input = NewTemplate {
        title: field(&pairs, "title").to_owned(),
        details: field(&pairs, "details").to_owned(),
        location_id,
        priority: field(&pairs, "priority").parse::<TaskPriority>().back(B)?,
        schedule,
        starts_on: req_date(field(&pairs, "starts_on"), "starting", B)?,
        lead_days: if lead.is_empty() { 0 } else { number(lead, "days early")? },
        catch_up: !field(&pairs, "catch_up").is_empty(),
    };
    c.store.create_template(input).await.back(B)?;
    // The person made the routine; the scheduler makes its tasks.
    crate::run_schedules(&s).await;
    Ok(see(B))
}

async fn pause(c: Can<cap::TasksWrite>, Path(id): Path<Uuid>) -> UiResult {
    c.store.set_template_enabled(id, false).await.back(B)?;
    Ok(see(B))
}

async fn resume(c: Can<cap::TasksWrite>, State(s): State<AppState>, Path(id): Path<Uuid>) -> UiResult {
    c.store.set_template_enabled(id, true).await.back(B)?;
    crate::run_schedules(&s).await;
    Ok(see(B))
}

async fn api_list(c: Can<cap::TasksRead>) -> ApiResult {
    Ok(Json(c.store.list_templates().await?).into_response())
}

async fn api_create(c: Can<cap::TasksWrite>, Json(b): Json<NewTemplate>) -> ApiResult {
    Ok(created(c.store.create_template(b).await?))
}

/// For an outside cron that wants tasks made even when nobody visits.
async fn api_run(_: Can<cap::TasksWrite>, State(s): State<AppState>) -> ApiResult {
    crate::run_schedules(&s).await;
    Ok(Json(serde_json::json!({ "ok": true })).into_response())
}

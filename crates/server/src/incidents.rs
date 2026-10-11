//! Incident and safety reports: anyone files, few read.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use homestead_domain::attachments::AttachmentOwner;
use homestead_domain::auth::Capability;
use homestead_domain::incidents::{FollowUp, Incident, IncidentKind, IncidentStatus, NewIncident, Treatment};
use maud::html;
use serde::Deserialize;
use time::PrimitiveDateTime;
use time::macros::format_description;
use uuid::Uuid;

use crate::AppState;
use crate::api::ApiResult;
use crate::auth::{Authed, Can, cap, deny};
use crate::ui::{Back, UiError, UiResult, layout, opt_date, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/incidents", get(list).post(report))
        .route("/incidents/{id}", get(detail))
        .route("/incidents/{id}/follow-up", post(follow_up))
        .route("/incidents/{id}/close", post(close))
}

pub fn api_routes() -> Router<AppState> {
    Router::new().route("/incidents", get(api_list))
}

const B: &str = "/incidents";

/// Who may see which reports: everything with incidents.read, only your
/// own with incidents.report, nothing otherwise.
fn scope(a: &Authed) -> Result<Option<Uuid>, Box<Response>> {
    if a.who.can(Capability::IncidentsRead) {
        Ok(None)
    } else if a.who.can(Capability::IncidentsReport) {
        Ok(Some(a.who.actor.id))
    } else {
        Err(Box::new(deny(&a.who, Capability::IncidentsReport)))
    }
}

async fn list(a: Authed) -> UiResult {
    let mine_only = match scope(&a) {
        Ok(s) => s,
        Err(r) => return Ok(*r),
    };
    let incidents = a.store.list_incidents(mine_only, 200).await.back(B)?;
    let locations = a.store.list_locations().await.back(B)?;
    let today = a.store.today().await.back(B)?;
    Ok(layout(
        "Incidents",
        html! {
            p.muted {
                "Report anything that hurt someone or nearly did, any animal bite or scratch, damage, or a food-safety "
                "problem. Say what happened and what was done. Don't include diagnoses or medical details: only "
                "what's needed to stop it happening again."
            }
            @if a.who.can(Capability::IncidentsReport) {
                form.card method="post" action="/incidents" {
                    strong { "Report an incident" }
                    div.grid {
                        label { "What kind" select name="kind" {
                            @for k in IncidentKind::ALL { option value=(k) { (k.label()) } }
                        } }
                        label { "When" input name="occurred_at" type="datetime-local" required; }
                        label { "Where" select name="location_id" {
                            option value="" { "—" }
                            @for l in &locations { option value=(l.id) { (l.name) } }
                        } }
                        label { "Exactly where" input name="location_detail" placeholder="by the north coop door"; }
                    }
                    label { "What happened" textarea name="description" rows="3" required {} }
                    label { "What was done right away" input name="immediate_action"; }
                    div.grid {
                        label { "Who was involved (names or roles)" input name="people_involved"; }
                        label { "Witnesses" input name="witnesses"; }
                        label { "If someone was hurt: treatment" select name="treatment" {
                            option value="" { "— not an injury —" }
                            option value=(Treatment::None) { "none needed" }
                            option value=(Treatment::FirstAid) { "first aid here" }
                            option value=(Treatment::MedicalCare) { "went for medical care" }
                        } }
                    }
                    button { "File report" }
                }
            }
            h2 { @if mine_only.is_some() { "Your reports" } @else { "Reports" } }
            @if incidents.is_empty() { p.muted { "None." } } @else {
                table {
                    tr { th { "When" } th { "What" } th { "Where" } th { "Status" } }
                    @for i in &incidents {
                        tr {
                            td { a href={"/incidents/" (i.id)} { (i.occurred_at.date()) } }
                            td { (i.kind.label()) }
                            td { (i.location_name.as_deref().unwrap_or("—")) }
                            td class=[overdue(i, today).then_some("warn")] {
                                (i.status) @if overdue(i, today) { " · follow-up overdue" }
                            }
                        }
                    }
                }
            }
        },
    )
    .into_response())
}

fn overdue(i: &Incident, today: time::Date) -> bool {
    i.status == IncidentStatus::Open && i.follow_up_by.is_some_and(|d| d < today)
}

#[derive(Deserialize)]
struct ReportForm {
    kind: String,
    occurred_at: String,
    #[serde(default)]
    location_id: String,
    #[serde(default)]
    location_detail: String,
    description: String,
    #[serde(default)]
    immediate_action: String,
    #[serde(default)]
    people_involved: String,
    #[serde(default)]
    witnesses: String,
    #[serde(default)]
    treatment: String,
}

async fn report(c: Can<cap::IncidentsReport>, Form(f): Form<ReportForm>) -> UiResult {
    let local =
        PrimitiveDateTime::parse(f.occurred_at.trim(), format_description!("[year]-[month]-[day]T[hour]:[minute]"))
            .map_err(|_| UiError::invalid("when: expected a date and time", B))?;
    let input = NewIncident {
        occurred_at: c.store.farm_time(local).await.back(B)?,
        location_id: match f.location_id.trim() {
            "" => None,
            v => Some(v.parse().map_err(|_| UiError::invalid("unknown location", B))?),
        },
        location_detail: f.location_detail,
        kind: f.kind.parse::<IncidentKind>().back(B)?,
        description: f.description,
        immediate_action: f.immediate_action,
        people_involved: f.people_involved,
        witnesses: f.witnesses,
        treatment: match f.treatment.trim() {
            "" => None,
            t => Some(t.parse::<Treatment>().back(B)?),
        },
    };
    let i = c.store.report_incident(input).await.back(B)?;
    Ok(see(&format!("/incidents/{}", i.id)))
}

async fn detail(a: Authed, Path(id): Path<Uuid>) -> UiResult {
    let mine_only = match scope(&a) {
        Ok(s) => s,
        Err(r) => return Ok(*r),
    };
    let i = match a.store.get_incident(id).await {
        // Someone else's report is indistinguishable from a missing one.
        Ok(i) if mine_only.is_none() || i.reported_by == mine_only => i,
        Ok(_) => return Ok(StatusCode::NOT_FOUND.into_response()),
        Err(e) => return Err(e).back(B),
    };
    let files = a.store.list_attachments(AttachmentOwner::Incident, id).await.back(B)?;
    let here = format!("/incidents/{id}");
    let can_write = a.who.can(Capability::IncidentsWrite);
    let can_add = can_write || (i.status == IncidentStatus::Open && i.reported_by == Some(a.who.actor.id));
    let times = a.store.farm_times(&[i.occurred_at]).await.back(B)?;
    Ok(layout(
        &format!("{} · {}", i.reference(), i.kind.label()),
        html! {
            p {
                span.pill { (i.status) } " "
                @if let Some(t) = times.first() { (t.date()) " " (format!("{:02}:{:02}", t.hour(), t.minute())) }
                @if let Some(l) = &i.location_name { " · " (l) }
                @if !i.location_detail.is_empty() { " (" (i.location_detail) ")" }
                " · reported by " (i.reported_by_name)
            }
            div.card {
                p { strong { "What happened: " } (i.description) }
                @if !i.immediate_action.is_empty() { p { strong { "Done right away: " } (i.immediate_action) } }
                @if !i.people_involved.is_empty() { p { strong { "Involved: " } (i.people_involved) } }
                @if !i.witnesses.is_empty() { p { strong { "Witnesses: " } (i.witnesses) } }
                @if let Some(t) = i.treatment { p { strong { "Treatment: " } (t) } }
            }
            h2 { "Follow-up" }
            @if i.corrective_action.is_empty() { p.muted { "No corrective action recorded yet." } } @else {
                p { (i.corrective_action) @if let Some(d) = i.follow_up_by { span.muted { " · by " (crate::ui::Day(d)) } } }
            }
            @if i.status == IncidentStatus::Closed {
                p.ok { "Closed " @if let Some(c) = i.closed_at { (c.date()) } @if let Some(n) = &i.closed_by_name { " by " (n) } }
                @if !i.closure_notes.is_empty() { p.muted { (i.closure_notes) } }
            } @else if can_write {
                div.grid {
                    form.card method="post" action={(here) "/follow-up"} {
                        strong { "What will stop it happening again" }
                        label { "Corrective action" textarea name="corrective_action" rows="2" required { (i.corrective_action) } }
                        label { "Follow up by" input name="follow_up_by" type="date"; }
                        button { "Save" }
                    }
                    form.card method="post" action={(here) "/close"} {
                        strong { "Close" }
                        label { "Closing notes" input name="notes"; }
                        button { "Close incident" }
                    }
                }
            }
            h2 { "Photos and documents" }
            (crate::attachments::panel_for(AttachmentOwner::Incident, id, &files, &here, can_add, can_write))
            p { a href="/incidents" { "← Incidents" } }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct FollowUpForm {
    corrective_action: String,
    #[serde(default)]
    follow_up_by: String,
}

async fn follow_up(c: Can<cap::IncidentsWrite>, Path(id): Path<Uuid>, Form(f): Form<FollowUpForm>) -> UiResult {
    let here = format!("/incidents/{id}");
    let input = FollowUp {
        corrective_action: f.corrective_action,
        follow_up_by: opt_date(&f.follow_up_by, "follow up by", B)?,
    };
    c.store.follow_up_incident(id, input).await.back_to(&here)?;
    Ok(see(&here))
}

#[derive(Deserialize)]
struct CloseForm {
    #[serde(default)]
    notes: String,
}

async fn close(c: Can<cap::IncidentsWrite>, Path(id): Path<Uuid>, Form(f): Form<CloseForm>) -> UiResult {
    let here = format!("/incidents/{id}");
    c.store.close_incident(id, &f.notes).await.back_to(&here)?;
    Ok(see(&here))
}

async fn api_list(c: Can<cap::IncidentsRead>) -> ApiResult {
    Ok(Json(c.store.list_incidents(None, 500).await?).into_response())
}

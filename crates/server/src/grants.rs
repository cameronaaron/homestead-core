//! Review-first grant outreach: recipients and templates become frozen
//! campaign snapshots, which must be approved before bounded email delivery.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use homestead_domain::attachments::AttachmentOwner;
use homestead_domain::auth::Capability;
use homestead_integrations::grants::{DESIGNS, LETTER_LIBRARY, Recipient, ResendClient, render_with_design};
use homestead_store::grants::{NewGrantMessage, NewGrantRecipient};
use maud::{PreEscaped, html};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::{Can, cap};
use crate::ui::{error_page, layout};
use crate::{AppState, status_of};

pub struct GrantDelivery {
    pub client: ResendClient,
    pub max_batch: i64,
}

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/grants", get(page))
        .route("/grants/templates", post(create_template))
        .route("/grants/templates/library/{key}", post(save_library_template))
        .route("/grants/templates/{id}", get(template_page).post(update_template))
        .route("/grants/recipients", post(create_recipient))
        .route("/grants/recipients/import", post(import_recipients))
        .route("/grants/recipients/{id}/suppress", post(suppress_recipient))
        .route("/grants/campaigns", post(create_campaign))
        .route("/grants/campaigns/{id}", get(campaign_page))
        .route("/grants/campaigns/{id}/approve", post(approve_campaign))
        .route("/grants/campaigns/{id}/send", post(send_campaign))
        .route("/grants/campaigns/{id}/retry", post(retry_campaign))
}

fn fail(status: StatusCode, message: impl AsRef<str>, back: &'static str) -> Response {
    (status, error_page(message.as_ref(), back)).into_response()
}

fn store_fail(error: &homestead_store::StoreError, back: &'static str) -> Response {
    let (status, message) = status_of(error);
    fail(status, message, back)
}

fn design_name(key: &str) -> &str {
    DESIGNS.iter().find(|(value, _)| *value == key).map(|(_, label)| *label).unwrap_or("Unknown design")
}

fn valid_design(key: &str) -> bool {
    DESIGNS.iter().any(|(value, _)| *value == key)
}

fn design_color(key: &str) -> &'static str {
    match key {
        "meadow" => "#3d7a3a",
        "ink" => "#29404a",
        "orchard" => "#9b4935",
        _ => "#c7771b",
    }
}

fn design_options(selected: &str) -> maud::Markup {
    html! {
        @for (value, label) in DESIGNS {
            option value=(value) selected[*value == selected] { (label) }
        }
    }
}

async fn page(c: Can<cap::GrantsRead>, State(s): State<AppState>) -> Response {
    let templates = match c.store.list_grant_templates().await {
        Ok(v) => v,
        Err(e) => return store_fail(&e, "/grants"),
    };
    let recipients = match c.store.list_grant_recipients().await {
        Ok(v) => v,
        Err(e) => return store_fail(&e, "/grants"),
    };
    let campaigns = match c.store.list_grant_campaigns().await {
        Ok(v) => v,
        Err(e) => return store_fail(&e, "/grants"),
    };
    let active = recipients.iter().filter(|r| r.suppressed_at.is_none()).count();
    layout(
        "Grant studio",
        html! {
            p.muted { "Create one strong proposal, personalize it for every funder, review the exact copies, then send a controlled batch." }
            @if s.grant_delivery.is_none() {
                p.warn { "Drafting and previews are ready. Sending stays locked until [grants] is enabled and RESEND_API_KEY is set." }
            }
            div.grid {
                div.card { div.stat { (templates.len()) } div.muted { "saved letter templates" } }
                div.card { div.stat { (active) } div.muted { "active recipients" } }
                div.card { div.stat { (campaigns.len()) } div.muted { "campaigns" } }
            }
            h2 { "Campaigns" }
            @if campaigns.is_empty() { p.muted { "No campaigns yet." } } @else {
                table {
                    tr { th { "Campaign" } th { "State" } th { "Progress" } th {} }
                    @for c in &campaigns {
                        tr {
                            td { (c.name) br; span.muted { (c.template_name) " · " (c.created_at.date()) } }
                            td { span.pill { (c.status) } }
                            td { (c.sent) "/" (c.total) " sent" @if c.failed > 0 { br; span.warn { (c.failed) " failed" } } @if c.suppressed > 0 { br; span.muted { (c.suppressed) " suppressed" } } }
                            td { a href={"/grants/campaigns/" (c.id)} { "Review →" } }
                        }
                    }
                }
            }
            @if !templates.is_empty() && active > 0 {
                h2 { "Generate a campaign" }
                form.card method="post" action="/grants/campaigns" {
                    label { "Campaign name" input name="name" required maxlength="200" placeholder="Spring 2027 food resilience outreach"; }
                    label { "Proposal template" select name="template_id" { @for t in &templates { option value=(t.id) { (t.name) } } } }
                    p.muted { "This generates a frozen preview for all " (active) " active recipients. It does not send anything." }
                    button { "Generate previews" }
                }
            }
            h2 { "Beautiful letter library" }
            p.muted { "Choose a polished starting point, save it to your templates, then make it unmistakably yours." }
            div.grid {
                @for template in LETTER_LIBRARY {
                    div.card style={"border-top:6px solid " (design_color(template.design))} {
                        span.pill { (template.category) }
                        h2 { (template.title) }
                        p { (template.description) }
                        p.muted { (design_name(template.design)) " · Subject: " (template.subject) }
                        details {
                            summary { "Read the letter" }
                            pre { (template.body) }
                        }
                        @if c.who.can(homestead_domain::auth::Capability::GrantsWrite) {
                            form method="post" action={"/grants/templates/library/" (template.key)} {
                                button { "Save to my templates" }
                            }
                        }
                    }
                }
            }
            h2 { "My saved templates" }
            @if !templates.is_empty() {
                @for t in &templates {
                    details.card style={"border-left:6px solid " (design_color(&t.design))} {
                        summary { strong { (t.name) } " — " (t.subject_template) }
                        p.muted { (design_name(&t.design)) }
                        pre { (t.body_template) }
                        p { a href={"/grants/templates/" (t.id)} { "Edit saved template →" } }
                    }
                }
            } @else {
                p.muted { "Nothing saved yet. Pick from the library or write your own below." }
            }
            @if c.who.can(homestead_domain::auth::Capability::GrantsWrite) {
                form.card method="post" action="/grants/templates" {
                    strong { "Write and save another template" }
                    label { "Name" input name="name" required maxlength="200" placeholder="Food resilience introduction"; }
                    label { "Email subject" input name="subject" required placeholder="{{organization}} + Sun Brewed Homestead"; }
                    label { "Proposal letter" textarea name="body" rows="12" required placeholder="Dear {{contact_name}},\n\nSun Brewed Homestead…" {} }
                    label { "Letter design" select name="design" { (design_options("sunrise")) } }
                    p.muted { "Built-ins: {{contact_name}}, {{organization}}, {{program_name}}, {{email}}. CSV columns with other names become custom variables." }
                    button { "Save template" }
                }
            }
            h2 { "Recipients" }
            @if recipients.is_empty() { p.muted { "No funders or organizations added yet." } } @else {
                table {
                    tr { th { "Organization" } th { "Contact" } th { "Source" } th {} }
                    @for r in &recipients {
                        tr {
                            td { (r.organization) @if !r.program_name.is_empty() { br; span.muted { (r.program_name) } } }
                            td { (r.contact_name) br; span.muted { (r.email) } }
                            td.muted { (r.source) }
                            td {
                                @if r.suppressed_at.is_some() { span.pill { "do not contact" } }
                                @else { form.inline method="post" action={"/grants/recipients/" (r.id) "/suppress"} { button.small { "Suppress" } } }
                            }
                        }
                    }
                }
            }
            div.grid {
                @if c.who.can(homestead_domain::auth::Capability::GrantsWrite) {
                    form.card method="post" action="/grants/recipients" {
                        strong { "Add one recipient" }
                        label { "Organization" input name="organization" required; }
                        label { "Contact name" input name="contact_name"; }
                        label { "Email" input type="email" name="email" required; }
                        label { "Program or opportunity" input name="program_name"; }
                        label { "Where this contact came from" input name="source" required placeholder="Foundation website, existing relationship…"; }
                        label { "Custom fields (JSON)" textarea name="custom_fields" rows="3" placeholder={"{\"amount\":\"$25,000\"}"} {} }
                        button { "Add recipient" }
                    }
                }
                @if c.who.can(homestead_domain::auth::Capability::GrantsWrite) {
                    form.card method="post" action="/grants/recipients/import" {
                        strong { "Import CSV" }
                        p.muted { "Required columns: organization, email, source. Optional: contact_name, program_name. Every other column becomes a template variable." }
                        label { "CSV" textarea name="csv" rows="12" required placeholder="organization,contact_name,email,program_name,source,amount" {} }
                        button { "Import recipients" }
                    }
                }
            }
        },
    )
    .into_response()
}

#[derive(Deserialize)]
struct TemplateForm {
    name: String,
    subject: String,
    body: String,
    #[serde(default = "default_design")]
    design: String,
}

fn default_design() -> String {
    "sunrise".into()
}

async fn create_template(c: Can<cap::GrantsWrite>, Form(f): Form<TemplateForm>) -> Response {
    if !valid_design(&f.design) {
        return fail(StatusCode::UNPROCESSABLE_ENTITY, "Choose a known letter design.", "/grants");
    }
    match c.store.create_grant_template_with_design(&f.name, &f.subject, &f.body, &f.design).await {
        Ok(_) => Redirect::to("/grants").into_response(),
        Err(e) => store_fail(&e, "/grants"),
    }
}

async fn save_library_template(c: Can<cap::GrantsWrite>, Path(key): Path<String>) -> Response {
    let Some(template) = LETTER_LIBRARY.iter().find(|template| template.key == key) else {
        return fail(StatusCode::NOT_FOUND, "That library template does not exist.", "/grants");
    };
    match c
        .store
        .create_grant_template_with_design(template.title, template.subject, template.body, template.design)
        .await
    {
        Ok(saved) => Redirect::to(&format!("/grants/templates/{}", saved.id)).into_response(),
        Err(e) => store_fail(&e, "/grants"),
    }
}

async fn template_page(c: Can<cap::GrantsRead>, Path(id): Path<Uuid>) -> Response {
    let template = match c.store.get_grant_template(id).await {
        Ok(value) => value,
        Err(e) => return store_fail(&e, "/grants"),
    };
    layout(
        "Edit letter template",
        html! {
            p { a href="/grants" { "← Grant studio" } }
            form.card method="post" action={"/grants/templates/" (id)} {
                strong { "Saved template" }
                label { "Name" input name="name" required maxlength="200" value=(template.name); }
                label { "Email subject" input name="subject" required value=(template.subject_template); }
                label { "Proposal letter" textarea name="body" rows="18" required { (template.body_template) } }
                label { "Letter design" select name="design" { (design_options(&template.design)) } }
                p.muted { "Saving changes affects future campaigns only. Existing generated letters remain frozen." }
                button { "Save changes" }
            }
        },
    )
    .into_response()
}

async fn update_template(c: Can<cap::GrantsWrite>, Path(id): Path<Uuid>, Form(f): Form<TemplateForm>) -> Response {
    if !valid_design(&f.design) {
        return fail(StatusCode::UNPROCESSABLE_ENTITY, "Choose a known letter design.", "/grants");
    }
    match c.store.update_grant_template(id, &f.name, &f.subject, &f.body, &f.design).await {
        Ok(_) => Redirect::to("/grants").into_response(),
        Err(e) => store_fail(&e, "/grants"),
    }
}

#[derive(Deserialize)]
struct RecipientForm {
    organization: String,
    #[serde(default)]
    contact_name: String,
    email: String,
    #[serde(default)]
    program_name: String,
    source: String,
    #[serde(default)]
    custom_fields: String,
}

fn parse_custom(value: &str) -> Result<BTreeMap<String, String>, String> {
    if value.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    serde_json::from_str(value).map_err(|e| format!("Custom fields must be a JSON object whose values are text: {e}"))
}

async fn create_recipient(c: Can<cap::GrantsWrite>, Form(f): Form<RecipientForm>) -> Response {
    let custom = match parse_custom(&f.custom_fields) {
        Ok(v) => v,
        Err(e) => return fail(StatusCode::UNPROCESSABLE_ENTITY, e, "/grants"),
    };
    match c
        .store
        .create_grant_recipient(&f.organization, &f.contact_name, &f.email, &f.program_name, &f.source, &custom)
        .await
    {
        Ok(_) => Redirect::to("/grants").into_response(),
        Err(e) => store_fail(&e, "/grants"),
    }
}

#[derive(Deserialize)]
struct CsvForm {
    csv: String,
}

async fn import_recipients(c: Can<cap::GrantsWrite>, Form(f): Form<CsvForm>) -> Response {
    let mut reader = csv::ReaderBuilder::new().trim(csv::Trim::All).from_reader(f.csv.as_bytes());
    let headers = match reader.headers() {
        Ok(h) => h.clone(),
        Err(e) => return fail(StatusCode::UNPROCESSABLE_ENTITY, format!("CSV header: {e}"), "/grants"),
    };
    for required in ["organization", "email", "source"] {
        if !headers.iter().any(|h| h == required) {
            return fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("CSV is missing required column {required}"),
                "/grants",
            );
        }
    }
    let mut rows = Vec::new();
    for (index, row) in reader.records().enumerate() {
        let row = match row {
            Ok(v) => v,
            Err(e) => return fail(StatusCode::UNPROCESSABLE_ENTITY, format!("CSV row {}: {e}", index + 2), "/grants"),
        };
        let fields: BTreeMap<&str, &str> = headers.iter().zip(row.iter()).collect();
        let mut custom = BTreeMap::new();
        for (key, value) in &fields {
            if !["organization", "contact_name", "email", "program_name", "source"].contains(key) && !value.is_empty() {
                custom.insert((*key).to_owned(), (*value).to_owned());
            }
        }
        rows.push(NewGrantRecipient {
            organization: fields.get("organization").copied().unwrap_or_default().to_owned(),
            contact_name: fields.get("contact_name").copied().unwrap_or_default().to_owned(),
            email: fields.get("email").copied().unwrap_or_default().to_owned(),
            program_name: fields.get("program_name").copied().unwrap_or_default().to_owned(),
            source: fields.get("source").copied().unwrap_or_default().to_owned(),
            custom_fields: custom,
        });
    }
    if rows.is_empty() {
        return fail(StatusCode::UNPROCESSABLE_ENTITY, "CSV has no recipient rows", "/grants");
    }
    // Validate the entire CSV shape before inserting any row. Database
    // constraints still protect each write; duplicate emails are reported.
    for (index, row) in rows.iter().enumerate() {
        if row.organization.trim().is_empty() || !row.email.contains('@') || row.source.trim().is_empty() {
            return fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("CSV row {} has a blank organization/source or invalid email", index + 2),
                "/grants",
            );
        }
    }
    if let Err(e) = c.store.import_grant_recipients(&rows).await {
        return store_fail(&e, "/grants");
    }
    Redirect::to("/grants").into_response()
}

async fn suppress_recipient(c: Can<cap::GrantsWrite>, Path(id): Path<Uuid>) -> Response {
    match c.store.suppress_grant_recipient(id).await {
        Ok(()) => Redirect::to("/grants").into_response(),
        Err(e) => store_fail(&e, "/grants"),
    }
}

#[derive(Deserialize)]
struct CampaignForm {
    name: String,
    template_id: Uuid,
}

async fn create_campaign(c: Can<cap::GrantsWrite>, Form(f): Form<CampaignForm>) -> Response {
    let template = match c.store.get_grant_template(f.template_id).await {
        Ok(v) => v,
        Err(e) => return store_fail(&e, "/grants"),
    };
    let recipients = match c.store.list_grant_recipients().await {
        Ok(v) => v,
        Err(e) => return store_fail(&e, "/grants"),
    };
    let mut messages = Vec::new();
    for recipient in recipients.iter().filter(|r| r.suppressed_at.is_none()) {
        let rendered = match render_with_design(
            &template.design,
            &template.subject_template,
            &template.body_template,
            &Recipient {
                organization: &recipient.organization,
                contact_name: &recipient.contact_name,
                email: &recipient.email,
                program_name: &recipient.program_name,
                custom_fields: &recipient.custom_fields,
            },
        ) {
            Ok(v) => v,
            Err(e) => {
                return fail(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!(
                        "{} ({}) could not be generated: {e}. Nothing was created.",
                        recipient.organization, recipient.email
                    ),
                    "/grants",
                );
            }
        };
        messages.push(NewGrantMessage {
            id: Uuid::new_v4(),
            recipient_id: recipient.id,
            recipient_email: recipient.email.clone(),
            recipient_name: recipient.contact_name.clone(),
            organization: recipient.organization.clone(),
            subject: rendered.subject,
            body_text: rendered.text,
            body_html: rendered.html,
        });
    }
    match c.store.create_grant_campaign(&f.name, f.template_id, &messages).await {
        Ok(id) => Redirect::to(&format!("/grants/campaigns/{id}")).into_response(),
        Err(e) => store_fail(&e, "/grants"),
    }
}

async fn campaign_page(c: Can<cap::GrantsRead>, State(s): State<AppState>, Path(id): Path<Uuid>) -> Response {
    let campaign = match c.store.get_grant_campaign(id).await {
        Ok(v) => v,
        Err(e) => return store_fail(&e, "/grants"),
    };
    let messages = match c.store.grant_campaign_messages(id).await {
        Ok(v) => v,
        Err(e) => return store_fail(&e, "/grants"),
    };
    let files = match c.store.list_attachments(AttachmentOwner::GrantCampaign, id).await {
        Ok(v) => v,
        Err(e) => return store_fail(&e, "/grants"),
    };
    let (can_approve, can_send) = (c.who.can(Capability::GrantsApprove), c.who.can(Capability::GrantsSend));
    layout(
        &campaign.name,
        html! {
            p { a href="/grants" { "← Grant studio" } }
            div.card {
                strong { (campaign.template_name) }
                p { span.pill { (campaign.status) } " · " (campaign.sent) "/" (campaign.total) " sent" }
                @if campaign.status == "draft" && can_approve {
                    p.warn { "Review the generated copies below. Approval freezes this campaign for delivery." }
                    form method="post" action={"/grants/campaigns/" (id) "/approve"} { button { "Approve exact copies" } }
                } @else if campaign.status == "draft" {
                    p.muted { "Waiting for someone with grants.approve to review and approve it." }
                } @else if campaign.status == "approved" && can_send {
                    @if let Some(delivery) = &s.grant_delivery {
                        p.muted { "Each click sends at most " (delivery.max_batch) " messages. Suppressed recipients are always skipped." }
                        form.inline method="post" action={"/grants/campaigns/" (id) "/send"} { button { "Send next batch" } }
                        @if campaign.failed > 0 { form.inline method="post" action={"/grants/campaigns/" (id) "/retry"} { button { "Retry failed" } } }
                    } @else {
                        p.warn { "Sending is locked until the Resend integration is configured." }
                    }
                }
            }
            h2 { "Documents" }
            p.muted { "Budgets, letters of support, award letters and reports for this campaign." }
            (crate::attachments::panel(&c.who, AttachmentOwner::GrantCampaign, id, &files, &format!("/grants/campaigns/{id}")))
            h2 { "Generated copies" }
            @for message in &messages {
                details.card {
                    summary {
                        strong { (message.organization) } " — " (message.recipient_email) " "
                        span.pill { (message.status) }
                    }
                    p { strong { "Subject: " } (message.subject) }
                    div { (PreEscaped(&message.body_html)) }
                    @if let Some(error) = &message.last_error { p.warn { "Delivery error: " (error) } }
                }
            }
        },
    )
    .into_response()
}

async fn approve_campaign(c: Can<cap::GrantsApprove>, Path(id): Path<Uuid>) -> Response {
    match c.store.approve_grant_campaign(id).await {
        Ok(()) => Redirect::to(&format!("/grants/campaigns/{id}")).into_response(),
        Err(e) => store_fail(&e, "/grants"),
    }
}

async fn send_campaign(c: Can<cap::GrantsSend>, State(s): State<AppState>, Path(id): Path<Uuid>) -> Response {
    let Some(delivery) = s.grant_delivery.as_deref() else {
        return fail(StatusCode::FORBIDDEN, "Grant email delivery is not configured.", "/grants");
    };
    let messages = match c.store.claim_grant_messages(id, delivery.max_batch).await {
        Ok(v) => v,
        Err(e) => return store_fail(&e, "/grants"),
    };
    for message in messages {
        match delivery
            .client
            .send(message.id, &message.recipient_email, &message.subject, &message.body_text, &message.body_html)
            .await
        {
            Ok(provider_id) => {
                if let Err(e) = c.store.settle_grant_message(message.id, Ok(&provider_id)).await {
                    tracing::error!(error = %e, message_id = %message.id, "grant email sent but not recorded");
                    return store_fail(&e, "/grants");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, message_id = %message.id, "grant email delivery failed");
                if let Err(store_error) = c.store.settle_grant_message(message.id, Err(&e.to_string())).await {
                    return store_fail(&store_error, "/grants");
                }
            }
        }
    }
    Redirect::to(&format!("/grants/campaigns/{id}")).into_response()
}

async fn retry_campaign(c: Can<cap::GrantsSend>, Path(id): Path<Uuid>) -> Response {
    match c.store.retry_grant_failures(id).await {
        Ok(_) => Redirect::to(&format!("/grants/campaigns/{id}")).into_response(),
        Err(e) => store_fail(&e, "/grants"),
    }
}

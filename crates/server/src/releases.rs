//! Legal releases: send them for e-signature through Documenso, and record
//! the outcome when Documenso calls back.

use std::collections::BTreeMap;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use homestead_integrations::documenso::{self, Settled};
use homestead_store::releases::{Outcome, SentRelease};
use maud::html;
use uuid::Uuid;

use homestead_domain::auth::Capability;

use crate::auth::{Can, cap};
use crate::ui::{error_page, layout};
use crate::{AppState, status_of};

pub struct Releases {
    pub client: documenso::Client,
    pub spec: documenso::Spec,
    /// Release key → PDF bytes, loaded at startup.
    pub pdfs: BTreeMap<String, Vec<u8>>,
    /// Release key → the signing fields its PDF was drawn with.
    pub layouts: BTreeMap<String, Vec<documenso::Field>>,
    pub webhook_secret: String,
}

/// The release `/onboard` sends: everything joining takes, signed once.
pub const WELCOME_AGREEMENT: &str = "onboarding";

/// Roles that don't join through the welcome agreement: the stewards who run
/// the server, and the board, who sign their own documents (the board
/// bundle). Everyone else holding a role is expected to have signed it.
const NOT_ONBOARDED: &[&str] = &["Steward", "Board"];

/// A release Documenso accepted, not yet recorded.
pub(crate) struct Dispatched {
    pub id: Uuid,
    pub title: String,
    pub sent: documenso::Sent,
}

impl Releases {
    /// Sends `rel` to be signed. Record the result with the returned id,
    /// which Documenso's webhook will carry back. `redirect` is where
    /// Documenso sends the signer once they've signed.
    pub(crate) async fn dispatch(
        &self,
        rel: &documenso::Release,
        signer: documenso::Signer<'_>,
        values: &BTreeMap<String, String>,
        redirect: Option<&str>,
    ) -> Result<Dispatched, String> {
        let id = Uuid::new_v4();
        let envelope = self.create(rel, signer, values, redirect, id).await?;
        let sent = self.distribute(envelope, signer.email).await?;
        Ok(Dispatched { id, title: documenso::document_title(rel, signer.name, values), sent })
    }

    /// The first half of [`Self::dispatch`]: wakes Documenso and creates
    /// the envelope, sent to nobody yet. `id` is what its webhook carries
    /// back. Returns the envelope id.
    pub(crate) async fn create(
        &self,
        rel: &documenso::Release,
        signer: documenso::Signer<'_>,
        values: &BTreeMap<String, String>,
        redirect: Option<&str>,
        id: Uuid,
    ) -> Result<String, String> {
        let redirected;
        let rel = match redirect {
            None => rel,
            Some(url) => {
                let mut r = rel.clone();
                r.meta.insert("redirectUrl".into(), serde_json::json!(url));
                redirected = r;
                &redirected
            }
        };
        let pdf = self.pdfs.get(&rel.key).cloned().unwrap_or_default();
        let layout = self.layouts.get(&rel.key).map(Vec::as_slice).unwrap_or_default();
        self.client.wake().await;
        self.client.create(&self.spec, rel, pdf, layout, signer, values, id).await
    }

    /// The second half: sends a created envelope out, and returns the
    /// signer's link. Safe to repeat on the same envelope.
    pub(crate) async fn distribute(&self, envelope: String, email: &str) -> Result<documenso::Sent, String> {
        let sent = self.client.distribute(envelope, email).await?;
        if sent.signing_url.is_none() && !self.spec.emails_signers() {
            tracing::warn!(envelope = sent.envelope, "Documenso returned no signing link and emails nobody");
        }
        Ok(sent)
    }
}

pub fn ui_routes() -> Router<AppState> {
    Router::new().route("/releases", get(page).post(send))
}

fn fail(status: StatusCode, msg: &str) -> Response {
    (status, error_page(msg, "/releases")).into_response()
}

/// Form field for a sender-filled detail of a release.
fn detail_field(release: &str, detail: &str) -> String {
    format!("d_{release}_{detail}")
}

async fn page(
    c: Can<cap::ReleasesRead>,
    State(s): State<AppState>,
    Query(q): Query<BTreeMap<String, String>>,
) -> Response {
    let Some(r) = s.releases.as_deref() else {
        return layout(
            "Releases",
            html! { p.muted { "Documenso isn't connected. Enable " code { "[documenso]" } " in homestead.toml and set its secrets." } },
        )
        .into_response();
    };
    let recent = match c.store.recent_releases(50).await {
        Ok(v) => v,
        Err(e) => return fail(status_of(&e).0, &status_of(&e).1),
    };
    let joined = match c.store.onboardings().await {
        Ok(v) => v,
        Err(e) => return fail(status_of(&e).0, &status_of(&e).1),
    };
    let waiting = joined.iter().filter(|o| o.completed_at.is_none()).count();
    let linked = match c.store.joins(100).await {
        Ok(v) => v,
        Err(e) => return fail(status_of(&e).0, &status_of(&e).1),
    };
    // Roles a steward gives by hand (Crew, working groups) skip /onboard,
    // so check who in Discord holds a role without a signed agreement.
    let unsigned = match s.discord_feed.as_deref() {
        None => None,
        Some(feed) => Some(feed.role_holders().await.map(|holders| {
            let signed: std::collections::BTreeSet<&str> =
                joined.iter().filter(|o| o.completed_at.is_some()).map(|o| o.discord_user_id.as_str()).collect();
            holders
                .iter()
                .filter(|h| !signed.contains(h.user_id.as_str()))
                .filter(|h| !h.roles.iter().any(|r| NOT_ONBOARDED.contains(&r.as_str())))
                .cloned()
                .collect::<Vec<_>>()
        })),
    };
    let ready = r.spec.approved;
    let bundle =
        q.get("bundle").and_then(|k| r.spec.bundles.iter().find(|b| &b.key == k)).or_else(|| r.spec.bundles.first());
    let preselected = |key: &str| bundle.is_some_and(|b| b.releases.iter().any(|k| k == key));
    // A signing link lets whoever holds it sign, so only senders see them.
    let links = c.who.can(Capability::ReleasesSend);
    layout(
        "Releases",
        html! {
            @if !ready {
                p.warn { "These releases are drafts, so sending is turned off. \
                          Once they are approved for use, set " code { "approved = true" } " in documenso/releases.toml." }
            }
            @if ready && !r.spec.emails_signers() {
                p.muted { "Documenso doesn't email signers yet. After sending, copy each signing link from Recent \
                           and send it to the person (text or DM), or open it on a phone for them to sign on the spot." }
            }
            form.card method="post" action="/releases" {
                strong { "Send releases to sign" }
                @if !r.spec.bundles.is_empty() {
                    p.muted {
                        "Who's signing? "
                        @for (i, b) in r.spec.bundles.iter().enumerate() {
                            @if i > 0 { " · " }
                            @if bundle.is_some_and(|x| x.key == b.key) { strong { (b.title) } }
                            @else { a href={"/releases?bundle=" (b.key)} { (b.title) } }
                        }
                    }
                }
                div.grid {
                    label { "Full name" input name="name" required autocomplete="off"; }
                    label { "Email" input type="email" name="email" required autocomplete="off"; }
                }
                @for rel in &r.spec.releases {
                    label.inline {
                        input type="checkbox" name={"r_" (rel.key)} checked[preselected(&rel.key)];
                        (rel.title)
                    }
                    @if rel.sender_details().next().is_some() {
                        div.grid {
                            @for d in rel.sender_details() {
                                label { (d.label) input name=(detail_field(&rel.key, &d.key)) autocomplete="off"; }
                            }
                        }
                    }
                }
                button disabled[!ready] { "Send for signature" }
            }
            h2 { "Joined from the link" }
            p.muted {
                "Share " @match crate::join::link(&s) { Some(url) => { a href=(url) { (url) } }, None => { code { "/join" } } }
                " (on Instagram, a flyer…). People sign the welcome agreement, connect Discord, and automatically \
                 join as a Volunteer. Their verified Discord username appears below, and the daily sync adds it to "
                code { "discord/server.toml" } ", where you change their roles."
            }
            @if !linked.is_empty() {
                table {
                    tr { th { "Name" } th { "Email" } th { "Started" } th { "Welcome agreement" } th { "Discord" } }
                    @for j in &linked {
                        tr {
                            td { (j.name) }
                            td.muted { (j.email) }
                            td.muted { (j.created_at.date()) }
                            @match (j.request.as_ref().map(|r| r.status.as_str()), &j.send_error) {
                                (Some("signed"), _) => td.ok { "signed" },
                                (Some("declined"), _) => td.warn { "declined" },
                                (Some(_), _) => td { "waiting to sign" },
                                (None, Some(e)) => td.warn { "couldn't send: " (e) },
                                (None, None) => td.muted { "sending…" },
                            }
                            td { @if let Some((_, username)) = &j.discord { "@" (username) } }
                        }
                    }
                }
            }
            h2 { "Joining through Discord" }
            p.muted {
                "Someone already in the server can also run " code { "/onboard" } " to get the agreement; signing \
                 it gives them the role they chose. "
                @if !joined.is_empty() { (joined.len() - waiting) " signed · " (waiting) " still to sign." }
            }
            @if !joined.is_empty() {
                table {
                    tr { th { "Discord" } th { "Name" } th { "Role" } th { "Welcome agreement" } @if links { th { "Signing link" } } }
                    @for o in &joined {
                        @let pending_url = o.latest.as_ref().filter(|r| r.status == "sent").and_then(|r| r.signing_url.as_deref());
                        tr {
                            td { "@" (o.discord_username) }
                            td { (o.latest.as_ref().map_or("", |r| r.signer_name.as_str())) }
                            td { (o.role) }
                            @match (o.completed_at, o.latest.as_ref().map(|r| r.status.as_str())) {
                                (Some(at), _) => td.ok { "signed " (at.date()) },
                                (None, Some("declined")) => td.warn { "declined" },
                                (None, _) => td { "waiting since " (o.started_at.date()) },
                            }
                            @if links {
                                td {
                                    @if let Some(url) = pending_url {
                                        input readonly value=(url) aria-label={"Signing link for @" (o.discord_username)};
                                    }
                                }
                            }
                        }
                    }
                }
            }
            @match &unsigned {
                None => {}
                Some(Err(e)) => p.warn { "Couldn't check who in Discord hasn't signed: " (e) },
                Some(Ok(v)) if v.is_empty() => p.ok { "Everyone in Discord with a role has signed the welcome agreement." },
                Some(Ok(v)) => {
                    p.warn {
                        strong { (v.len()) " in Discord hold a role without a matching signed agreement. " }
                        "Send them the join link so their paperwork and Discord identity are connected. \
                         (Stewards and the Board aren't listed.)"
                    }
                    ul { @for h in v { li { "@" (h.username) " · " (h.roles.join(", ")) } } }
                }
            }
            h2 { "Recent" }
            @if recent.is_empty() { p.muted { "Nothing sent yet." } } @else {
                table {
                    tr { th { "Release" } th { "Signer" } th { "Status" } th { "Sent" } @if links { th { "Signing link" } } }
                    @for q in &recent {
                        tr {
                            td { (q.title) }
                            td { (q.signer_name) br; span.muted { (q.signer_email) } }
                            td class=[match q.status.as_str() { "declined" => Some("warn"), "signed" => Some("ok"), _ => None }] { (q.status) }
                            td.muted { (q.sent_at.date()) }
                            @if links {
                                td {
                                    @if let Some(url) = &q.signing_url {
                                        input readonly value=(url) aria-label={"Signing link for " (q.signer_name)};
                                        a href=(url) target="_blank" rel="noreferrer" { "Open" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        },
    )
    .into_response()
}

async fn send(
    c: Can<cap::ReleasesSend>,
    State(s): State<AppState>,
    Form(form): Form<BTreeMap<String, String>>,
) -> Response {
    let Some(r) = s.releases.as_deref() else { return fail(StatusCode::NOT_FOUND, "Documenso isn't connected.") };
    if !r.spec.approved {
        return fail(
            StatusCode::FORBIDDEN,
            "These releases are drafts that haven't been approved for use, so they can't be sent.",
        );
    }
    let name = form.get("name").map(|v| v.trim()).unwrap_or_default();
    let email = form.get("email").map(|v| v.trim()).unwrap_or_default();
    if name.is_empty() || !email.contains('@') {
        return fail(StatusCode::UNPROCESSABLE_ENTITY, "Enter the signer's full name and email.");
    }
    let chosen: Vec<_> = r.spec.releases.iter().filter(|rel| form.contains_key(&format!("r_{}", rel.key))).collect();
    if chosen.is_empty() {
        return fail(StatusCode::UNPROCESSABLE_ENTITY, "Choose at least one release to send.");
    }
    // Check every chosen release's details before sending any, so a gap in
    // the second doesn't leave the first already emailed.
    let mut ready = Vec::new();
    for rel in chosen {
        let given: BTreeMap<String, String> = rel
            .sender_details()
            .filter_map(|d| form.get(&detail_field(&rel.key, &d.key)).map(|v| (d.key.clone(), v.clone())))
            .collect();
        match documenso::sender_values(rel, &given) {
            Ok(values) => ready.push((rel, values)),
            Err(missing) => {
                return fail(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &format!("The {} needs: {}. Nothing was sent.", rel.title, missing.join(", ")),
                );
            }
        }
    }
    for (rel, values) in ready {
        let Dispatched { id, title, sent } =
            match r.dispatch(rel, documenso::Signer { name, email }, &values, None).await {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!(error = %e, release = rel.key, "Documenso send failed");
                    return fail(
                        StatusCode::BAD_GATEWAY,
                        &format!("Documenso didn't accept the {}. Nothing further was sent.", rel.title),
                    );
                }
            };
        let recorded = c
            .store
            .record_release_sent(&SentRelease {
                id,
                release_key: &rel.key,
                title: &title,
                signer_name: name,
                signer_email: email,
                envelope_id: &sent.envelope,
                signing_url: sent.signing_url.as_deref(),
            })
            .await;
        if let Err(e) = recorded {
            // Documenso already has it; its webhook will find no row and be
            // ignored, so say so rather than hide it.
            tracing::error!(error = %e, envelope = sent.envelope, "release sent but not recorded");
            return fail(
                status_of(&e).0,
                &format!("The {} was sent but couldn't be recorded: {}", rel.title, status_of(&e).1),
            );
        }
    }
    Redirect::to("/releases").into_response()
}

/// `POST /hooks/documenso`. Documenso retries on non-2xx, so storage errors
/// return 500 and everything we deliberately ignore returns 200.
pub async fn webhook(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(r) = s.releases.as_deref() else { return StatusCode::NOT_FOUND.into_response() };
    let presented = headers.get("x-documenso-secret").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if !documenso::secret_matches(presented, &r.webhook_secret) {
        tracing::warn!("rejected Documenso webhook with a bad secret");
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let settled = match documenso::parse_webhook(&body) {
        Ok(Some(s)) => s,
        Ok(None) => return StatusCode::OK.into_response(),
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let (id, outcome) = match settled {
        Settled::Completed(id) => (id, Outcome::Signed),
        Settled::Rejected(id) => (id, Outcome::Declined),
    };
    match s.system_store.complete_release(id, outcome).await {
        Ok(Some(_)) => StatusCode::OK.into_response(),
        Ok(None) => {
            tracing::info!(%id, "webhook for an unknown or already-settled release; ignored");
            StatusCode::OK.into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "could not record release outcome");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

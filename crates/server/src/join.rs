//! Joining from a link: `/join` is what goes in the Instagram bio.
//!
//! No account and no Discord command. Someone gives their name and email,
//! signs the welcome agreement, then Discord OAuth identifies that
//! exact account and the bot adds it with the Volunteer role. `systems-sync`
//! then writes its username into `discord/server.toml` for a steward to
//! adjust later.
//!
//! `/join/{token}` is the person's own page (the token is theirs alone, and
//! only its hash is stored). Documenso sends them back to it after signing.
//! The form carries that token, made when the form is shown, so sending the
//! form twice (a double tap) lands on the same join instead of making two.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use homestead_domain::onboarding::OnboardingRole;
use homestead_integrations::discord::feed::Reader;
use homestead_integrations::documenso;
use homestead_store::Store;
use homestead_store::releases::{Join, JoinSend, SentRelease};
use maud::{PreEscaped, html};
use serde::Deserialize;

use crate::AppState;
use crate::auth::{hash_secret, new_secret};
use crate::releases::{Releases, WELCOME_AGREEMENT};
use crate::ui::{Back, UiError, UiResult, layout, see};

/// What joining from the link makes someone. Stewards move people on from
/// there by editing `[members]` in `discord/server.toml`.
pub const JOIN_ROLE: OnboardingRole = OnboardingRole::Volunteer;

/// Hides the honeypot from people (and screen readers) but not from bots
/// that fill every field.
const HONEYPOT_CSS: &str = ".hp{position:absolute;left:-10000px;width:1px;height:1px;overflow:hidden}\
     .step{display:flex;gap:.75rem;align-items:baseline}.step b{min-width:1.5rem}";

pub struct DiscordJoin {
    pub reader: Reader,
    pub application_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}

/// Reachable without signing in; see the module docs.
pub fn public_routes() -> Router<AppState> {
    Router::new()
        .route("/join", get(page).post(start))
        .route("/join/discord", get(discord_callback))
        .route("/join/{token}", get(progress))
        .route("/join/{token}/retry", post(retry))
}

/// Whether joining is switched on: releases approved, the welcome agreement
/// configured, and Discord OAuth able to add the signer.
fn open(s: &AppState) -> bool {
    s.discord_join.is_some()
        && s.releases.as_deref().is_some_and(|r| r.spec.approved && r.spec.release(WELCOME_AGREEMENT).is_some())
}

/// Whether `token` has the shape [`new_secret`] makes.
fn well_formed(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

async fn page(State(s): State<AppState>) -> Response {
    // The form's token becomes the person's page, so no cache may hand one
    // form to two people.
    let (token, _) = new_secret();
    let page = layout(
        "Join us",
        html! {
            style { (PreEscaped(HONEYPOT_CSS)) }
            p { "Come grow with Sun Brewed Homestead! Volunteers help on work days with planting, building, \
                 animal chores and events. Everything happens in our Discord." }
            div.card {
                div.step { b { "1" } span { "Tell us your name and email." } }
                div.step { b { "2" } span { "Sign our welcome agreement on your phone (about five minutes)." } }
                div.step { b { "3" } span { "Connect Discord. You'll arrive as a Volunteer, with the channels open." } }
            }
            @if open(&s) {
                form.card method="post" action="/join" {
                    label { "Your full name" input name="name" required maxlength="80" autocomplete="name"; }
                    label { "Email" input name="email" type="email" required maxlength="254" autocomplete="email"; }
                    input type="hidden" name="t" value=(token);
                    div.hp aria-hidden="true" { label { "Leave this empty" input name="website" tabindex="-1" autocomplete="off"; } }
                    button { "Continue" }
                    p.muted { "We use your email only for your signed copy and homestead news you can opt out of." }
                }
            } @else {
                p.warn { "Joining online isn't open right now. Please message us on Instagram and we'll get you in." }
            }
        },
    );
    ([(header::CACHE_CONTROL, "no-store")], page).into_response()
}

#[derive(Deserialize)]
struct StartForm {
    name: String,
    email: String,
    /// The honeypot: people never see it, so anything in it is a bot.
    #[serde(default)]
    website: String,
    /// Their page's token, from the form; see the module docs.
    #[serde(default)]
    t: String,
}

async fn start(State(s): State<AppState>, Form(f): Form<StartForm>) -> UiResult {
    const B: &str = "/join";
    let token = if well_formed(&f.t) { f.t } else { new_secret().0 };
    if !f.website.is_empty() {
        // Looks accepted, so the bot learns nothing; the page it lands on
        // belongs to no one.
        return Ok(see(&format!("/join/{token}")));
    }
    let Some(releases) = s.releases.clone().filter(|_| open(&s)) else {
        return Err(UiError::invalid("Joining online isn't open right now.", B));
    };
    let (name, email) = (f.name.trim().to_owned(), f.email.trim().to_owned());
    if name.is_empty() || name.chars().count() > 80 || !email.contains('@') || email.len() > 254 {
        return Err(UiError::invalid("Please give your full name and your email address.", B));
    }
    let store = s.public_store("join page");
    // `None`: the same form again, whose join is already under way.
    if let Some(job) = store.start_join(&hash_secret(&token), &name, &email, JOIN_ROLE).await.back(B)? {
        send_in_background(&s, store, releases, job, &token);
    }
    Ok(see(&format!("/join/{token}")))
}

/// "Try again" on a failed or cut-off send: sends the same join again,
/// distributing its envelope if Documenso already has one.
async fn retry(State(s): State<AppState>, Path(token): Path<String>) -> UiResult {
    let back = format!("/join/{token}");
    let store = s.public_store("join page");
    let join = if well_formed(&token) { store.join_by_token(&hash_secret(&token)).await.back("/join")? } else { None };
    let Some(join) = join else { return Ok(see("/join")) };
    let Some(releases) = s.releases.clone().filter(|_| open(&s)) else {
        return Err(UiError::invalid_at("Joining online isn't open right now.", &back));
    };
    // `None`: already sent, still sending, or another tap won; their page
    // shows which.
    if let Some(job) = store.retry_join(join.id).await.back("/join")? {
        send_in_background(&s, store, releases, job, &token);
    }
    Ok(see(&back))
}

/// Sends a join's agreement without holding up their page: Documenso may be
/// asleep (half a minute from cold), and the page shows progress meanwhile.
fn send_in_background(s: &AppState, store: Store, releases: Arc<Releases>, job: JoinSend, token: &str) {
    // `?signed` marks the return from Documenso, which can beat its webhook.
    let redirect = s.public_url.as_deref().map(|base| format!("{base}/join/{token}?signed"));
    tokio::spawn(async move {
        if let Err(e) = send_agreement(&store, &releases, &job, redirect.as_deref()).await {
            tracing::error!(error = %e, join = %job.id, "couldn't send a join's welcome agreement");
            if let Err(e) = store.record_join_failed(job.id, &e).await {
                tracing::error!(error = %e, join = %job.id, "couldn't record the failed send");
            }
        }
    });
}

/// Creates the envelope (unless an earlier try did), records it, then
/// distributes it. Recording it between the two is what lets a send cut off
/// by a restart finish on the next try without a second envelope.
async fn send_agreement(
    store: &Store,
    releases: &Releases,
    job: &JoinSend,
    redirect: Option<&str>,
) -> Result<(), String> {
    let agreement = releases.spec.release(WELCOME_AGREEMENT).ok_or("the welcome agreement isn't configured")?;
    let signer = documenso::Signer { name: &job.name, email: &job.email };
    let values = BTreeMap::new();
    let envelope = match &job.envelope_id {
        Some(envelope) => envelope.clone(),
        None => {
            let envelope = releases.create(agreement, signer, &values, redirect, job.release_id).await?;
            store
                .record_join_envelope(job.id, &envelope)
                .await
                .map_err(|e| format!("created {envelope} but couldn't record it: {e}"))?;
            envelope
        }
    };
    let sent = releases.distribute(envelope, &job.email).await?;
    let row = SentRelease {
        id: job.release_id,
        release_key: &agreement.key,
        title: &documenso::document_title(agreement, &job.name, &values),
        signer_name: &job.name,
        signer_email: &job.email,
        envelope_id: &sent.envelope,
        signing_url: sent.signing_url.as_deref(),
    };
    // Documenso has it; a retry distributes the same envelope again.
    store.record_join_sent(job.id, &row).await.map_err(|e| format!("sent but couldn't be recorded: {e}"))?;
    Ok(())
}

/// Where someone's join stands, and what to do next.
enum Step {
    Preparing,
    Failed,
    Sign(String),
    Declined,
    /// Signed; connect the exact Discord account that should receive access.
    ConnectDiscord(String),
    /// Signed and connected.
    Joined(String),
}

#[derive(Deserialize)]
struct Progress {
    /// Present when Documenso sent them back after signing.
    signed: Option<String>,
}

async fn progress(State(s): State<AppState>, Path(token): Path<String>, Query(q): Query<Progress>) -> UiResult {
    let store = s.public_store("join page");
    let join = if well_formed(&token) { store.join_by_token(&hash_secret(&token)).await.back("/join")? } else { None };
    let Some(join) = join else {
        return Ok(layout(
            "Join us",
            html! {
                p.warn { "We couldn't find that link. If you just filled in the form, give it a minute and refresh." }
                p { a href="/join" { "Start again" } }
            },
        )
        .into_response());
    };
    let step = step(&s, &join, &token);
    // Back from signing before Documenso's webhook: don't offer the button
    // again, which invites a second signature; check again shortly.
    let confirming = q.signed.is_some() && matches!(step, Step::Sign(_));
    let waiting = matches!(step, Step::Preparing | Step::Sign(_));
    Ok(layout(
        "Join us",
        html! {
            @if confirming { meta http-equiv="refresh" content="3"; }
            @else if waiting { meta http-equiv="refresh" content="20"; }
            p { "Hi " (join.name) "!" }
            @if confirming {
                p role="status" { "Thanks! Confirming your signature… this page updates in a few seconds." }
            } @else {
            @match &step {
                Step::Preparing => p role="status" { "Getting your welcome agreement ready… this can take up to a minute. \
                                                       This page updates by itself." },
                Step::Failed => {
                    p.warn { "Sorry, we couldn't prepare your agreement just now." }
                    form method="post" action={ "/join/" (token) "/retry" } {
                        button { "Try again" }
                    }
                    p.muted { "Still stuck? Message us on Instagram and we'll get you in." }
                }
                Step::Sign(url) => {
                    p { "Here's what the welcome agreement says, in plain words:" }
                    ul { @for line in PLAIN_WORDS { li { (line) } } }
                    p { "Please read it all before you sign, and ask a Steward about anything unclear. You'll \
                         initial the boxed release, add an emergency contact, and sign once." }
                    p { a.button href=(url) { "Read and sign (about 5 minutes)" } }
                    p.muted { "This link is just for you. Already signed? This page checks again in a moment." }
                }
                Step::Declined => p.warn { "You declined the agreement, so we can't add you to Discord. \
                                            If that was a mistake, " a href="/join" { "start again" } "." },
                Step::ConnectDiscord(url) => {
                    p.ok { "Signed, thank you! 🌞" }
                    p { a.button href=(url) { "Connect Discord and join" } }
                    p.muted { "Discord will ask you to approve your identity and server access. Then the Volunteer \
                               channels open automatically—no command or username needed." }
                }
                Step::Joined(username) => {
                    p.ok { "You're in! Welcome, @" (username) " 🌞" }
                    p { "Open Discord to see the Volunteer channels." }
                }
            }
            }
        },
    )
    .into_response())
}

/// The welcome agreement's summary, shown before someone opens it. It is
/// the "In plain words" box in legal/onboarding.typ, word for word, so the
/// page can't promise more (or less) than the document; a test checks.
const PLAIN_WORDS: [&str; 4] = [
    "You're volunteering. You can say no to any task, or stop, at any time, and we'll show you how to do things safely.",
    "Farm work has real risks. If you get hurt, even through our ordinary carelessness, you agree not to sue us. \
     That doesn't cover gross negligence or harm done on purpose.",
    "We may share photos and video of our days together, but never a child's full name or school. You can ask us \
     to stop.",
    "We keep each other's private information private.",
];

fn step(s: &AppState, join: &Join, token: &str) -> Step {
    let Some(request) = &join.request else {
        return if join.send_error.is_some() || join.stalled { Step::Failed } else { Step::Preparing };
    };
    match request.status.as_str() {
        "declined" => return Step::Declined,
        "signed" => {}
        _ => return request.signing_url.clone().map_or(Step::Preparing, Step::Sign),
    }
    if let Some((_, username)) = &join.discord {
        return Step::Joined(username.clone());
    }
    let Some(discord) = s.discord_join.as_deref() else { return Step::Preparing };
    let query = form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &discord.application_id)
        .append_pair("redirect_uri", &discord.redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", "identify guilds.join")
        .append_pair("state", token)
        .finish();
    Step::ConnectDiscord(format!("https://discord.com/oauth2/authorize?{query}"))
}

#[derive(Deserialize)]
struct DiscordCallback {
    /// Absent when the person pressed Cancel on Discord's screen.
    code: Option<String>,
    state: String,
}

async fn discord_callback(State(s): State<AppState>, Query(q): Query<DiscordCallback>) -> UiResult {
    let back = format!("/join/{}", q.state);
    let store = s.public_store("join page");
    let join =
        if well_formed(&q.state) { store.join_by_token(&hash_secret(&q.state)).await.back("/join")? } else { None };
    let Some(join) = join.filter(Join::signed) else {
        return Err(UiError::invalid("That join link is invalid or the agreement is not signed.", "/join"));
    };
    let Some(discord) = s.discord_join.as_deref() else {
        return Err(UiError::invalid("Discord joining is temporarily unavailable.", "/join"));
    };
    // Cancelled on Discord: back to their page, where the button still is.
    let Some(code) = q.code else { return Ok(see(&back)) };
    // Discord's error bodies are for us, not for someone joining from Instagram.
    let fail = |e: String| {
        tracing::warn!(error = %e, join = %join.id, "Discord join didn't finish");
        UiError::invalid_at("Discord didn't finish connecting. Please try again, or message us on Instagram.", &back)
    };
    let grant = discord
        .reader
        .oauth_identify(&discord.application_id, &discord.client_secret, &discord.redirect_uri, &code)
        .await
        .map_err(fail)?;
    // Claim before adding, so a second account can't ride the same signed
    // agreement into the server.
    store.claim_join_discord(join.id, &grant.user.id, &grant.user.username).await.back("/join")?;
    if let Err(e) = discord.reader.add_joiner(&grant, join.role.as_str()).await {
        if let Err(e) = store.release_join_discord(join.id, &grant.user.id).await {
            tracing::error!(error = %e, join = %join.id, "couldn't release a failed Discord claim");
        }
        return Err(fail(e));
    }
    Ok(see(&back))
}

/// The address to share, when `[server] public_url` is set.
pub fn link(s: &AppState) -> Option<String> {
    s.public_url.as_deref().map(|b| format!("{b}/join"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_join_page_summary_is_the_agreements_own() {
        let source = include_str!("../../../legal/onboarding.typ").replace('*', "");
        let start = source.find("#plain-words[").expect("the agreement has a plain-words summary");
        let end = start + source[start..].find("\n]").expect("summary ends");
        let bullets: Vec<&str> = source[start..end].lines().filter_map(|l| l.strip_prefix("- ")).collect();
        assert_eq!(bullets, super::PLAIN_WORDS);
    }
}

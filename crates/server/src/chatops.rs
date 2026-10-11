//! Discord slash commands: `/tasks`, `/lowstock`, `/task title:… priority:…`,
//! and `/onboard`, which sends a newcomer the welcome agreement.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use homestead_domain::auth::{Capability, CapabilitySet, Role};
use homestead_domain::onboarding::OnboardingRole;
use homestead_domain::{NewTask, TaskPriority};
use homestead_integrations::discord::{self, APPLICATION_COMMAND, Interaction, PING};
use homestead_integrations::documenso;
use homestead_store::releases::{Joiner, SentRelease};
use homestead_store::{Actor, Store};
use serde_json::Value;

use crate::auth::{Principal, Via};
use crate::releases;
use crate::{AppState, status_of};

/// Command names this handler answers; tested against the registered set.
pub const HANDLED: &[&str] = &["tasks", "lowstock", "task", "onboard"];

pub async fn interactions(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(verifier) = s.discord.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let h = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default();
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    if let Err(e) = verifier.verify(h("x-signature-ed25519"), h("x-signature-timestamp"), &body, now) {
        tracing::warn!(error = %e, "rejected Discord interaction");
        return (StatusCode::UNAUTHORIZED, "invalid request signature").into_response();
    }
    let Ok(interaction) = serde_json::from_slice::<Interaction>(&body) else {
        return (StatusCode::BAD_REQUEST, "malformed interaction").into_response();
    };
    Json(respond(&s, interaction).await).into_response()
}

/// Who is asking from Discord, and what they may do. A member linked to a
/// Homestead account (`discord:<user id>`) acts as that account; anyone else
/// in the (invite-only) server gets volunteer capabilities, which is what
/// these commands have always allowed, and is named in the audit log by
/// their Discord username.
async fn caller(s: &AppState, i: &Interaction) -> Result<(CapabilitySet, Store), Value> {
    let Some(user) = i.invoker() else {
        return Err(discord::reply("I couldn't tell who asked.", true));
    };
    match s.system_store.actor_by_external_subject(&format!("discord:{}", user.id)).await {
        Ok(Some(actor)) if actor.disabled_at.is_none() => {
            let who = Principal::new(actor, Via::Discord);
            Ok((who.caps, who.store(&s.system_store)))
        }
        Ok(Some(_)) => Err(discord::reply("Your Homestead account is disabled.", true)),
        Ok(None) => Ok((
            Role::Volunteer.capabilities(),
            s.system_store.acting_as(Actor { id: None, label: format!("discord:{}", user.username) }),
        )),
        Err(e) => Err(failure(&e)),
    }
}

async fn respond(s: &AppState, i: Interaction) -> Value {
    if i.kind == PING {
        return discord::pong();
    }
    // Anyone in the (invite-only) server may join: newcomers hold no role
    // and have no Homestead account yet.
    if i.kind == APPLICATION_COMMAND && i.data.as_ref().is_some_and(|c| c.name == "onboard") {
        return onboard(s, i).await;
    }
    let (caps, store) = match caller(s, &i).await {
        Ok(c) => c,
        Err(reply) => return reply,
    };
    let needs = |c: Capability| discord::reply(&format!("Your account can't do that (it needs {c})."), true);
    match (i.kind, i.data) {
        (APPLICATION_COMMAND, Some(cmd)) => match cmd.name.as_str() {
            "tasks" if caps.has(Capability::TasksRead) => list_tasks(&store).await,
            "tasks" => needs(Capability::TasksRead),
            "lowstock" if caps.has(Capability::InventoryRead) => low_stock(&store).await,
            "lowstock" => needs(Capability::InventoryRead),
            "task" if caps.has(Capability::TasksWrite) => {
                add_task(&store, cmd.str_option("title"), cmd.str_option("priority")).await
            }
            "task" => needs(Capability::TasksWrite),
            other => discord::reply(&format!("I don't know the command `/{other}`."), true),
        },
        _ => discord::reply("Unsupported interaction.", true),
    }
}

/// `/onboard name email [joining_as]`: send the welcome agreement and reply,
/// to the asker only, with its signing link. Signing it grants the role
/// (`member.onboarded`). Someone who already signed gets the role at once;
/// someone with a agreement still out gets that one's link again, so asking
/// twice doesn't create two envelopes.
async fn onboard(s: &AppState, i: Interaction) -> Value {
    let Some(user) = i.invoker().cloned() else {
        return discord::reply("I couldn't tell who asked.", true);
    };
    let Some(releases) = s.releases.clone().filter(|r| r.spec.approved) else {
        return discord::reply("Onboarding isn't switched on yet. Please ask a Steward.", true);
    };
    let Some(agreement) = releases.spec.release(releases::WELCOME_AGREEMENT).cloned() else {
        return discord::reply("There's no welcome agreement set up yet. Please ask a Steward.", true);
    };
    let cmd = i.data.as_ref().expect("checked by the caller");
    let name = cmd.str_option("name").map(str::trim).unwrap_or_default().to_owned();
    let email = cmd.str_option("email").map(str::trim).unwrap_or_default().to_owned();
    let role = match cmd.str_option("joining_as").unwrap_or("Volunteer").parse::<OnboardingRole>() {
        Ok(r) => r,
        Err(e) => return discord::reply(&e.to_string(), true),
    };
    if name.is_empty() || !email.contains('@') {
        return discord::reply("Please give your full name and your email address.", true);
    }
    let store = s.system_store.acting_as(Actor { id: None, label: format!("discord:{}", user.username) });
    let who = Joiner { discord_user_id: &user.id, discord_username: &user.username, role };
    match store.onboarding(&user.id).await {
        Err(e) => return failure(&e),
        Ok(Some(o)) if o.completed_at.is_some() => {
            return match store.choose_onboarded_role(who).await {
                Ok(_) => discord::reply(
                    &format!(
                        "You've already signed the welcome agreement, so you're all set: giving you the {role} role now."
                    ),
                    true,
                ),
                Err(e) => failure(&e),
            };
        }
        Ok(Some(o)) => {
            if let Some(url) = o.latest.as_ref().filter(|r| r.status == "sent").and_then(|r| r.signing_url.as_deref()) {
                return discord::reply(&waiting_on(url, o.role), true);
            }
        }
        Ok(None) => {}
    }
    // Documenso may be asleep: answer now, send in the background, and
    // edit the answer when the link is ready.
    let (s, application_id, token) = (s.clone(), i.application_id.clone(), i.token.clone());
    tokio::spawn(async move {
        let text = match send_agreement(&releases, &agreement, &store, &user, &name, &email, role).await {
            Ok(url) => waiting_on(&url, role),
            Err(e) => {
                tracing::error!(error = %e, user = user.id, "onboarding agreement failed");
                "Sorry, I couldn't prepare your welcome agreement just now. Please try /onboard again in a few \
                 minutes, or ask a Steward."
                    .to_owned()
            }
        };
        let http = homestead_integrations::http_client();
        if let Err(e) = discord::edit_reply(&http, &s.discord_api, &application_id, &token, &text).await {
            tracing::error!(error = %e, "couldn't post the onboarding reply");
        }
    });
    discord::deferred(true)
}

fn waiting_on(url: &str, role: OnboardingRole) -> String {
    format!(
        "👋 Welcome! Here's your welcome agreement: {url}\n\
         It starts with a short summary in plain words, then covers how we work together, the liability \
         release (you'll initial it), photo permission and confidentiality. Add an emergency contact and sign \
         once; it takes about five minutes and works on a phone. \
         This link is only for you, so please don't share it.\n\
         As soon as it's signed you'll get the **{role}** role and the channels will open."
    )
}

async fn send_agreement(
    releases: &releases::Releases,
    agreement: &documenso::Release,
    store: &Store,
    user: &discord::User,
    name: &str,
    email: &str,
    role: OnboardingRole,
) -> Result<String, String> {
    let signer = documenso::Signer { name, email };
    let d = releases.dispatch(agreement, signer, &Default::default(), None).await?;
    let sent = SentRelease {
        id: d.id,
        release_key: &agreement.key,
        title: &d.title,
        signer_name: name,
        signer_email: email,
        envelope_id: &d.sent.envelope,
        signing_url: d.sent.signing_url.as_deref(),
    };
    let who = Joiner { discord_user_id: &user.id, discord_username: &user.username, role };
    store.record_onboarding_sent(&sent, who).await.map_err(|e| status_of(&e).1)?;
    d.sent.signing_url.ok_or_else(|| "Documenso returned no signing link".to_owned())
}

fn failure(e: &homestead_store::StoreError) -> Value {
    discord::reply(&format!("Couldn't do that: {}", status_of(e).1), true)
}

async fn list_tasks(store: &Store) -> Value {
    match store.list_open_tasks().await {
        Ok(tasks) if tasks.is_empty() => discord::reply("No open tasks. 🌞", true),
        Ok(tasks) => {
            let mut lines: Vec<String> = tasks
                .iter()
                .take(15)
                .map(|t| {
                    let due = t.due_on.map(|d| format!(" — due {d}")).unwrap_or_default();
                    format!("• [{}] {}{due}", t.priority, t.title)
                })
                .collect();
            if tasks.len() > 15 {
                lines.push(format!("…and {} more", tasks.len() - 15));
            }
            discord::reply(&format!("**Open tasks ({})**\n{}", tasks.len(), lines.join("\n")), true)
        }
        Err(e) => failure(&e),
    }
}

async fn low_stock(store: &Store) -> Value {
    match store.list_inventory().await {
        Ok(items) => {
            let low: Vec<String> = items
                .iter()
                .filter(|i| i.is_low())
                .map(|i| {
                    format!(
                        "• {}: {} {} (reorder at {})",
                        i.name,
                        i.on_hand.normalize(),
                        i.unit,
                        i.reorder_level.normalize()
                    )
                })
                .collect();
            if low.is_empty() {
                discord::reply("Nothing is low. 📦", true)
            } else {
                discord::reply(&format!("**Low stock**\n{}", low.join("\n")), true)
            }
        }
        Err(e) => failure(&e),
    }
}

async fn add_task(store: &Store, title: Option<&str>, priority: Option<&str>) -> Value {
    let priority = match priority.map(str::parse::<TaskPriority>) {
        None => TaskPriority::Normal,
        Some(Ok(p)) => p,
        Some(Err(e)) => return discord::reply(&e.to_string(), true),
    };
    let input = NewTask {
        title: title.unwrap_or_default().to_owned(),
        details: String::new(),
        location_id: None,
        due_on: None,
        priority,
        mirror_to_linear: false,
        assigned_to: None,
    };
    match store.create_task(input).await {
        // Ephemeral: the webhook sink already broadcasts `task.created` to
        // the notifications channel; a public reply would say it twice.
        Ok(t) => discord::reply(&format!("📝 Added {} task: {}", t.priority, t.title), true),
        Err(e) => failure(&e),
    }
}

//! The homestead HTTP server.
//!
//! - `/` …        server-rendered UI (per-person session cookie)
//! - `/login`, `/setup/{token}`  sign-in and one-time password setup
//! - `/api/v1/…`  JSON API for other systems (service bearer token)
//! - `/discord/interactions`  Discord slash commands (Ed25519 signature)
//! - `/hooks/documenso`       e-signature outcomes (shared secret)
//! - `/healthz`   liveness + database reachability (public)

pub mod adopt;
pub mod animals;
pub mod api;
pub mod attachment_backup;
pub mod attachments;
pub mod auth;
pub mod carry;
pub mod chatops;
pub mod community;
pub mod config;
pub mod contacts;
pub mod crops;
pub mod envfile;
pub mod equipment;
pub mod farm;
pub mod feeds;
pub mod gifts;
pub mod grants;
pub mod harvests;
pub mod health;
pub mod idempotency;
pub mod incidents;
pub mod join;
pub mod legal;
pub mod linear_pull;
pub mod map;
pub mod movements;
pub mod observe;
pub mod people;
pub mod purchasing;
pub mod pwa;
pub mod readings;
pub mod releases;
pub mod reports;
pub mod routines;
pub mod search;
pub mod seeds;
pub mod soil;
pub mod stand;
pub mod tags;
pub mod today;
pub mod ui;
pub mod water;
pub mod zeffy;

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use homestead_integrations::discord::InteractionVerifier;
use homestead_store::{Store, StoreError};

#[derive(Clone)]
pub struct AppState {
    /// Writes as `system`. Handlers acting for a person get their store from
    /// [`auth::Can`] instead; only webhooks and sign-in use this directly
    /// (`only_unauthenticated_paths_use_the_system_store` checks).
    pub system_store: Store,
    /// The legacy shared API credential, if configured.
    pub admin_token: Option<Arc<str>>,
    /// The service account `admin_token` acts as, resolved on first use.
    pub admin_actor: Arc<std::sync::OnceLock<homestead_domain::auth::ActorRecord>>,
    pub secure_cookies: bool,
    pub discord: Option<Arc<InteractionVerifier>>,
    /// Documenso releases, when `[documenso]` is enabled.
    pub releases: Option<Arc<releases::Releases>>,
    /// Grant email delivery. Drafting and previews work when this is absent.
    pub grant_delivery: Option<Arc<grants::GrantDelivery>>,
    pub zeffy_webhook_secret: Option<Arc<str>>,
    /// Attachment storage; uploads are refused with an explanation when off.
    pub attachments: Option<attachments::Storage>,
    /// Pulls Linear issues in as tasks, when `[linear]` is on and pulling.
    pub linear_pull: Option<Arc<linear_pull::LinearPull>>,
    /// Discord's feed channels and events, for the Community page.
    pub discord_feed: Option<Arc<feeds::DiscordFeed>>,
    /// Signed-paperwork Discord OAuth continuation for the public join page.
    pub discord_join: Option<Arc<join::DiscordJoin>>,
    /// Buffer's channels, posts and drafts, for the Community page.
    pub social_feed: Option<Arc<feeds::SocialFeed>>,
    /// Where people reach the app (`[server] public_url`): what printed QR
    /// codes point at. Falls back to the request's host.
    pub public_url: Option<Arc<str>>,
    /// `[stand] pay_links`: (label, URL) for the stand sign's QR codes.
    pub pay_links: Arc<[(String, String)]>,
    /// Discord's API root, where deferred slash-command replies are edited
    /// (a mock in tests).
    pub discord_api: Arc<str>,
}

impl AppState {
    /// The store a background job writes with, attributed to `label` (the
    /// `scheduler`, the `linear` pull): no person asked, so no `Can<…>`.
    pub fn job_store(&self, label: &str) -> Store {
        self.system_store.acting_as(homestead_store::Actor { id: None, label: label.into() })
    }

    /// The store a QR-tag action writes with: as the signed-in person when
    /// their cookie came along, otherwise as the name typed at the tag. The
    /// tag's code is the credential (see [`tags`]), so no capability is
    /// checked; each tag action is small and bounded instead.
    pub fn tag_store(&self, who: Option<&auth::Principal>, typed_name: &str, code: &str) -> Store {
        match who {
            Some(p) => p.store(&self.system_store),
            None => self
                .system_store
                .acting_as(homestead_store::Actor { id: None, label: format!("{typed_name} (tag {code})") }),
        }
    }

    /// The store a public page (no account at all) writes with, attributed
    /// to `label`. Its handlers take no `Can<…>`; each checks and bounds
    /// its own input instead (see `adopt`).
    pub fn public_store(&self, label: &str) -> Store {
        self.system_store.acting_as(homestead_store::Actor { id: None, label: label.into() })
    }

    /// A state with every optional integration off.
    pub fn new(store: Store) -> Self {
        Self {
            system_store: store,
            admin_token: None,
            admin_actor: Arc::default(),
            secure_cookies: true,
            discord: None,
            releases: None,
            grant_delivery: None,
            zeffy_webhook_secret: None,
            attachments: None,
            linear_pull: None,
            discord_feed: None,
            discord_join: None,
            social_feed: None,
            public_url: None,
            pay_links: Arc::new([]),
            discord_api: homestead_integrations::discord::feed::API.into(),
        }
    }
}

pub fn app(state: AppState) -> Router {
    let max_upload =
        state.attachments.as_ref().map_or(homestead_domain::attachments::DEFAULT_MAX_BYTES, |a| a.max_bytes);
    let protected_ui = ui::routes()
        .merge(today::routes())
        .merge(attachments::ui_routes(max_upload))
        .merge(releases::ui_routes())
        .merge(grants::ui_routes())
        .merge(animals::ui_routes())
        .merge(gifts::ui_routes())
        .merge(community::ui_routes())
        .merge(stand::ui_routes())
        .merge(contacts::ui_routes())
        .merge(people::ui_routes())
        .merge(purchasing::ui_routes())
        .merge(routines::ui_routes())
        .merge(seeds::ui_routes())
        .merge(crops::ui_routes())
        .merge(harvests::ui_routes())
        .merge(soil::ui_routes())
        .merge(health::ui_routes())
        .merge(water::ui_routes())
        .merge(equipment::ui_routes())
        .merge(farm::ui_routes())
        .merge(map::ui_routes())
        .merge(movements::ui_routes())
        .merge(readings::ui_routes())
        .merge(incidents::ui_routes())
        .merge(reports::ui_routes())
        .merge(search::ui_routes())
        .merge(carry::ui_routes())
        .merge(tags::ui_routes())
        .merge(adopt::ui_routes())
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth::require_session));
    let protected_api = api::routes()
        .merge(attachments::api_routes(max_upload))
        .merge(purchasing::api_routes())
        .merge(routines::api_routes())
        .merge(seeds::api_routes())
        .merge(crops::api_routes())
        .merge(harvests::api_routes())
        .merge(soil::api_routes())
        .merge(health::api_routes())
        .merge(water::api_routes())
        .merge(equipment::api_routes())
        .merge(incidents::api_routes())
        .merge(reports::api_routes())
        .merge(search::api_routes())
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), idempotency::enforce))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth::require_bearer));
    Router::new()
        .merge(protected_ui)
        .nest("/api/v1", protected_api)
        .route("/login", get(auth::login_page).post(auth::login))
        .route("/logout", post(auth::logout))
        .route("/setup/{token}", get(auth::setup_page).post(auth::setup))
        .route("/discord/interactions", post(chatops::interactions))
        .route("/hooks/documenso", post(releases::webhook))
        .route("/hooks/zeffy", post(zeffy::webhook))
        .route("/healthz", get(healthz))
        .merge(pwa::routes())
        .merge(tags::public_routes())
        .merge(adopt::public_routes())
        .merge(join::public_routes())
        .merge(legal::public_routes())
        .layer(axum::middleware::from_fn(observe::http_metrics))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

/// Posts deadline reminders (see `homestead_domain::deadlines`) and, in
/// the morning, the daily digest (`homestead_domain::digest`), as the
/// `scheduler`. Run on start and hourly, not by pages: it first brings
/// Linear's due dates in, which can take seconds. A pull already running
/// holds the lock; this waits for it and then finds it fresh.
pub async fn send_reminders(state: &AppState) {
    if let Some(pull) = &state.linear_pull {
        pull.pull_if_stale(&state.job_store("linear")).await;
    }
    match state.job_store("scheduler").send_deadline_reminders().await {
        Ok(0) => {}
        Ok(n) => tracing::info!(tasks = n, "sent deadline reminders"),
        Err(e) => tracing::error!(error = %e, "deadline reminders failed"),
    }
    match state.job_store("scheduler").send_daily_digest().await {
        Ok(None) => {}
        Ok(Some(d)) => tracing::info!(summary = %d.headline(), "sent the daily digest"),
        Err(e) => tracing::error!(error = %e, "daily digest failed"),
    }
}

/// Makes any recurring tasks now due, as the `scheduler`. Cheap when
/// nothing is due (one indexed query), so pages call it before listing
/// tasks: a container that slept through a morning still shows its chores.
pub async fn run_schedules(state: &AppState) {
    let scheduler = state.job_store("scheduler");
    match scheduler.generate_recurring_tasks().await {
        Ok(0) => {}
        Ok(n) => tracing::info!(tasks = n, "made recurring tasks"),
        Err(e) => tracing::error!(error = %e, "recurring task generation failed"),
    }
    if let Some(storage) = &state.attachments {
        match attachments::purge_due(&scheduler, storage).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(files = n, "purged deleted attachments past retention"),
            Err(e) => tracing::error!(error = %e, "attachment purge failed"),
        }
    }
}

async fn healthz(axum::extract::State(s): axum::extract::State<AppState>) -> Response {
    match s.system_store.ping().await {
        Ok(()) => (StatusCode::OK, "ok").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "health check failed");
            (StatusCode::SERVICE_UNAVAILABLE, "database unreachable").into_response()
        }
    }
}

/// Maps store failures onto HTTP semantics. Internal errors are logged in
/// full but reported generically, so SQL details never reach a client.
pub fn status_of(e: &StoreError) -> (StatusCode, String) {
    match e {
        StoreError::Invalid(v) => (StatusCode::UNPROCESSABLE_ENTITY, v.to_string()),
        StoreError::NotFound(_) => (StatusCode::NOT_FOUND, e.to_string()),
        StoreError::Conflict(_) | StoreError::InsufficientStock => (StatusCode::CONFLICT, e.to_string()),
        StoreError::Corrupt(_) | StoreError::Db(_) => {
            tracing::error!(error = %e, "internal store error");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_owned())
        }
    }
}

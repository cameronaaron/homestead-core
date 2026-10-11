//! Who is asking, and may they?
//!
//! - **People** sign in with an email and password (Argon2id) and get a
//!   session cookie. The cookie holds 256 random bits; the database holds
//!   only their SHA-256. It is `HttpOnly` + `SameSite=Strict`, which is also
//!   the CSRF defence for the UI's form posts.
//! - **Services** call `/api/v1` with a bearer API token (also stored as a
//!   hash). The legacy `ADMIN_TOKEN`, if set, is one such credential, acting
//!   as the built-in `admin-token` service account.
//!
//! Authorization is by [`Capability`], checked in one place: the [`Can`]
//! extractor. A handler that takes `Can<cap::GiftsRead>` never runs for
//! someone without `gifts.read`, and the only store it gets writes as the
//! person asking, so the audit log names them.

use std::marker::PhantomData;
use std::sync::OnceLock;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use axum::Form;
use axum::extract::{FromRequestParts, Path, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use homestead_domain::auth::{ActorRecord, Capability, CapabilitySet};
use homestead_store::{Actor, Store};
use maud::html;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::Duration;

use crate::AppState;

pub const COOKIE: &str = "sbh_session";
pub const SESSION_TTL: Duration = Duration::days(30);
pub const SETUP_LINK_TTL: Duration = Duration::hours(72);

/// How the principal proved who they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Session,
    ApiToken,
    AdminToken,
    Discord,
}

/// The authenticated caller of this request.
#[derive(Debug, Clone)]
pub struct Principal {
    pub actor: ActorRecord,
    pub caps: CapabilitySet,
    pub via: Via,
}

impl Principal {
    pub fn new(actor: ActorRecord, via: Via) -> Self {
        Self { caps: actor.capabilities(), actor, via }
    }

    pub fn can(&self, c: Capability) -> bool {
        self.caps.has(c)
    }

    /// The database, writing as this principal.
    pub fn store(&self, system: &Store) -> Store {
        system.acting_as(Actor { id: Some(self.actor.id), label: self.actor.display_name.clone() })
    }
}

/// A capability named at the type level, so a handler's signature says
/// what it requires.
pub trait Cap: Send + Sync + 'static {
    const CAP: Capability;
}

/// One marker type per [`Capability`] variant.
pub mod cap {
    use super::{Cap, Capability};

    macro_rules! markers {
        ($($name:ident),+ $(,)?) => {$(
            pub struct $name;
            impl Cap for $name {
                const CAP: Capability = Capability::$name;
            }
        )+};
    }

    markers!(
        TasksRead,
        TasksWrite,
        GardenRead,
        GardenWrite,
        AnimalsRead,
        AnimalsWrite,
        AnimalsMedical,
        InventoryRead,
        InventoryAdjust,
        PurchasesRead,
        PurchasesWrite,
        EquipmentRead,
        EquipmentWrite,
        GiftsRead,
        GiftsWrite,
        ContactsRead,
        ContactsWrite,
        ContactsExport,
        CommunityRead,
        CommunityWrite,
        StandRead,
        StandWrite,
        ReleasesRead,
        ReleasesSend,
        GrantsRead,
        GrantsWrite,
        GrantsApprove,
        GrantsSend,
        IncidentsReport,
        IncidentsRead,
        IncidentsWrite,
        ReportsRead,
        AuditRead,
        SystemsRead,
        SystemsAdmin,
    );
}

/// Proof the caller holds `C`, plus a store that writes as them. Put it
/// first in a handler's arguments so it runs before any body parsing.
pub struct Can<C> {
    pub who: Principal,
    pub store: Store,
    _cap: PhantomData<C>,
}

impl<C: Cap> FromRequestParts<AppState> for Can<C> {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let who = principal(parts).map_err(IntoResponse::into_response)?;
        if !who.can(C::CAP) {
            return Err(forbidden(&who, C::CAP));
        }
        Ok(Can { store: who.store(&state.system_store), who, _cap: PhantomData })
    }
}

/// Any signed-in caller, whatever they may do.
pub struct Authed {
    pub who: Principal,
    pub store: Store,
}

impl FromRequestParts<AppState> for Authed {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let who = principal(parts).map_err(IntoResponse::into_response)?;
        Ok(Authed { store: who.store(&state.system_store), who })
    }
}

fn principal(parts: &Parts) -> Result<Principal, StatusCode> {
    // Only the authentication middleware inserts one; a route outside it
    // that asks for a capability is a wiring bug, and fails closed.
    parts.extensions.get::<Principal>().cloned().ok_or(StatusCode::UNAUTHORIZED)
}

/// The response for a caller who lacks `needed`: a page for people, JSON
/// for services.
pub fn deny(who: &Principal, needed: Capability) -> Response {
    forbidden(who, needed)
}

pub(crate) fn forbidden(who: &Principal, needed: Capability) -> Response {
    tracing::info!(actor = %who.actor.id, capability = %needed, "forbidden");
    match who.via {
        Via::Session => (
            StatusCode::FORBIDDEN,
            crate::ui::error_page(&format!("Your account can't do that (it needs {needed})."), "/"),
        )
            .into_response(),
        _ => (StatusCode::FORBIDDEN, axum::Json(serde_json::json!({ "error": "forbidden", "needs": needed })))
            .into_response(),
    }
}

// ── Secrets ────────────────────────────────────────────────────────────────

/// A fresh 256-bit secret, hex-encoded, and the hash to store for it.
pub fn new_secret() -> (String, String) {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS random source is available");
    let plain = hex::encode(bytes);
    let hash = hash_secret(&plain);
    (plain, hash)
}

pub fn hash_secret(plain: &str) -> String {
    hex::encode(Sha256::digest(plain.as_bytes()))
}

pub fn hash_password(pw: &str) -> String {
    Argon2::default().hash_password(pw.as_bytes()).expect("argon2 accepts any password").to_string()
}

pub fn verify_password(pw: &str, hash: &str) -> bool {
    hash.parse::<PasswordHash>().is_ok_and(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
}

/// Checked against when the email is unknown, so a wrong address costs the
/// same time as a wrong password and doesn't reveal who has an account.
fn dummy_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hash_password("not anyone's password"))
}

/// Argon2 is deliberately slow; keep it off the async workers.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.expect("password hashing does not panic")
}

// ── Middleware ─────────────────────────────────────────────────────────────

fn cookie_value(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| kv.trim().strip_prefix(COOKIE)?.strip_prefix('='))
        .filter(|v| !v.is_empty())
}

tokio::task_local! {
    /// The signed-in person, for page chrome (navigation, "signed in as").
    pub static VIEWER: Principal;
    /// The path of this request, for the current-page mark in the nav.
    pub static PATH: String;
}

/// UI routes: a valid session, or off to `/login`.
pub async fn require_session(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let Some(token) = cookie_value(req.headers()) else { return Redirect::to("/login").into_response() };
    match state.system_store.session_actor(&hash_secret(token)).await {
        Ok(Some(actor)) => {
            let who = Principal::new(actor, Via::Session);
            req.extensions_mut().insert(who.clone());
            let path = req.uri().path().to_owned();
            VIEWER.scope(who, PATH.scope(path, next.run(req))).await
        }
        Ok(None) => Redirect::to("/login").into_response(),
        Err(e) => crate::status_of(&e).into_response(),
    }
}

/// The signed-in person behind a request outside the session middleware
/// (a QR tag page), when their cookie came along. Never required there: a
/// tag works for anyone holding it, and this only decides whose name an
/// action goes under.
pub async fn viewer(state: &AppState, headers: &HeaderMap) -> Option<Principal> {
    let token = cookie_value(headers)?;
    match state.system_store.session_actor(&hash_secret(token)).await {
        Ok(actor) => actor.map(|a| Principal::new(a, Via::Session)),
        Err(e) => {
            tracing::error!(error = %e, "session lookup on a tag page failed");
            None
        }
    }
}

/// API routes: an API token (or the legacy `ADMIN_TOKEN`) as a bearer.
pub async fn require_bearer(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let presented =
        req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
    let unauthorized = || (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Bearer")], "unauthorized");
    let Some(presented) = presented.map(str::trim).filter(|t| !t.is_empty()) else {
        return unauthorized().into_response();
    };
    let found = match state.admin_token.as_deref() {
        // Constant-time: a timing side channel must not leak the token prefix.
        Some(admin) if bool::from(presented.as_bytes().ct_eq(admin.as_bytes())) => {
            admin_token_actor(&state).await.map(|a| Some((a, Via::AdminToken)))
        }
        _ => state.system_store.api_token_actor(&hash_secret(presented)).await.map(|a| a.map(|a| (a, Via::ApiToken))),
    };
    match found {
        Ok(Some((actor, via))) => {
            req.extensions_mut().insert(Principal::new(actor, via));
            next.run(req).await
        }
        Ok(None) => unauthorized().into_response(),
        Err(e) => crate::status_of(&e).into_response(),
    }
}

async fn admin_token_actor(state: &AppState) -> homestead_store::Result<ActorRecord> {
    if let Some(a) = state.admin_actor.get() {
        return Ok(a.clone());
    }
    let a = state.system_store.admin_token_actor().await?;
    Ok(state.admin_actor.get_or_init(|| a).clone())
}

// ── Sign-in pages ──────────────────────────────────────────────────────────

pub async fn login_page() -> maud::Markup {
    crate::ui::layout(
        "Sign in",
        html! {
            form.card.narrow method="post" action="/login" {
                label { "Email" input type="email" name="email" autocomplete="username" required; }
                label { "Password" input type="password" name="password" autocomplete="current-password" required; }
                button { "Sign in" }
                p.muted { "No password yet, or forgot it? Ask a steward for a setup link." }
            }
        },
    )
}

#[derive(Deserialize)]
pub struct LoginForm {
    email: String,
    password: String,
}

fn session_cookie(state: &AppState, token: &str) -> String {
    let secure = if state.secure_cookies { "; Secure" } else { "" };
    let max_age = SESSION_TTL.whole_seconds();
    format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}")
}

/// Starts a session for someone who just proved who they are.
async fn sign_in(state: &AppState, actor: &ActorRecord) -> Response {
    let (token, hash) = new_secret();
    let who = Principal::new(actor.clone(), Via::Session);
    if let Err(e) = who.store(&state.system_store).start_session(actor.id, &hash, SESSION_TTL).await {
        return crate::status_of(&e).into_response();
    }
    let dest = if actor.kind == homestead_domain::auth::ActorKind::Human && actor.welcomed_at.is_none() {
        "/welcome"
    } else {
        "/"
    };
    ([(header::SET_COOKIE, session_cookie(state, &token))], Redirect::to(dest)).into_response()
}

pub async fn login(State(state): State<AppState>, Form(f): Form<LoginForm>) -> Response {
    let wrong = || {
        (StatusCode::UNAUTHORIZED, crate::ui::error_page("That email and password don't match.", "/login"))
            .into_response()
    };
    let candidate = match state.system_store.login_candidate(&f.email).await {
        Ok(c) => c,
        Err(e) => return crate::status_of(&e).into_response(),
    };
    let Some(c) = candidate else {
        let pw = f.password;
        blocking(move || verify_password(&pw, dummy_hash())).await;
        return wrong();
    };
    let (pw, hash) = (f.password, c.password_hash.clone());
    let ok = blocking(move || verify_password(&pw, &hash)).await;
    if c.locked_until.is_some_and(|t| t > time::OffsetDateTime::now_utc()) {
        let msg = "Too many wrong passwords. Try again in 15 minutes, or ask a steward for a setup link.";
        return (StatusCode::TOO_MANY_REQUESTS, crate::ui::error_page(msg, "/login")).into_response();
    }
    if !ok || c.actor.disabled_at.is_some() {
        if let Err(e) = state.system_store.record_login_failure(c.actor.id).await {
            tracing::error!(error = %e, "could not record a failed sign-in");
        }
        return wrong();
    }
    sign_in(&state, &c.actor).await
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = cookie_value(&headers)
        && let Err(e) = state.system_store.end_session(&hash_secret(token)).await
    {
        tracing::error!(error = %e, "could not end a session");
    }
    let cookie = format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    ([(header::SET_COOKIE, cookie)], Redirect::to("/login")).into_response()
}

/// The link is the credential: keep it out of other sites' logs.
const NO_REFERRER: (header::HeaderName, &str) = (header::REFERRER_POLICY, "no-referrer");

pub async fn setup_page(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    match state.system_store.setup_link_actor(&hash_secret(&token)).await {
        Ok(Some(actor)) => (
            [NO_REFERRER],
            crate::ui::layout(
                "Set your password",
                html! {
                    form.card.narrow method="post" action={"/setup/" (token)} {
                        p { "Hi " (actor.display_name) ". You'll sign in as " strong { (actor.email.unwrap_or_default()) } "." }
                        label { "New password (12 characters or more; a few words work well)"
                            input type="password" name="password" autocomplete="new-password" minlength="12" required; }
                        label { "Same again" input type="password" name="confirm" autocomplete="new-password" required; }
                        button { "Set password and sign in" }
                    }
                },
            ),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            [NO_REFERRER],
            crate::ui::error_page("This link has expired or was already used. Ask a steward for a new one.", "/login"),
        )
            .into_response(),
        Err(e) => crate::status_of(&e).into_response(),
    }
}

#[derive(Deserialize)]
pub struct SetupForm {
    password: String,
    confirm: String,
}

pub async fn setup(State(state): State<AppState>, Path(token): Path<String>, Form(f): Form<SetupForm>) -> Response {
    let back = format!("/setup/{token}");
    let invalid =
        |m: &str| (StatusCode::UNPROCESSABLE_ENTITY, [NO_REFERRER], crate::ui::error_page(m, &back)).into_response();
    if f.password != f.confirm {
        return invalid("The two passwords don't match.");
    }
    if let Err(e) = homestead_domain::auth::check_password(&f.password) {
        return invalid(&e.to_string());
    }
    let hash = hash_secret(&token);
    let who = match state.system_store.setup_link_actor(&hash).await {
        Ok(Some(a)) => a,
        Ok(None) => return invalid("This link has expired or was already used."),
        Err(e) => return crate::status_of(&e).into_response(),
    };
    let pw = f.password;
    let password_hash = blocking(move || hash_password(&pw)).await;
    let store = Principal::new(who, Via::Session).store(&state.system_store);
    match store.redeem_setup_link(&hash, &password_hash).await {
        Ok(actor) => sign_in(&state, &actor).await,
        Err(e) => crate::status_of(&e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_random_and_only_their_hash_is_kept() {
        let (a, ha) = new_secret();
        let (b, _) = new_secret();
        assert_ne!(a, b);
        assert_eq!((a.len(), ha.len()), (64, 64));
        assert_eq!(hash_secret(&a), ha);
        assert_ne!(a, ha);
    }

    #[test]
    fn passwords_hash_with_argon2id_and_verify() {
        let h = hash_password("correct horse battery");
        assert!(h.starts_with("$argon2id$"), "{h}");
        assert!(verify_password("correct horse battery", &h));
        assert!(!verify_password("correct horse batterY", &h));
        assert!(!verify_password("anything", "not a hash"));
        assert_ne!(h, hash_password("correct horse battery"), "salted");
    }
}

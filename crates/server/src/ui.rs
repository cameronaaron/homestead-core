//! Server-rendered UI. Plain forms + POST/redirect/GET: works on a phone in
//! the field with no JavaScript. Maud escapes every interpolated value.

use axum::Form;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use homestead_domain::auth::Capability;
use homestead_domain::inventory::RUNS_OUT_SOON_DAYS;
use homestead_domain::{
    HarvestUnit, LocationKind, NewHarvest, NewInventoryItem, NewLocation, NewPlanting, NewTask, PlantingStatus,
    StockAdjustment, TaskPriority, ValidationError,
};
use homestead_store::StoreError;
use maud::{DOCTYPE, Markup, html};
use rust_decimal::Decimal;
use serde::Deserialize;
use time::Date;
use time::macros::{date, format_description};
use uuid::Uuid;

use crate::auth::{Can, cap};
use crate::{AppState, status_of};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/tasks", get(tasks_page).post(create_task))
        .route("/tasks/{id}/complete", post(complete_task))
        .route("/tasks/{id}/assign", post(assign_task))
        .route("/garden", get(garden_page))
        .route("/locations", post(create_location))
        .route("/locations/{id}", get(location_page).post(update_location))
        .route("/plantings", post(create_planting))
        .route("/plantings/{id}/status", post(set_planting_status))
        .route("/harvests", post(log_harvest))
        .route("/inventory", get(inventory_page).post(create_item))
        .route("/inventory/{id}/adjust", post(adjust_stock))
        .route("/ops", get(ops_page))
        .route("/ops/social", get(social_page))
        .route("/ops/retry/{event_id}/{sink}", post(retry_delivery))
}

// ── Errors ───────────────────────────────────────────────────────────────────

pub(crate) struct UiError {
    status: StatusCode,
    message: String,
    back: std::borrow::Cow<'static, str>,
}

impl UiError {
    fn store(e: StoreError, back: std::borrow::Cow<'static, str>) -> Self {
        let (status, message) = status_of(&e);
        Self { status, message, back }
    }
    pub(crate) fn invalid(message: impl Into<String>, back: &'static str) -> Self {
        Self { status: StatusCode::UNPROCESSABLE_ENTITY, message: message.into(), back: back.into() }
    }
    /// The same error for an API caller: `{"error": message}`.
    pub(crate) fn into_json(self) -> Response {
        (self.status, axum::Json(serde_json::json!({ "error": self.message }))).into_response()
    }
    /// Like [`UiError::invalid`], going back to a page built at runtime.
    pub(crate) fn invalid_at(message: impl Into<String>, back: &str) -> Self {
        Self { status: StatusCode::UNPROCESSABLE_ENTITY, message: message.into(), back: back.to_owned().into() }
    }
}

impl IntoResponse for UiError {
    fn into_response(self) -> Response {
        (self.status, error_page(&self.message, &self.back)).into_response()
    }
}

pub(crate) type UiResult = Result<Response, UiError>;

pub(crate) trait Back<T> {
    fn back(self, to: &'static str) -> Result<T, UiError>;
    /// Going back to a page built at runtime, like `/inventory/{id}`.
    fn back_to(self, to: &str) -> Result<T, UiError>;
}

impl<T> Back<T> for Result<T, StoreError> {
    fn back(self, to: &'static str) -> Result<T, UiError> {
        self.map_err(|e| UiError::store(e, to.into()))
    }
    fn back_to(self, to: &str) -> Result<T, UiError> {
        self.map_err(|e| UiError::store(e, to.to_owned().into()))
    }
}

impl<T> Back<T> for Result<T, ValidationError> {
    fn back(self, to: &'static str) -> Result<T, UiError> {
        self.map_err(|e| UiError::invalid(e.to_string(), to))
    }
    fn back_to(self, to: &str) -> Result<T, UiError> {
        self.map_err(|e| UiError::invalid_at(e.to_string(), to))
    }
}

// ── Form field parsing (HTML sends "" for empty optional inputs) ────────────

pub(crate) fn opt_date(s: &str, field: &str, back: &'static str) -> Result<Option<Date>, UiError> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    Date::parse(s, format_description!("[year]-[month]-[day]"))
        .map(Some)
        .map_err(|_| UiError::invalid(format!("{field}: expected a date like 2026-03-01"), back))
}

pub(crate) fn req_date(s: &str, field: &str, back: &'static str) -> Result<Date, UiError> {
    opt_date(s, field, back)?.ok_or_else(|| UiError::invalid(format!("{field} is required"), back))
}

pub(crate) fn decimal(s: &str, field: &str, back: &'static str) -> Result<Decimal, UiError> {
    s.trim().parse().map_err(|_| UiError::invalid(format!("{field}: expected a number"), back))
}

/// An optional dollar amount; a leading "$" is accepted.
pub(crate) fn opt_money(s: &str, field: &str, back: &'static str) -> Result<Option<Decimal>, UiError> {
    let s = s.trim().trim_start_matches('$');
    if s.is_empty() { Ok(None) } else { decimal(s, field, back).map(Some) }
}

/// A `datetime-local` value (`2026-09-29T08:00`, seconds optional).
pub(crate) fn local_time_at(s: &str, what: &str, back: &str) -> Result<time::PrimitiveDateTime, UiError> {
    use time::macros::format_description;
    let s = s.trim();
    time::PrimitiveDateTime::parse(s, format_description!("[year]-[month]-[day]T[hour]:[minute]"))
        .or_else(|_| {
            time::PrimitiveDateTime::parse(s, format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]"))
        })
        .map_err(|_| UiError::invalid_at(format!("{what}: expected a date and time"), back))
}

pub(crate) fn opt_uuid(s: &str, back: &'static str) -> Result<Option<Uuid>, UiError> {
    if s.trim().is_empty() {
        return Ok(None);
    }
    s.trim().parse().map(Some).map_err(|_| UiError::invalid("unknown selection", back))
}

pub(crate) fn see(to: &str) -> Response {
    Redirect::to(to).into_response()
}

// ── Layout ───────────────────────────────────────────────────────────────────

const CSS: &str = r#"
/* Colours come from the brand mark: leaf green, sun gold, soil brown on
   cream (brand/mark.svg). Headings use a serif, like the banner. */
:root{
  color-scheme:light dark;
  --primary:#2f6b2f;--on-primary:#fff;--primary-container:#dcefcf;--on-primary-container:#173e17;
  --secondary-container:#fbe8c3;--on-secondary-container:#5a3a0e;
  --tertiary:#a55d00;--tertiary-container:#ffe1b5;--on-tertiary-container:#573200;
  --ok:#2f6b2f;
  --error:#b3261e;--error-container:#fce1dc;--on-error-container:#7a1712;
  --surface:#fbf7ef;--surface-low:#f6f0e3;--surface-container:#f2eadb;--surface-high:#ebe2d0;--surface-highest:#e4d9c4;
  --card:#fffdf8;--field:#fffefb;--on-surface:#2b1b10;--on-surface-variant:#665544;--outline:#8f7c69;--outline-variant:#e6dccb;
  --inverse-surface:#3b2f2a;--inverse-on-surface:#fbf1e3;
  --glow:rgb(246 176 66/.2);
  --shadow:0 1px 2px rgb(43 27 16/.05),0 6px 16px -8px rgb(43 27 16/.12);
  --ring:0 0 0 4px color-mix(in srgb,var(--primary) 20%,transparent);
  --r-s:12px;--r-m:16px;--r-l:20px;--r-xl:28px;--r-full:999px;
  --ease:cubic-bezier(.2,0,0,1);
  --spring:cubic-bezier(.34,1.35,.64,1);
  --drawer:17rem;--bar:4.25rem;
  --sans:"Avenir Next","Segoe UI Variable Text","Segoe UI",system-ui,-apple-system,Roboto,sans-serif;
  --display:"Iowan Old Style","Palatino Linotype",Palatino,"Book Antiqua",Georgia,serif;
  --search-icon:url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='%238f7c69'%3E%3Cpath d='M15.5 14h-.79l-.28-.27A6.47 6.47 0 0 0 16 9.5 6.5 6.5 0 1 0 9.5 16c1.61 0 3.09-.59 4.23-1.57l.27.28v.79l5 4.99L20.49 19zm-6 0C7.01 14 5 11.99 5 9.5S7.01 5 9.5 5 14 7.01 14 9.5 11.99 14 9.5 14'/%3E%3C/svg%3E");
  --chevron:url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='%238f7c69'%3E%3Cpath d='M7 10l5 5 5-5z'/%3E%3C/svg%3E")
}
@supports (transition-timing-function:linear(0,1)){:root{
  /* A damped spring (ratio .62): overshoots 8%, then settles. */
  --spring:linear(0,.052,.178,.34,.509,.667,.802,.909,.987,1.039,1.069,1.082,1.082,1.075,1.062,1.048,1.034,1.022,1.012,1.004,.999,.995,.994,.993,.993,.994,.995,.997,1)
}}
@media (prefers-color-scheme:dark){:root{
  --primary:#a6d892;--on-primary:#0e3a0f;--primary-container:#264f28;--on-primary-container:#cbefba;
  --secondary-container:#4b3918;--on-secondary-container:#fbe8c3;
  --tertiary:#ffb957;--tertiary-container:#5b3c05;--on-tertiary-container:#ffe1b5;
  --ok:#a6d892;
  --error:#ffb4ab;--error-container:#8c1d18;--on-error-container:#ffdad6;
  --surface:#15120d;--surface-low:#1b1812;--surface-container:#211d16;--surface-high:#2a251d;--surface-highest:#343026;
  --card:#1e1a14;--field:#17140f;--on-surface:#f0e7d8;--on-surface-variant:#cdbfa9;--outline:#978a76;--outline-variant:#383126;
  --inverse-surface:#f0e7d8;--inverse-on-surface:#2b241b;
  --glow:rgb(246 176 66/.07);
  --shadow:none
}}
*{box-sizing:border-box}
html{-webkit-text-size-adjust:100%;scroll-padding-top:9rem}
body{margin:0;min-height:100vh;background:radial-gradient(70rem 26rem at 50% -10rem,var(--glow),transparent 70%) no-repeat,var(--surface);color:var(--on-surface);font:16px/1.55 var(--sans);overflow-wrap:break-word;-webkit-font-smoothing:antialiased;-webkit-tap-highlight-color:transparent;font-variant-numeric:tabular-nums}
a,button,input,select,textarea,summary{transition:background-color .2s var(--ease),color .2s var(--ease),border-color .2s var(--ease),box-shadow .2s var(--ease),border-radius .45s var(--spring),scale .45s var(--spring)}
:focus-visible{outline:3px solid var(--primary);outline-offset:2px}
a,a:visited{color:var(--primary);text-decoration-thickness:1px;text-decoration-color:color-mix(in srgb,currentColor 45%,transparent);text-underline-offset:.22em}
a:hover{text-decoration-color:currentColor}
::placeholder{color:var(--on-surface-variant);opacity:.6}
::selection{background:var(--primary-container);color:var(--on-primary-container)}
time{white-space:nowrap}

/* Top app bar. Phones: mark, search, sign out; section chips below. */
.appbar{position:sticky;top:0;z-index:3;display:grid;grid-template-columns:auto 1fr auto;grid-template-areas:"brand search out" "nav nav nav";align-items:center;gap:.5rem .75rem;padding:.6rem 1rem .5rem;background:color-mix(in srgb,var(--surface) 86%,transparent);border-bottom:1px solid var(--outline-variant);backdrop-filter:saturate(1.4) blur(16px);-webkit-backdrop-filter:saturate(1.4) blur(16px)}
.brand,.brand:visited{grid-area:brand;display:flex;align-items:center;gap:.65rem;color:var(--on-surface);text-decoration:none;font:600 1.15rem/1.1 var(--display);letter-spacing:-.005em;white-space:nowrap;border-radius:var(--r-full);padding:.15rem .75rem .15rem .15rem;justify-self:start}
.brand img{width:2.5rem;height:2.5rem;border-radius:50%;box-shadow:0 0 0 2px var(--card),0 2px 8px rgb(43 27 16/.18);transition:scale .45s var(--spring),rotate .6s var(--spring)}
.brand:hover img{scale:1.08;rotate:-8deg}
.appbar form.search{grid-area:search;margin:0;min-width:0}
.appbar form.search input[type=search]{width:100%;max-width:34rem;min-height:2.75rem;border:1px solid var(--outline-variant);border-radius:var(--r-full);background:var(--field) var(--search-icon) no-repeat .9rem center/1.3rem;padding:.5rem 1.25rem .5rem 2.75rem}
.appbar form.signout{grid-area:out}
.appbar form.signout button{background:transparent;color:var(--on-surface-variant);box-shadow:inset 0 0 0 1px var(--outline-variant)}
.appbar form.signout button:hover{background:var(--surface-high);color:var(--on-surface)}
.appbar nav{grid-area:nav;display:flex;gap:.4rem;overflow-x:auto;margin:0 -1rem;padding:.2rem 1rem .3rem;scrollbar-width:none;-webkit-mask-image:linear-gradient(90deg,transparent,#000 1rem,#000 calc(100% - 1.5rem),transparent);mask-image:linear-gradient(90deg,transparent,#000 1rem,#000 calc(100% - 1.5rem),transparent)}
.appbar nav::-webkit-scrollbar{display:none}
.appbar nav .navgroup{display:none}
.appbar nav a,.appbar nav a:visited{flex:none;display:flex;align-items:center;min-height:2.4rem;padding:0 .95rem;border-radius:var(--r-full);border:1px solid var(--outline-variant);background:var(--card);color:var(--on-surface-variant);font-size:.9rem;font-weight:600;text-decoration:none;white-space:nowrap}
.appbar nav a:hover{background:var(--surface-high);color:var(--on-surface)}
.appbar nav a:active{scale:.94}
.appbar nav a[aria-current=page]{background:var(--primary);color:var(--on-primary);border-color:transparent}
@media (max-width:560px){.with-nav .brand span{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0,0,0,0)}.with-nav .brand{padding:0}}

/* Wide screens: a navigation drawer on the left, grouped by area. */
@media (min-width:1100px){
  body.with-nav{padding-left:var(--drawer)}
  /* No backdrop-filter here: it would make the bar the containing block of
     the fixed drawer inside it, pinning the drawer to the bar's height. */
  .with-nav .appbar{margin-left:calc(-1*var(--drawer));height:var(--bar);background:var(--surface);backdrop-filter:none;-webkit-backdrop-filter:none;grid-template-columns:calc(var(--drawer) - 1rem) 1fr auto;grid-template-areas:"brand search out";padding:0 1.5rem 0 1rem;gap:1rem}
  .appbar nav{position:fixed;top:var(--bar);left:0;bottom:0;width:var(--drawer);flex-direction:column;gap:1px;margin:0;padding:.5rem .75rem 2rem;overflow-y:auto;background:var(--surface-low);border-right:1px solid var(--outline-variant);-webkit-mask-image:none;mask-image:none;scrollbar-width:thin}
  .appbar nav .navgroup{display:block;padding:1rem .9rem .3rem;font-size:.7rem;font-weight:700;letter-spacing:.09em;text-transform:uppercase;color:var(--on-surface-variant);opacity:.8}
  .appbar nav a,.appbar nav a:visited{min-height:2.35rem;padding:0 .9rem;border:0;background:none;border-radius:var(--r-s);font-size:.925rem;font-weight:550}
  .appbar nav a[aria-current=page]{background:var(--primary-container);color:var(--on-primary-container);font-weight:700}
  .appbar nav a:active{scale:.97}
  html{scroll-padding-top:6rem}
}

main{max-width:1100px;margin:0 auto;padding:2rem 1.5rem 5rem}
h1,h2{font-family:var(--display);font-weight:600;text-wrap:balance}
h1{font-size:clamp(2.2rem,1.6rem + 2vw,3.2rem);letter-spacing:-.02em;line-height:1.05;margin:.25rem 0 1.25rem}
h2{font-size:1.55rem;letter-spacing:-.01em;line-height:1.2;margin:2.75rem 0 1rem}
h3{font-size:1.05rem;font-weight:700;letter-spacing:-.005em;margin:1.5rem 0 .6rem}
h1:has(+p.lede){margin-bottom:.35rem}
p.lede{font-size:1.1rem;margin:0 0 1.75rem}
p{margin:.4rem 0 .9rem}
main>ul:not([class]){padding-left:1.25rem}
main>ul:not([class]) li{margin:.3rem 0}
main>ul:not([class]) li::marker{color:var(--outline)}
img{max-width:100%;border-radius:var(--r-s)}
.legal{max-width:44rem}.legal ul{padding-left:1.25rem}.legal li{margin:.3rem 0}.legal h2{margin-top:1.75rem}.legal dt{font-weight:600;margin-top:1rem}.legal dd{margin:.25rem 0 0}
footer.site{display:flex;gap:1rem;justify-content:center;padding:2rem 1rem 2.5rem;font-size:.85rem}
.muted,a.muted,a.muted:visited{color:var(--on-surface-variant)}
.clamp{display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:2;line-clamp:2;overflow:hidden;white-space:pre-line}
.warn{color:var(--error)}.ok{color:var(--ok)}
p.warn[role=alert]{background:var(--error-container);color:var(--on-error-container);padding:1rem 1.25rem;border-radius:var(--r-m);font-weight:550}

/* Surfaces */
.grid{display:grid;gap:1rem;grid-template-columns:repeat(auto-fit,minmax(min(240px,100%),1fr))}
.card{background:var(--card);border:1px solid var(--outline-variant);border-radius:var(--r-l);padding:1.35rem 1.5rem;min-width:0;box-shadow:var(--shadow)}
:not(.grid)>.card+.card{margin-top:1rem}
.card>h3:first-child,.card>strong:first-child{margin-top:0}
.stat{font:600 clamp(2.4rem,2rem + 1.5vw,3.2rem)/1 var(--display);letter-spacing:-.02em;margin:.1rem 0 .5rem}
.grid>.card:has(>.stat){border-color:transparent;box-shadow:none;background:var(--primary-container);color:var(--on-primary-container)}
.grid>.card:has(>.stat):nth-child(3n+2){background:var(--tertiary-container);color:var(--on-tertiary-container)}
.grid>.card:has(>.stat):nth-child(3n+3){background:var(--surface-highest);color:var(--on-surface)}
.grid>.card:has(>.stat) .muted,.grid>.card:has(>.stat) a{color:inherit}
.grid>.card:has(>.stat) .muted{opacity:.85;font-weight:600}
.grid>.card:has(>.stat.warn){background:var(--error-container);color:var(--on-error-container)}
.grid>.card:has(>.stat.warn) .stat{color:inherit}

table{width:100%;border-collapse:separate;border-spacing:0;background:var(--card);border:1px solid var(--outline-variant);border-radius:var(--r-l);overflow:hidden;box-shadow:var(--shadow)}
td,th{text-align:left;padding:.8rem 1rem;border-bottom:1px solid var(--outline-variant);vertical-align:top}
th{background:var(--surface-low);font-size:.72rem;font-weight:700;letter-spacing:.07em;text-transform:uppercase;color:var(--on-surface-variant)}
tr:last-child>td{border-bottom:0}
tr:hover>td{background:color-mix(in srgb,var(--primary) 4%,transparent)}

/* Forms. Inputs sit inside their label, so they reset its small type. */
form.card{display:grid;gap:1.1rem;align-content:start}
form.card>button{justify-self:start;min-width:10rem}
form.narrow{max-width:30rem}
label{display:grid;gap:.4rem;font-size:.85rem;font-weight:650;letter-spacing:.01em;color:var(--on-surface-variant)}
label.inline{display:flex;gap:.65rem;align-items:center;min-height:2.75rem;font-size:.95rem;font-weight:450;letter-spacing:0;color:var(--on-surface);cursor:pointer}
input,select,textarea{font:inherit;font-size:1rem;font-weight:400;letter-spacing:normal;min-height:3rem;padding:.65rem .95rem;border:1px solid color-mix(in srgb,var(--outline) 45%,transparent);border-radius:var(--r-s);background:var(--field);color:var(--on-surface);width:100%}
input:hover,select:hover,textarea:hover{border-color:var(--outline)}
input:focus,select:focus,textarea:focus{outline:none;border-color:var(--primary);box-shadow:var(--ring)}
input:user-invalid,select:user-invalid,textarea:user-invalid{border-color:var(--error)}
select{appearance:none;-webkit-appearance:none;background-image:var(--chevron);background-repeat:no-repeat;background-position:right .7rem center;background-size:1.4rem;padding-right:2.6rem}
textarea{min-height:5.5rem;resize:vertical;field-sizing:content}
input:-webkit-autofill,input:-webkit-autofill:hover,input:-webkit-autofill:focus,textarea:-webkit-autofill,select:-webkit-autofill{-webkit-text-fill-color:var(--on-surface);caret-color:var(--on-surface);box-shadow:0 0 0 1000px var(--surface-low) inset}
input[type=checkbox],input[type=radio]{width:1.25rem;height:1.25rem;min-height:0;padding:0;box-shadow:none;accent-color:var(--primary);flex:none;cursor:pointer}
input[type=file]{padding:.5rem}
input[type=file]::file-selector-button{font:inherit;font-weight:600;border:0;border-radius:var(--r-full);padding:.45rem 1rem;margin-right:.75rem;background:var(--primary-container);color:var(--on-primary-container);cursor:pointer}
fieldset.chips{border:0;margin:0;padding:0;display:flex;flex-wrap:wrap;gap:.5rem}
fieldset.chips legend{padding:0 0 .5rem;font-size:.85rem;font-weight:650;color:var(--on-surface-variant)}
fieldset.chips label.inline{min-height:2.5rem;padding:0 1rem 0 .75rem;border:1px solid var(--outline-variant);border-radius:var(--r-full);background:var(--field);font-weight:550;font-size:.9rem}
fieldset.chips label.inline:has(:checked){background:var(--primary-container);color:var(--on-primary-container);border-color:transparent}

/* Buttons: filled, and tonal for small ones. They squash toward a rounded
   square when pressed and spring back. */
a.button{display:inline-flex;align-items:center;justify-content:center;text-decoration:none}
button,a.button{font:inherit;font-weight:650;min-height:3rem;padding:0 1.5rem;border:0;border-radius:var(--r-full);background:var(--primary);color:var(--on-primary);cursor:pointer;letter-spacing:.005em}
button:hover,a.button:hover{background:color-mix(in srgb,var(--on-primary) 12%,var(--primary));box-shadow:var(--shadow)}
button:active{border-radius:var(--r-s);scale:.97}
button:disabled{background:color-mix(in srgb,var(--on-surface) 12%,transparent);color:color-mix(in srgb,var(--on-surface) 45%,transparent);cursor:not-allowed;box-shadow:none;scale:1}
button.small,ul.work button{min-height:2.5rem;padding:0 1.1rem;font-size:.875rem;background:var(--primary-container);color:var(--on-primary-container)}
button.small:hover,ul.work button:hover{background:color-mix(in srgb,var(--on-primary-container) 10%,var(--primary-container));box-shadow:none}
form.inline{display:inline-flex;gap:.5rem;align-items:center;flex-wrap:wrap}
.row{display:flex;flex-wrap:wrap;gap:.5rem;align-items:center;margin-top:.9rem}
form.roles{display:flex;flex-wrap:wrap;gap:.75rem;align-items:end;margin-top:1rem}
form.inline input:not([type=checkbox],[type=radio]),form.inline select,form.inline textarea{width:auto;min-width:8rem;min-height:2.5rem;padding:.35rem .75rem}
form.inline select{padding-right:2.4rem}
.task-filters{display:flex;flex-wrap:wrap;gap:.5rem;margin:0 0 1rem}
.task-filters a{display:inline-flex;align-items:center;min-height:2.5rem;padding:0 1rem;border:1px solid var(--outline-variant);border-radius:var(--r-s);font-weight:550;font-size:.9rem;text-decoration:none;color:inherit}
.task-filters a[aria-current=page]{background:var(--secondary-container);color:var(--on-secondary-container);border-color:transparent;border-radius:var(--r-full)}

.pill{display:inline-flex;align-items:center;min-height:1.5rem;padding:.05rem .65rem;border-radius:var(--r-full);background:var(--secondary-container);color:var(--on-secondary-container);font-size:.75rem;font-weight:700;letter-spacing:.02em;white-space:nowrap;text-decoration:none;vertical-align:.1em}
a.pill:hover{box-shadow:inset 0 0 0 1px currentColor}
.pill.urgent,.pill.high{background:var(--error-container);color:var(--on-error-container)}
.pill.low{background:var(--surface-highest);color:var(--on-surface-variant)}

details.card{padding:0 1.5rem}
details.card[open]{padding-bottom:1.25rem}
summary{cursor:pointer;padding:1.1rem 0;font-weight:650;list-style:none;display:flex;justify-content:space-between;gap:1rem;align-items:center;border-radius:var(--r-s)}
summary::-webkit-details-marker{display:none}
summary::after{content:"";flex:none;width:1.5rem;height:1.5rem;background:var(--chevron) center/1.5rem no-repeat;transition:rotate .45s var(--spring)}
details[open]>summary::after{rotate:180deg}
/* A task's "Change" control sits in its Who cell, not as a card.
   The name, the take button and the picker stack: side by side they
   collide once the title column takes the width. */
details.reassign>summary{padding:.25rem 0;font-weight:500;font-size:.875rem;color:var(--on-surface-variant);justify-content:flex-start;white-space:nowrap;overflow-wrap:normal}
.who{display:flex;flex-direction:column;align-items:stretch;gap:.4rem}
.who>form{margin:0}
.who>form button{align-self:flex-start}
details.reassign{width:100%}
form.pick{display:flex;gap:.4rem;align-items:center;margin:.15rem 0 0}
form.pick select{flex:1;width:auto;min-width:0;min-height:2.5rem}
form.pick button{flex:none}
/* Column widths have to live on <col>: a width on the cells is ignored. */
table.tasks{table-layout:fixed}
table.tasks col.pri{width:6.5rem}
table.tasks col.due{width:11rem}
table.tasks col.who{width:13rem}
table.tasks col.done{width:7.5rem}
.task-description{margin-top:.35rem}
.task-description>summary{display:inline-flex;justify-content:flex-start;gap:.25rem;padding:0;color:var(--on-surface-variant);font-size:.875rem;font-weight:600;white-space:nowrap;overflow-wrap:normal}
.task-description>summary::after{width:1.15rem;height:1.15rem;background-size:1.15rem}
.task-description>p{margin:.4rem 0 0;color:var(--on-surface-variant);white-space:pre-line}
.task-description .when-open{display:none}
.task-description[open] .when-closed{display:none}
.task-description[open] .when-open{display:inline}
code,pre{font-family:ui-monospace,"SF Mono","Roboto Mono",Menlo,monospace}
code{font-size:.85em;overflow-wrap:anywhere;background:var(--surface-high);padding:.1em .35em;border-radius:6px}
pre code{background:none;padding:0}
pre,summary{white-space:pre-wrap;overflow-wrap:anywhere;max-width:100%}
pre{background:var(--surface-container);border-radius:var(--r-m);padding:1rem 1.15rem;font-size:.85rem}
.letter p{margin:.8rem 0}
.skip{position:absolute;left:.75rem;top:.75rem;transform:translateY(-200%);background:var(--primary);color:var(--on-primary);padding:.6rem 1.1rem;border-radius:var(--r-full);z-index:5;text-decoration:none;font-weight:650}
.skip:focus{transform:none}
.banner{max-width:1100px;margin:1.25rem auto 0;padding:0 1.5rem}
.banner a,.banner a:visited{display:flex;align-items:center;gap:.75rem;padding:.9rem 1.25rem;border-radius:var(--r-l);background:var(--primary-container);color:var(--on-primary-container);font-weight:650;text-decoration:none}
.banner a::before{content:"🌱";font-size:1.2rem}
.banner a::after{content:"→";margin-left:auto;transition:translate .45s var(--spring)}
.banner a:hover::after{translate:.35rem 0}

/* "Since you last looked": one card per notice, its tasks as a list. */
ul.notices{list-style:none;padding:0;margin:0;display:grid;gap:.75rem}
ul.notices>li{background:var(--card);border:1px solid var(--outline-variant);border-radius:var(--r-l);padding:.95rem 1.15rem;box-shadow:var(--shadow)}
ul.notices p{margin:0 0 .35rem}
ul.notices>li>p:first-child{font-weight:650}
ul.notices ul{margin:.15rem 0 .45rem;padding-left:1.15rem}
ul.notices ul:last-child{margin-bottom:0}
ul.notices li li{margin:.28rem 0}
ul.notices a{overflow-wrap:anywhere}

/* A list of things to do: one row each, the action at the end. */
ul.work{list-style:none;padding:0;margin:0;display:grid;background:var(--card);border:1px solid var(--outline-variant);border-radius:var(--r-l);box-shadow:var(--shadow);overflow:hidden}
ul.work li{padding:.7rem .85rem .7rem 1.25rem;min-height:3.75rem;display:flex;gap:.75rem;align-items:center;justify-content:space-between;border-bottom:1px solid var(--outline-variant)}
ul.work li:last-child{border-bottom:0}
ul.work li>span:first-child{flex:1;min-width:0}
ul.work li>a{flex:none;font-weight:600;font-size:.9rem}
ul.work form{flex:none;margin:0}
.actions{display:grid;gap:1rem;grid-template-columns:repeat(auto-fit,minmax(min(16rem,100%),1fr))}
.actions form.card{border-radius:var(--r-xl)}
.actions form.card>button{justify-self:stretch}
.actions button{min-height:3.25rem;font-size:1rem}
#field-queue:empty{display:none}
#field-queue{position:fixed;left:50%;bottom:1rem;translate:-50% 0;z-index:4;margin:0;max-width:calc(100% - 2rem);padding:.9rem 1.25rem;border-radius:var(--r-s);background:var(--inverse-surface);color:var(--inverse-on-surface);box-shadow:0 4px 16px rgb(0 0 0/.2)}
.visually-hidden{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0}

/* Signed out (sign-in, join, adopt, QR tags): no drawer, a narrower page. */
body:not(.with-nav) .appbar{background:transparent;border-bottom-color:transparent;backdrop-filter:none;-webkit-backdrop-filter:none;position:static;padding-top:1.25rem;justify-content:center;grid-template-columns:auto}
body:not(.with-nav) main{max-width:44rem}
body:not(.with-nav) .brand{justify-self:center}

@media (max-width:900px){
  main{padding:1.25rem 1rem 5rem}
  h2{margin-top:2.25rem}
  .card{padding:1.15rem 1.2rem}
  table{display:block;overflow-x:auto;-webkit-overflow-scrolling:touch}
  ul.work li{padding-left:1rem}
  form.card>button{justify-self:stretch}
  form.inline{display:flex;width:100%}
  form.inline input:not([type=checkbox],[type=radio]),form.inline select,form.inline textarea{width:100%;min-width:0}
  .appbar form.signout button{padding:0 .9rem}
  /* The task table becomes one card per task: title first, then the rest. */
  table.tasks{display:block;overflow:visible}
  table.tasks tbody{display:block}
  table.tasks tr:first-child:has(th){display:none}
  table.tasks tr{display:grid;grid-template-columns:auto 1fr auto;grid-template-areas:"task task task" "pri due done" "who who who";align-items:center;column-gap:.6rem;row-gap:.35rem;padding:.8rem .85rem .8rem 1rem;border-bottom:1px solid var(--outline-variant)}
  table.tasks tr:last-child{border-bottom:0}
  table.tasks td{display:block;padding:0;border:0;background:none;width:auto}
  table.tasks td:nth-child(1){grid-area:pri}
  table.tasks td:nth-child(2){grid-area:task;margin-bottom:.15rem}
  table.tasks td:nth-child(3){grid-area:due;font-size:.875rem}
  table.tasks td:nth-child(4){grid-area:who}
  table.tasks td:nth-child(5){grid-area:done;justify-self:end}
}
@media (max-width:560px){
  table.tasks tr{grid-template-columns:auto 1fr;grid-template-areas:"task task" "pri due" "who who" "done done"}
  table.tasks td:nth-child(5){justify-self:stretch}
  table.tasks td:nth-child(5) form,.tasks td:nth-child(5) button{width:100%}
}
@media (prefers-reduced-motion:reduce){*,*::before,*::after{scroll-behavior:auto!important;transition:none!important}}
@media print{.appbar,form,.banner,.noprint,main>h1{display:none}body.with-nav{padding-left:0}.card,table,ul.work{border:0;box-shadow:none;padding:0;background:none}body{background:#fff;color:#000}}
.plan{display:block;width:100%;max-height:70vh;background:var(--card);border:1px solid var(--outline-variant);border-radius:var(--r-l);box-shadow:var(--shadow);margin:.5rem 0;touch-action:manipulation}
.plan .grid line{stroke:var(--outline-variant);stroke-width:.5;vector-effect:non-scaling-stroke}
.plan .shape>*{--c:var(--outline);fill:var(--c);fill-opacity:.18;stroke:var(--c);stroke-width:1.5;vector-effect:non-scaling-stroke;stroke-linejoin:round}
.plan .shape>polyline{fill:none;stroke-width:3}.plan .shape>circle{fill-opacity:.9}
.plan .grow>*{--c:var(--ok)}.plan .animal>*{--c:var(--tertiary)}.plan .compost>*{--c:var(--on-secondary-container)}
.plan .shape:hover>*,.plan .shape:focus-visible>*{fill-opacity:.4;stroke-width:2.5}
.plan .here>*{fill-opacity:.45;stroke-width:3}.plan .dim>*{fill-opacity:.07;stroke-opacity:.5}
.plan text{fill:var(--on-surface);pointer-events:none;paint-order:stroke;stroke:var(--card);stroke-width:3px;stroke-linejoin:round;vector-effect:non-scaling-stroke}
.plan text.scale{fill:var(--on-surface-variant);text-anchor:start}
.spark{display:block}.spark polyline{fill:none;stroke:var(--ok);stroke-width:1.5;vector-effect:non-scaling-stroke;stroke-linejoin:round}
"#;

/// Navigation, grouped by area; each link shown only to people who can
/// open it, and a group's heading only when one of its links is.
/// A navigation link: path, label, and the capability that opens it.
type NavLink = (&'static str, &'static str, Capability);

const NAV: &[(&str, &[NavLink])] = &[
    (
        "Growing",
        &[
            ("/tasks", "Tasks", Capability::TasksRead),
            ("/garden", "Garden", Capability::GardenRead),
            ("/map", "Map", Capability::GardenRead),
            ("/crops", "Crops", Capability::GardenRead),
            ("/health", "Crop health", Capability::GardenRead),
            ("/farm", "Farm", Capability::GardenRead),
            ("/water", "Water", Capability::GardenRead),
            ("/readings", "Readings", Capability::GardenRead),
        ],
    ),
    (
        "Supplies",
        &[
            ("/inventory", "Inventory", Capability::InventoryRead),
            ("/purchases", "Purchases", Capability::PurchasesRead),
            ("/equipment", "Equipment", Capability::EquipmentRead),
            ("/tags", "QR tags", Capability::EquipmentWrite),
        ],
    ),
    (
        "Animals",
        &[("/animals", "Animals", Capability::AnimalsRead), ("/applications", "Adoptions", Capability::AnimalsWrite)],
    ),
    (
        "People",
        &[
            ("/community", "Community", Capability::CommunityRead),
            ("/stand", "Farm stand", Capability::StandRead),
            ("/gifts", "Gifts", Capability::GiftsRead),
            ("/contacts", "Mailing list", Capability::ContactsRead),
            ("/grants", "Grants", Capability::GrantsRead),
            ("/releases", "Releases", Capability::ReleasesRead),
        ],
    ),
    (
        "Running the place",
        &[
            ("/incidents", "Incidents", Capability::IncidentsReport),
            ("/report", "Report", Capability::ReportsRead),
            ("/audit", "Audit", Capability::AuditRead),
            ("/ops", "Integrations", Capability::SystemsRead),
            ("/carry", "Carry", Capability::SystemsAdmin),
            ("/people", "People", Capability::SystemsAdmin),
        ],
    ),
];

pub fn layout(title: &str, body: Markup) -> Markup {
    let viewer = crate::auth::VIEWER.try_with(Clone::clone).ok();
    let path = crate::auth::PATH.try_with(Clone::clone).unwrap_or_default();
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="color-scheme" content="light dark";
                meta name="theme-color" media="(prefers-color-scheme: light)" content="#fbf7ef";
                meta name="theme-color" media="(prefers-color-scheme: dark)" content="#15120d";
                link rel="manifest" href="/manifest.webmanifest";
                link rel="icon" href="/icon.svg" type="image/svg+xml";
                title { (title) " · Sun Brewed Homestead" }
                style { (maud::PreEscaped(CSS)) }
            }
            body class=[viewer.is_some().then_some("with-nav")] {
                a.skip href="#main" { "Skip to content" }
                header.appbar {
                    a.brand href="/" {
                        img src="/icon.svg" alt="" width="40" height="40";
                        span { "Sun Brewed Homestead" }
                    }
                    @if let Some(who) = &viewer {
                        @if !homestead_domain::search::SearchKind::visible(who.caps).is_empty() {
                            form.search method="get" action="/search" role="search" {
                                input type="search" name="q" placeholder="Search" aria-label="Search";
                            }
                        }
                        form.signout method="post" action="/logout" { button type="submit" class="small" { "Sign out" } }
                        nav aria-label="Sections" {
                            a href="/" aria-current=[(path == "/").then_some("page")] { "Today" }
                            @for (group, links) in NAV {
                                @if links.iter().any(|l| who.can(l.2)) { span.navgroup aria-hidden="true" { (group) } }
                                @for (href, label, cap) in *links {
                                    @if who.can(*cap) { a href=(href) aria-current=[(path == *href).then_some("page")] { (label) } }
                                }
                            }
                            a href="/me" aria-current=[(path == "/me").then_some("page")] { (who.actor.display_name) }
                        }
                        // The current section can sit off the end of the chip row.
                        (maud::PreEscaped(r#"<script>document.querySelector('.appbar nav [aria-current]')?.scrollIntoView({block:"nearest",inline:"center"})</script>"#))
                    }
                }
                @if viewer.as_ref().is_some_and(|w| w.actor.welcomed_at.is_none() && w.actor.kind == homestead_domain::auth::ActorKind::Human) && path != "/welcome" {
                    p.banner { a href="/welcome" { "New here? Finish the welcome page." } }
                }
                main id="main" tabindex="-1" { h1 { (title) } (body) }
                footer.site { a.muted href="/terms" { "Terms of Service" } a.muted href="/privacy" { "Privacy Policy" } }
                @if viewer.is_some() { p id="field-queue" class="muted" role="status" {} }
                (maud::PreEscaped(crate::pwa::REGISTER))
                @if viewer.is_some() { (maud::PreEscaped(crate::pwa::FIELD_QUEUE)) }
            }
        }
    }
}

pub fn error_page(message: &str, back: &str) -> Markup {
    layout("Something's off", html! { p.warn role="alert" { (message) } p { a href=(back) { "← Go back" } } })
}

pub(crate) fn options<T: Copy + PartialEq + std::fmt::Display>(all: &[T], selected: Option<T>) -> Markup {
    html! { @for v in all { option value=(v) selected[Some(*v) == selected] { (v) } } }
}

/// Markdown (Linear's descriptions) as the plain text a person would read
/// aloud: `[text](<url>)` keeps its text, emphasis and escapes go, and a bare
/// link becomes its address.
pub(crate) fn plain(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut rest = md;
    while let Some(open) = rest.find('[') {
        let (before, tail) = rest.split_at(open);
        if let Some(escaped) = before.strip_suffix('\\') {
            out.push_str(escaped);
            out.push('[');
            rest = &tail[1..];
            continue;
        }
        out.push_str(before);
        let link = tail.find("](").and_then(|close| {
            let end = tail[close..].find(')')? + close;
            Some((&tail[1..close], end))
        });
        match link {
            Some((text, end)) if !text.contains('\n') => {
                let text = text.trim_start_matches('<').trim_end_matches('>');
                out.push_str(text.strip_prefix("mailto:").unwrap_or(text));
                rest = &tail[end + 1..];
            }
            _ => {
                out.push('[');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "").replace("__", "").replace('`', "").replace("\\", "")
}

/// The same chat markdown, as HTML. Deadline reminders and the morning
/// digest are written for Discord (`**bold**`, `` `code` ``, `[text](<url>)`,
/// one `•` per line). On a page that string has to become a list and real
/// links; anything that isn't http(s) or mailto stays as text.
pub(crate) fn chat(md: &str) -> Markup {
    let mut out = String::new();
    let mut paras: Vec<&str> = Vec::new();
    let mut items: Vec<&str> = Vec::new();
    let flush_paras = |out: &mut String, paras: &mut Vec<&str>| {
        for line in paras.drain(..) {
            out.push_str("<p>");
            push_inline(out, line);
            out.push_str("</p>");
        }
    };
    let flush_items = |out: &mut String, items: &mut Vec<&str>| {
        if items.is_empty() {
            return;
        }
        out.push_str("<ul>");
        for line in items.drain(..) {
            out.push_str("<li>");
            push_inline(out, line);
            out.push_str("</li>");
        }
        out.push_str("</ul>");
    };
    for line in md.split('\n') {
        let line = line.trim().trim_end_matches('\r');
        if line.is_empty() {
            flush_paras(&mut out, &mut paras);
            flush_items(&mut out, &mut items);
            continue;
        }
        if let Some(item) = line.strip_prefix('•').or_else(|| line.strip_prefix("- ")) {
            flush_paras(&mut out, &mut paras);
            items.push(item.trim());
        } else {
            flush_items(&mut out, &mut items);
            paras.push(line);
        }
    }
    flush_paras(&mut out, &mut paras);
    flush_items(&mut out, &mut items);
    maud::PreEscaped(out)
}

/// `[text](<url>)` or `[text](url)`, as Linear and Discord write a link.
/// The returned length covers the whole marker.
fn parse_link(tail: &str) -> Option<(&str, &str, usize)> {
    let close = tail.find("](")?;
    if close < 2 {
        return None;
    }
    let text = &tail[1..close];
    if text.contains('\n') {
        return None;
    }
    let after = &tail[close + 2..];
    if let Some(inner) = after.strip_prefix('<') {
        let gt = inner.find('>')?;
        if !inner[gt..].starts_with(">)") {
            return None;
        }
        Some((text, &inner[..gt], close + gt + 5))
    } else {
        let end = after.find(')')?;
        if after[..end].contains('\n') {
            return None;
        }
        Some((text, &after[..end], close + end + 3))
    }
}

fn safe_href(url: &str) -> Option<&str> {
    let url = url.trim();
    if url.is_empty()
        || url.chars().any(|c| c.is_ascii_control() || c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '\\'))
    {
        return None;
    }
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:") {
        Some(url)
    } else {
        None
    }
}

fn push_text(out: &mut String, s: &str) {
    use std::fmt::Write;
    write!(maud::Escaper::new(out), "{s}").ok();
}

fn push_inline(out: &mut String, md: &str) {
    let mut rest = md;
    while !rest.is_empty() {
        if let Some(n) = push_bold(out, rest) {
            rest = &rest[n..];
            continue;
        }
        if let Some(n) = push_code(out, rest) {
            rest = &rest[n..];
            continue;
        }
        if let Some(n) = push_link(out, rest) {
            rest = &rest[n..];
            continue;
        }
        let next = rest.find(['*', '`', '[', '\\']).unwrap_or(rest.len());
        if next == 0 {
            let ch = rest.chars().next().expect("the loop condition guarantees non-empty input");
            if ch == '\\' {
                let mut chars = rest.chars();
                chars.next();
                if let Some(escaped) = chars.next() {
                    push_text(out, &escaped.to_string());
                    rest = &rest[ch.len_utf8() + escaped.len_utf8()..];
                    continue;
                }
            }
            push_text(out, &ch.to_string());
            rest = &rest[ch.len_utf8()..];
        } else {
            push_text(out, &rest[..next]);
            rest = &rest[next..];
        }
    }
}

fn push_bold(out: &mut String, rest: &str) -> Option<usize> {
    let inner = rest.strip_prefix("**")?;
    let end = inner.find("**")?;
    if end == 0 || inner[..end].contains('\n') {
        return None;
    }
    out.push_str("<strong>");
    push_inline(out, &inner[..end]);
    out.push_str("</strong>");
    Some(end + 4)
}

fn push_code(out: &mut String, rest: &str) -> Option<usize> {
    let inner = rest.strip_prefix('`')?;
    let end = inner.find('`')?;
    if end == 0 || inner[..end].contains('\n') {
        return None;
    }
    out.push_str("<code>");
    push_text(out, &inner[..end]);
    out.push_str("</code>");
    Some(end + 2)
}

fn push_link(out: &mut String, rest: &str) -> Option<usize> {
    if !rest.starts_with('[') {
        return None;
    }
    let (text, url, n) = parse_link(rest)?;
    let text = text.trim_start_matches('<').trim_end_matches('>');
    let text = text.strip_prefix("mailto:").unwrap_or(text);
    if let Some(href) = safe_href(url) {
        out.push_str("<a href=\"");
        push_text(out, href);
        out.push_str("\">");
        push_inline(out, text);
        out.push_str("</a>");
    } else {
        push_inline(out, text);
    }
    Some(n)
}

/// A date the way a person reads it ("Sat, Oct 3, 2026"), with the ISO date
/// kept in `datetime` for machines. The year is always shown: leaving it out
/// "when it's this year" would need the farm's today, which a renderer
/// can't ask the store for.
pub(crate) struct Day(pub Date);

impl maud::Render for Day {
    fn render(&self) -> Markup {
        let d = self.0;
        let label =
            d.format(format_description!("[weekday repr:short], [month repr:short] [day padding:none], [year]"));
        html! { time datetime=(d) { (label.unwrap_or_else(|_| d.to_string())) } }
    }
}

/// How long ago, the way a person would say it.
pub(crate) fn ago(d: time::Duration) -> String {
    match d.whole_minutes() {
        m if m < 1 => "just now".into(),
        1 => "a minute ago".into(),
        m if m < 60 => format!("{m} minutes ago"),
        m if m < 120 => "an hour ago".into(),
        m if m < 48 * 60 => format!("{} hours ago", m / 60),
        m => format!("{} days ago", m / (24 * 60)),
    }
}

fn priority_pill(p: TaskPriority) -> Markup {
    html! { span class={"pill " (p)} { (p) } }
}

/// Who has a task, as the viewer would say it.
pub(crate) fn who_has(t: &homestead_domain::Task, names: &std::collections::HashMap<Uuid, String>) -> Option<String> {
    let me = crate::auth::VIEWER.try_with(|v| v.actor.id).ok();
    let id = t.assigned_to?;
    Some(if Some(id) == me { "you".to_owned() } else { names.get(&id).cloned().unwrap_or_else(|| "someone".into()) })
}

fn task_table(
    tasks: &[homestead_domain::Task],
    links: &std::collections::HashMap<uuid::Uuid, homestead_store::linear::IssueLink>,
    people: &[(Uuid, String)],
    names: &std::collections::HashMap<Uuid, String>,
) -> Markup {
    let can_write = crate::auth::VIEWER.try_with(|v| v.can(Capability::TasksWrite)).unwrap_or(false);
    let me = crate::auth::VIEWER.try_with(|v| v.actor.id).ok();
    html! {
        @if tasks.is_empty() { p.muted { "Nothing open. Enjoy the sunshine." } } @else {
            table.tasks {
                caption.visually-hidden { "Open tasks" }
                colgroup { col.pri; col; col.due; col.who; col.done; }
                tr { th scope="col" { "Priority" } th scope="col" { "Task" } th scope="col" { "Due" } th scope="col" { "Who" } th scope="col" { "Done" } }
                @for t in tasks {
                    tr {
                        td { (priority_pill(t.priority)) }
                        td {
                            @if t.template_id.is_some() { "🔁 " } (t.title) (crate::linear_pull::chip(links.get(&t.id)))
                            @if !t.details.is_empty() {
                                details.task-description {
                                    summary {
                                        span.when-closed { "Show details" }
                                        span.when-open { "Hide details" }
                                    }
                                    p { (plain(&t.details)) }
                                }
                            }
                        }
                        td.muted { @if let Some(d) = t.due_on { (crate::ui::Day(d)) } }
                        td {
                            div.who {
                                @match who_has(t, names) { Some(w) => { (w) }, None => { span.muted { "anyone" } } }
                                @if can_write {
                                    @if t.assigned_to.is_none() {
                                        @if let Some(me) = me {
                                            form method="post" action={"/tasks/" (t.id) "/assign"} {
                                                input type="hidden" name="assigned_to" value=(me);
                                                button.small type="submit" aria-label={"Take: " (t.title)} { "Take it" }
                                            }
                                        }
                                    } @else if t.assigned_to == me {
                                        form method="post" action={"/tasks/" (t.id) "/assign"} {
                                            input type="hidden" name="assigned_to" value="";
                                            button.small.secondary type="submit" aria-label={"Let go of: " (t.title)} { "Let go" }
                                        }
                                    }
                                    details.reassign {
                                        summary { "Change" }
                                        form.pick method="post" action={"/tasks/" (t.id) "/assign"} {
                                            select name="assigned_to" aria-label={"Who does " (t.title)} { (people_options(people, t.assigned_to)) }
                                            button.small type="submit" { "Set" }
                                        }
                                    }
                                }
                            }
                        }
                        td { @if can_write { form.inline method="post" action={"/tasks/" (t.id) "/complete"} { button.small type="submit" aria-label={"Mark done: " (t.title)} { "Done" } } } }
                    }
                }
            }
        }
    }
}

/// `<option>`s for choosing who does a task, "anyone" first.
fn people_options(people: &[(Uuid, String)], selected: Option<Uuid>) -> Markup {
    html! {
        option value="" selected[selected.is_none()] { "Anyone" }
        @for (id, name) in people { option value=(id) selected[selected == Some(*id)] { (name) } }
    }
}

// ── Tasks ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TaskView {
    #[default]
    All,
    Mine,
    Unassigned,
}

#[derive(Default, Deserialize)]
struct TasksQuery {
    #[serde(default)]
    who: TaskView,
}

async fn tasks_page(c: Can<cap::TasksRead>, State(s): State<AppState>, Query(q): Query<TasksQuery>) -> UiResult {
    crate::run_schedules(&s).await;
    crate::linear_pull::refresh(&s).await;
    let mut tasks = c.store.list_open_tasks().await.back("/tasks")?;
    match q.who {
        TaskView::All => {}
        TaskView::Mine => tasks.retain(|t| t.assigned_to == Some(c.who.actor.id)),
        TaskView::Unassigned => tasks.retain(|t| t.assigned_to.is_none()),
    }
    let links = c.store.open_task_issue_links().await.back("/tasks")?;
    let linear = match &s.linear_pull {
        Some(_) => Some(c.store.linear_sync_status().await.back("/tasks")?),
        None => None,
    };
    let locations = c.store.list_locations().await.back("/tasks")?;
    let routines = c.store.list_templates().await.back("/tasks")?;
    let people = c.store.assignable_people().await.back("/tasks")?;
    let names = c.store.person_names().await.back("/tasks")?;
    Ok(layout(
        "Tasks",
        html! {
            nav.task-filters aria-label="Whose tasks" {
                a href="/tasks" aria-current=[(q.who == TaskView::All).then_some("page")] { "Everyone's" }
                a href="/tasks?who=mine" aria-current=[(q.who == TaskView::Mine).then_some("page")] { "Mine" }
                a href="/tasks?who=unassigned" aria-current=[(q.who == TaskView::Unassigned).then_some("page")] { "Up for grabs" }
            }
            (task_table(&tasks, &links, &people, &names))
            @if let Some(status) = &linear {
                (crate::linear_pull::status_line(status.as_ref(), time::OffsetDateTime::now_utc()))
            }
            h2 { "Add a task" }
            @if c.who.can(homestead_domain::auth::Capability::TasksWrite) {
                form.card method="post" action="/tasks" {
                    label { "Title" input name="title" required maxlength="200"; }
                    label { "Details" textarea name="details" rows="2" {} }
                    div.grid {
                        label { "Priority" select name="priority" { (options(TaskPriority::ALL, Some(TaskPriority::Normal))) } }
                        label { "Due" input type="date" name="due_on"; }
                        label { "Where" select name="location_id" {
                            option value="" { "—" }
                            @for l in &locations { option value=(l.id) { (l.name) } }
                        } }
                        label { "Who" select name="assigned_to" { (people_options(&people, None)) } }
                    }
                    label.inline { input type="checkbox" name="mirror_to_linear"; "Also track in Linear" }
                    button { "Add task" }
                }
            }
            (crate::routines::section(&c.who, &routines, &locations))
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct TaskForm {
    title: String,
    #[serde(default)]
    details: String,
    priority: String,
    #[serde(default)]
    due_on: String,
    #[serde(default)]
    location_id: String,
    #[serde(default)]
    assigned_to: String,
    mirror_to_linear: Option<String>,
}

async fn create_task(c: Can<cap::TasksWrite>, Form(f): Form<TaskForm>) -> UiResult {
    const B: &str = "/tasks";
    let input = NewTask {
        title: f.title,
        details: f.details,
        location_id: opt_uuid(&f.location_id, B)?,
        due_on: opt_date(&f.due_on, "due", B)?,
        priority: f.priority.parse::<TaskPriority>().back(B)?,
        mirror_to_linear: f.mirror_to_linear.is_some(),
        assigned_to: opt_uuid(&f.assigned_to, B)?,
    };
    c.store.create_task(input).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct AssignForm {
    #[serde(default)]
    assigned_to: String,
}

async fn assign_task(c: Can<cap::TasksWrite>, Path(id): Path<Uuid>, Form(f): Form<AssignForm>) -> UiResult {
    const B: &str = "/tasks";
    c.store.assign_task(id, opt_uuid(&f.assigned_to, B)?).await.back(B)?;
    Ok(see(B))
}

async fn complete_task(c: Can<cap::TasksWrite>, Path(id): Path<Uuid>) -> UiResult {
    c.store.complete_task(id).await.back("/tasks")?;
    Ok(see("/"))
}

// ── Garden: locations, plantings, harvests ───────────────────────────────────

async fn garden_page(c: Can<cap::GardenRead>) -> UiResult {
    const B: &str = "/garden";
    let locations = c.store.list_locations().await.back(B)?;
    let plantings = c.store.list_active_plantings().await.back(B)?;
    let year_start = Date::from_ordinal_date(c.store.today().await.back(B)?.year(), 1).unwrap_or(Date::MIN);
    let totals = c.store.harvest_totals_since(year_start).await.back(B)?;
    let lots = c.store.list_seed_lots().await.back(B)?;
    Ok(layout(
        "Garden",
        html! {
            h2 { "Growing now" }
            @if plantings.is_empty() { p.muted { "No active plantings yet." } } @else {
                table {
                    tr { th { "Crop" } th { "Where" } th { "Sown" } th { "Status" } }
                    @for (p, loc) in &plantings {
                        tr {
                            td {
                                a href={"/plantings/" (p.id)} { (p.crop) } @if !p.variety.is_empty() { " " span.muted { (p.variety) } }
                                @if let Some(lot) = p.seed_lot_id { " " a.muted href={"/seeds/" (lot)} title="seed lot" { "🌱" } }
                            }
                            td { (loc) }
                            td.muted { (crate::ui::Day(p.sown_on)) }
                            td {
                                @if c.who.can(homestead_domain::auth::Capability::GardenWrite) {
                                    form.inline method="post" action={"/plantings/" (p.id) "/status"} {
                                        select name="status" { (options(PlantingStatus::ALL, Some(p.status))) }
                                        button.small { "Set" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div.grid {
                @if c.who.can(homestead_domain::auth::Capability::GardenWrite) {
                    form.card method="post" action="/harvests" {
                        strong { "Log a harvest" }
                        label { "Planting" select name="planting_id" required {
                            @for (p, loc) in &plantings { option value=(p.id) { (p.crop) " — " (loc) } }
                        } }
                        label { "Date" input type="date" name="harvested_on" required; }
                        div.grid {
                            label { "Quantity" input name="quantity" inputmode="decimal" required; }
                            label { "Unit" select name="unit" { (options(HarvestUnit::ALL, Some(HarvestUnit::Lb))) } }
                        }
                        label { "Notes" input name="notes"; }
                        button { "Log harvest" }
                    }
                }
                @if c.who.can(homestead_domain::auth::Capability::GardenWrite) {
                    form.card method="post" action="/plantings" {
                        strong { "Plant something" }
                        label { "Where" select name="location_id" required {
                            @for l in &locations { option value=(l.id) { (l.name) } }
                        } }
                        label { "Crop" input name="crop" required; }
                        div.grid {
                            label { "Variety" input name="variety"; }
                            label { "Family" input name="family" placeholder="nightshade, brassica…"; }
                        }
                        div.grid {
                            label { "Sown" input type="date" name="sown_on" required; }
                            label { "Expected harvest" input type="date" name="expected_harvest_on"; }
                        }
                        div.grid {
                            label { "Seed lot" select name="seed_lot_id" {
                                option value="" { "—" }
                                @for l in &lots { option value=(l.id) { (l.label()) } }
                            } }
                            label { "Seed used (in the lot's unit)" input name="seed_quantity_used" inputmode="decimal"; }
                        }
                        button { "Add planting" }
                    }
                }
                @if c.who.can(homestead_domain::auth::Capability::GardenWrite) {
                    form.card method="post" action="/locations" {
                        strong { "Add a location" }
                        label { "Name" input name="name" required placeholder="Bed 4, North coop…"; }
                        label { "Kind" select name="kind" { (options(LocationKind::ALL, Some(LocationKind::Bed))) } }
                        label { "Inside" select name="parent_id" {
                            option value="" { "—" }
                            @for l in &locations { option value=(l.id) { (l.name) } }
                        } }
                        label { "Area (sq ft)" input name="area_sqft" inputmode="decimal"; }
                        label { "Notes" input name="notes"; }
                        label { "Soil notes" input name="soil_notes"; }
                        button { "Add location" }
                    }
                }
            }
            h2 { "Harvested this year" }
            @if totals.is_empty() { p.muted { "Nothing logged yet this year." } } @else {
                table { @for (crop, unit, qty) in &totals { tr { td { (crop) } td { (qty.normalize()) " " (unit) } } } }
            }
            (crate::seeds::section(&c.who, &lots))
            h2 { "Locations" }
            ul { @for l in &locations {
                li { a href={"/locations/" (l.id)} { (l.name) } " " span.muted { (l.kind) @if !l.active { " · inactive" } } }
            } }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct LocationForm {
    name: String,
    kind: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    parent_id: String,
    #[serde(default)]
    area_sqft: String,
    #[serde(default)]
    soil_notes: String,
    active: Option<String>,
    /// Set by the edit form's route, never by the browser.
    #[serde(skip)]
    editing: bool,
}

fn location_input(f: LocationForm, back: &str) -> Result<NewLocation, UiError> {
    Ok(NewLocation {
        name: f.name,
        kind: f.kind.parse::<LocationKind>().back_to(back)?,
        notes: f.notes,
        parent_id: match f.parent_id.trim() {
            "" => None,
            id => Some(id.parse().map_err(|_| UiError::invalid_at("unknown selection", back))?),
        },
        // A new place is in use; the edit form's checkbox says otherwise.
        active: f.active.is_some() || !f.editing,
        area_sqft: match f.area_sqft.trim() {
            "" => None,
            q => Some(q.parse().map_err(|_| UiError::invalid_at("area: expected a number", back))?),
        },
        soil_notes: f.soil_notes,
    })
}

async fn create_location(c: Can<cap::GardenWrite>, Form(f): Form<LocationForm>) -> UiResult {
    const B: &str = "/garden";
    let input = location_input(LocationForm { editing: false, ..f }, B)?;
    c.store.create_location(input).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct RotationQuery {
    #[serde(default)]
    crop: String,
    #[serde(default)]
    family: String,
}

/// "Tomatoes were last here 362 days ago", or that they never were.
fn rotation_note(history: &[homestead_domain::Planting], crop: &str, family: &str, today: Date) -> Markup {
    use homestead_domain::crops::last_grown;
    let what = if family.trim().is_empty() { crop.to_owned() } else { format!("{crop} or its family ({family})") };
    html! {
        @match last_grown(history, crop, family, today) {
            None => span.muted { "No record of " (what) " here." },
            Some(l) if l.present => span.warn { (what) " is in the ground here now." },
            Some(l) => span {
                (what) " was last here " @if !l.exact { "at least " } (l.days_before(today)) " days ago (" (l.on) ")"
                @if !l.exact { span.muted { " — its end wasn't recorded, so this counts from sowing" } } "."
            },
        }
    }
}

async fn location_page(
    c: Can<cap::GardenRead>,
    Path(id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<RotationQuery>,
) -> UiResult {
    const B: &str = "/garden";
    let loc = c.store.get_location(id).await.back(B)?;
    let h = c.store.location_history(id).await.back(B)?;
    let places = c.store.list_locations().await.back(B)?;
    let today = c.store.today().await.back(B)?;
    let write = c.who.can(Capability::GardenWrite);
    let soil = c.store.soil_history(id).await.back(B)?;
    let health_filter = homestead_store::health::ObservationFilter { location_id: Some(id), ..Default::default() };
    let observations = c.store.observations(&health_filter, 10).await.back(B)?;
    let history: Vec<homestead_domain::Planting> = h.plantings.iter().map(|(p, _)| p.clone()).collect();
    let (planned, rest): (Vec<_>, Vec<_>) = h.plantings.iter().partition(|(p, _)| p.status == PlantingStatus::Planned);
    let (past, now): (Vec<_>, Vec<_>) = rest.into_iter().partition(|(p, _)| p.status.is_over());
    let elsewhere = |place: &str| place != loc.name;
    let houses_animals = matches!(loc.kind, LocationKind::Coop | LocationKind::Pen | LocationKind::AnimalArea);
    let on_map = crate::map::place_section(&c.who, &c.store, &loc).await.back(B)?;
    let readings = crate::readings::place_section(&c.who, &c.store, id).await?;
    let moved = crate::movements::at_place(&c.who, &c.store, id).await.back(B)?;
    let housing = if c.who.can(Capability::AnimalsRead) {
        c.store.housing().await.back(B)?.into_iter().find(|h| h.location_id == id)
    } else {
        None
    };
    Ok(layout(
        &loc.name,
        html! {
            p.muted {
                @for a in &h.ancestors { a href={"/locations/" (a.id)} { (a.name) } " › " }
                (loc.kind)
                @if let Some(a) = loc.area_sqft { " · " (a.normalize()) " sq ft" }
                @if !loc.active { " · " span.warn { "inactive" } }
                " " (crate::tags::make_button(&c.who, homestead_domain::tags::TagKind::Location, id))
            }
            @if let Some(h) = &housing {
                p class=[h.over_capacity().then_some("warn")] {
                    (h.head) " animals live here"
                    @if let Some(n) = h.capacity { "; it holds " (n) @if h.over_capacity() { ", so it's over capacity" } }
                    "."
                }
            }
            @if (houses_animals || housing.is_some()) && c.who.can(Capability::AnimalsWrite) {
                form.inline method="post" action={"/locations/" (id) "/capacity"} {
                    label { "Holds (head) "
                        input name="capacity" type="number" min="1" value=[housing.as_ref().and_then(|h| h.capacity)] style="width:6rem"; }
                    button.small { "Set" }
                    span.muted {
                        " Oregon State Extension (EC 1644): at least 3 sq ft of coop floor per bird with a run, "
                        "8–10 without one."
                    }
                }
            }
            @if !loc.soil_notes.is_empty() { p { "Soil: " (loc.soil_notes) } }
            @if !loc.notes.is_empty() { p { (loc.notes) } }
            (on_map)
            @if !h.children.is_empty() {
                h2 { "Inside" }
                ul { @for ch in &h.children {
                    li { a href={"/locations/" (ch.id)} { (ch.name) } " " span.muted { (ch.kind) @if !ch.active { " · inactive" } } }
                } }
            }
            h2 { "Growing now" }
            @if now.is_empty() { p.muted { "Nothing in the ground." } } @else {
                table { tr { th { "Crop" } th { "Family" } th { "Sown" } th { "Expected harvest" } th { "Status" } }
                    @for (p, place) in &now { tr {
                        td { a href={"/plantings/" (p.id)} { (p.crop) } @if !p.variety.is_empty() { " " span.muted { (p.variety) } }
                             @if elsewhere(place) { br; span.muted { (place) } } }
                        td { (p.family) } td { (crate::ui::Day(p.sown_on)) }
                        td { @if let Some(d) = p.expected_harvest_on { (crate::ui::Day(d)) } } td { (p.status) }
                    } }
                }
            }
            @if !planned.is_empty() {
                h2 { "Planned here" }
                table { tr { th { "Crop" } th { "Sow" } th { "Rotation" } }
                    @for (p, place) in &planned { tr {
                        td { (p.crop) @if elsewhere(place) { br; span.muted { (place) } } }
                        td { (crate::ui::Day(p.sown_on)) }
                        td { (rotation_note(&history.iter().filter(|o| o.id != p.id).cloned().collect::<Vec<_>>(), &p.crop, &p.family, today)) }
                    } }
                }
            }
            h2 { "Grown here before" }
            @if past.is_empty() { p.muted { "Nothing recorded." } } @else {
                table { tr { th { "Crop" } th { "Family" } th { "Sown" } th { "Ended" } th { "Outcome" } }
                    @for (p, place) in &past { tr {
                        td { a href={"/plantings/" (p.id)} { (p.crop) } @if elsewhere(place) { br; span.muted { (place) } } }
                        td { (p.family) } td { (crate::ui::Day(p.sown_on)) }
                        td { @if let Some(e) = p.ended_on { (e) } @else { span.muted { "not recorded" } } }
                        td { (p.status) }
                    } }
                }
            }
            form.card method="get" action={"/locations/" (loc.id)} {
                strong { "Rotation check" }
                div.grid {
                    label { "Crop" input name="crop" value=(q.crop) required; }
                    label { "Family" input name="family" value=(q.family) placeholder="nightshade, brassica…"; }
                }
                @if !q.crop.trim().is_empty() { p { (rotation_note(&history, &q.crop, &q.family, today)) } }
                button.small { "Check" }
            }
            h2 { "Harvested here" }
            @if h.harvests.is_empty() { p.muted { "No harvests." } } @else {
                table { @for hv in &h.harvests {
                    tr { td { a href={"/harvests/" (hv.id)} { (crate::ui::Day(hv.harvested_on)) } } td { (hv.crop) @if elsewhere(&hv.place) { br; span.muted { (hv.place) } } }
                         td { (hv.quantity.normalize()) " " (hv.unit) } }
                } }
            }
            (crate::soil::section(&c.who, loc.id, today, &soil, &loc.name))
            (readings)
            (moved)
            h2 { "Crop health" }
            (crate::health::table(&observations))
            p { a href={"/health?location_id=" (loc.id)} { "All crop health here" } }
            h2 { "Watering" }
            p.muted { (h.waterings) " watering record" @if h.waterings != 1 { "s" } "." }
            @if !h.irrigation.is_empty() {
                table { @for (at, litres, method, place) in &h.irrigation {
                    tr { td { (at.date()) } td { (method) } td { (place) }
                         td { @if let Some(l) = litres { (homestead_domain::water::litres_to_gallons(*l)) " gal" } } }
                } }
            }
            h2 { "Tasks" }
            @if h.tasks.is_empty() { p.muted { "None." } } @else {
                ul { @for (_, title, due, status) in &h.tasks {
                    li { (title) " " span.muted { (status) @if let Some(d) = due { " · " (d) } } }
                } }
            }
            @if write {
                details.card {
                    summary { "Edit this place" }
                    form method="post" action={"/locations/" (loc.id)} style="display:grid;gap:.85rem" {
                        label { "Name" input name="name" value=(loc.name) required; }
                        label { "Kind" select name="kind" { (options(LocationKind::ALL, Some(loc.kind))) } }
                        label { "Inside" select name="parent_id" {
                            option value="" { "—" }
                            @for l in places.iter().filter(|l| l.id != loc.id) {
                                option value=(l.id) selected[Some(l.id) == loc.parent_id] { (l.name) }
                            }
                        } }
                        label { "Area (sq ft)" input name="area_sqft" inputmode="decimal"
                            value=[loc.area_sqft.map(|a| a.normalize().to_string())]; }
                        label { "Soil notes" input name="soil_notes" value=(loc.soil_notes); }
                        label { "Notes" input name="notes" value=(loc.notes); }
                        label.inline { input type="checkbox" name="active" checked[loc.active]; "In use" }
                        button { "Save" }
                    }
                }
            }
        },
    )
    .into_response())
}

async fn update_location(c: Can<cap::GardenWrite>, Path(id): Path<Uuid>, Form(f): Form<LocationForm>) -> UiResult {
    let back = format!("/locations/{id}");
    let input = location_input(LocationForm { editing: true, ..f }, &back)?;
    c.store.update_location(id, input).await.back_to(&back)?;
    Ok(see(&back))
}

#[derive(Deserialize)]
struct PlantingForm {
    location_id: String,
    crop: String,
    #[serde(default)]
    variety: String,
    #[serde(default)]
    family: String,
    sown_on: String,
    #[serde(default)]
    expected_harvest_on: String,
    #[serde(default)]
    seed_lot_id: String,
    #[serde(default)]
    seed_quantity_used: String,
}

async fn create_planting(c: Can<cap::GardenWrite>, Form(f): Form<PlantingForm>) -> UiResult {
    const B: &str = "/garden";
    let input = NewPlanting {
        location_id: opt_uuid(&f.location_id, B)?.ok_or_else(|| UiError::invalid("choose a location", B))?,
        crop: f.crop,
        variety: f.variety,
        family: f.family,
        sown_on: req_date(&f.sown_on, "sown", B)?,
        expected_harvest_on: opt_date(&f.expected_harvest_on, "expected harvest", B)?,
        notes: String::new(),
        seed_lot_id: opt_uuid(&f.seed_lot_id, B)?,
        seed_quantity_used: match f.seed_quantity_used.trim() {
            "" => None,
            q => Some(decimal(q, "seed used", B)?),
        },
        ..NewPlanting::at(Uuid::nil(), "", date!(2026 - 01 - 01))
    };
    c.store.create_planting(input).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct StatusForm {
    status: String,
}

async fn set_planting_status(c: Can<cap::GardenWrite>, Path(id): Path<Uuid>, Form(f): Form<StatusForm>) -> UiResult {
    const B: &str = "/garden";
    c.store.set_planting_status(id, f.status.parse::<PlantingStatus>().back(B)?).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct HarvestForm {
    planting_id: String,
    harvested_on: String,
    quantity: String,
    unit: String,
    #[serde(default)]
    notes: String,
}

async fn log_harvest(c: Can<cap::GardenWrite>, Form(f): Form<HarvestForm>) -> UiResult {
    const B: &str = "/garden";
    let input = NewHarvest {
        planting_id: opt_uuid(&f.planting_id, B)?.ok_or_else(|| UiError::invalid("choose a planting", B))?,
        harvested_on: req_date(&f.harvested_on, "date", B)?,
        quantity: decimal(&f.quantity, "quantity", B)?,
        unit: f.unit.parse::<HarvestUnit>().back(B)?,
        notes: f.notes,
    };
    c.store.log_harvest(input).await.back(B)?;
    Ok(see(B))
}

// ── Inventory ────────────────────────────────────────────────────────────────

/// "about 12 days" from a pace, or a dash when nothing's been used lately.
pub(crate) fn lasts(days: Option<i64>) -> String {
    match days {
        None => "—".into(),
        Some(0) => "out today".into(),
        Some(1) => "about a day".into(),
        Some(d) => format!("about {d} days"),
    }
}

async fn inventory_page(c: Can<cap::InventoryRead>) -> UiResult {
    let items = c.store.list_inventory().await.back("/inventory")?;
    let paces = c.store.usage_paces().await.back("/inventory")?;
    Ok(layout(
        "Inventory",
        html! {
            @if items.is_empty() { p.muted { "No items tracked yet." } } @else {
                table {
                    tr { th { "Item" } th { "On hand" } th { "At this pace" } th { "Reorder at" } th { "Adjust" } }
                    @for i in &items {
                        @let left = paces.get(&i.id).and_then(|p| p.days_left(i.on_hand));
                        tr {
                            td { a href={"/inventory/" (i.id)} { (i.name) } }
                            td class=[i.is_low().then_some("warn")] { (i.on_hand.normalize()) " " (i.unit) }
                            td class=[left.is_some_and(|d| d <= RUNS_OUT_SOON_DAYS).then_some("warn")] { (lasts(left)) }
                            td.muted { (i.reorder_level.normalize()) }
                            td {
                                @if c.who.can(homestead_domain::auth::Capability::InventoryAdjust) {
                                    form.inline method="post" action={"/inventory/" (i.id) "/adjust"} {
                                        input name="delta" inputmode="decimal" placeholder="+5 / -2" required style="width:6rem";
                                        input name="reason" placeholder="why" required style="width:8rem";
                                        button.small { "Apply" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            h2 { "Track a new item" }
            @if c.who.can(homestead_domain::auth::Capability::InventoryAdjust) {
                form.card method="post" action="/inventory" {
                    label { "Name" input name="name" required placeholder="Layer feed"; }
                    div.grid {
                        label { "Unit" input name="unit" required placeholder="bag"; }
                        label { "Reorder at" input name="reorder_level" inputmode="decimal" value="0"; }
                    }
                    button { "Add item" }
                }
            }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct ItemForm {
    name: String,
    unit: String,
    reorder_level: String,
}

async fn create_item(c: Can<cap::InventoryAdjust>, Form(f): Form<ItemForm>) -> UiResult {
    const B: &str = "/inventory";
    let level =
        if f.reorder_level.trim().is_empty() { Decimal::ZERO } else { decimal(&f.reorder_level, "reorder at", B)? };
    c.store
        .create_inventory_item(NewInventoryItem { name: f.name, unit: f.unit, reorder_level: level })
        .await
        .back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct AdjustForm {
    delta: String,
    reason: String,
}

async fn adjust_stock(c: Can<cap::InventoryAdjust>, Path(id): Path<Uuid>, Form(f): Form<AdjustForm>) -> UiResult {
    const B: &str = "/inventory";
    let delta = decimal(f.delta.trim().trim_start_matches('+'), "adjustment", B)?;
    c.store.adjust_stock(id, StockAdjustment { delta, reason: f.reason }).await.back(B)?;
    Ok(see(B))
}

// ── Integrations / ops ───────────────────────────────────────────────────────

async fn ops_page(c: Can<cap::SystemsRead>) -> UiResult {
    const B: &str = "/ops";
    let bus = c.store.bus_stats().await.back(B)?;
    let troubled = c.store.troubled_deliveries(50).await.back(B)?;
    let recent = c.store.recent_events(25).await.back(B)?;
    Ok(layout(
        "Integrations",
        html! {
            div.grid {
                div.card { div.stat { (bus.delivered) } div.muted { "delivered" } }
                div.card { div.stat { (bus.pending + bus.unfanned) } div.muted { "queued" } }
                div.card { div.stat class=[(bus.dead > 0).then_some("warn")] { (bus.dead) } div.muted { "dead-lettered" } }
            }
            p { a href="/ops/social" { "Instagram and Facebook setup" } }
            h2 { "Needs attention" }
            @if troubled.is_empty() { p.ok { "All integrations healthy." } } @else {
                table {
                    tr { th { "Event" } th { "Sink" } th { "State" } th { "Last error" } th {} }
                    @for d in &troubled {
                        tr {
                            td { "#" (d.event_id) " " (d.kind) }
                            td { (d.sink) }
                            td class=[(d.status == "dead").then_some("warn")] { (d.status) " · " (d.attempts) " tries" }
                            td { code { (d.last_error.as_deref().unwrap_or("")) } }
                            td { @if d.status == "dead" {
                                form.inline method="post" action={"/ops/retry/" (d.event_id) "/" (d.sink)} { button.small { "Retry" } }
                            } }
                        }
                    }
                }
            }
            h2 { "Recent events" }
            table {
                @for e in &recent {
                    tr {
                        td.muted { "#" (e.id) }
                        td { (e.kind) }
                        td { @match homestead_domain::Event::from_parts(&e.kind, e.payload.clone()) {
                            Ok(ev) => (ev.summary()),
                            Err(_) => span.warn { "undecodable" },
                        } }
                    }
                }
            }
        },
    )
    .into_response())
}

async fn retry_delivery(c: Can<cap::SystemsAdmin>, Path((event_id, sink)): Path<(i64, String)>) -> UiResult {
    c.store.retry_dead(event_id, &sink).await.back("/ops")?;
    Ok(see("/ops"))
}

async fn social_page(c: Can<cap::SystemsRead>) -> UiResult {
    let _ = c;
    let spec = homestead_integrations::buffer::Spec::parse(include_str!("../../../buffer/account.toml"))
        .map_err(|e| UiError::invalid(e, "/ops"))?;
    Ok(layout(
        "Instagram and Facebook",
        html! {
            p { "Buffer holds the drafts. Connecting the accounts happens in Buffer's own site; this list is what " code { "buffer/account.toml" } " says should be there." }
            ol {
                li { "Rename the Buffer organization to " strong { (spec.account.organization) } ". Buffer → Settings → Organization." }
                @for channel in &spec.channels {
                    li {
                        "Connect " (channel.service) " as @" (channel.handle) ". Buffer → Channels → Connect."
                        @if channel.service == "instagram" {
                            " Instagram has to be a Business or Creator account linked to the Facebook Page."
                        }
                    }
                }
                li { "A harvest or a new planting becomes a draft idea, not a post. A person adds the photo (no faces or kids unless they've agreed) and approves it." }
            }
            p.muted { "Buffer's API cannot connect a channel or rename the organization. " code { "homestead buffer-verify" } " checks this list against the live account." }
            p { a href="/ops" { "Integrations" } }
        },
    )
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use maud::Render;

    #[test]
    fn days_read_like_a_person_says_them_and_keep_the_iso_date() {
        assert_eq!(
            Day(date!(2026 - 10 - 03)).render().into_string(),
            r#"<time datetime="2026-10-03">Sat, Oct 3, 2026</time>"#
        );
    }

    /// `form.inline input` outranks `input[type=checkbox]`, so it once gave
    /// every role checkbox on People an 8rem width and field padding. Any
    /// scoped rule that sizes a bare `input` must leave checkboxes out.
    #[test]
    fn no_scoped_input_rule_resizes_checkboxes() {
        let mut css = CSS.to_string();
        while let Some(a) = css.find("/*") {
            let b = a + css[a..].find("*/").expect("unclosed comment") + 2;
            css.replace_range(a..b, "");
        }
        let sizing = ["width", "height", "padding"];
        let mut offenders = vec![];
        for rule in css.split('}') {
            let Some((sel, decls)) = rule.rsplit_once('{') else { continue };
            let sel = sel.rsplit('{').next().unwrap_or_default();
            if !sizing.iter().any(|p| decls.contains(p)) {
                continue;
            }
            // Split on commas outside parentheses.
            let (mut depth, mut start, mut parts) = (0, 0, vec![]);
            for (i, c) in sel.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    ',' if depth == 0 => {
                        parts.push(&sel[start..i]);
                        start = i + 1;
                    }
                    _ => {}
                }
            }
            parts.push(&sel[start..]);
            for s in parts.iter().map(|s| s.trim()) {
                let last = s.rsplit([' ', '>']).next().unwrap_or_default();
                let scoped = last.len() < s.len();
                let bare = last == "input" || (last.starts_with("input:") && !last.contains("checkbox"));
                if scoped && bare {
                    offenders.push(s.to_string());
                }
            }
        }
        assert!(offenders.is_empty(), "rules that resize checkboxes: {offenders:?}");
    }

    #[test]
    fn linear_markdown_reads_as_plain_text() {
        // Verbatim fragments of SUN-7, SUN-18 and SUN-34 as the API returns them.
        assert_eq!(
            plain("register **within 30 days of first receiving charitable assets**. Fee $50."),
            "register within 30 days of first receiving charitable assets. Fee $50."
        );
        assert_eq!(
            plain(
                "The Donate page ([https://www.sunbrewedhomestead.org/donate](<https://www.sunbrewedhomestead.org/donate>)) says"
            ),
            "The Donate page (https://www.sunbrewedhomestead.org/donate) says"
        );
        assert_eq!(
            plain("from [onboarding@resend.dev](<mailto:onboarding@resend.dev>) and `homestead.toml`"), // oss-publish:allow: Resend's public test sender
            "from onboarding@resend.dev and homestead.toml" // oss-publish:allow: Resend's public test sender
        );
        assert_eq!(
            plain("add them to `discord/server.toml` \\[members\\]"),
            "add them to discord/server.toml [members]"
        );
        assert_eq!(plain("a [dangling bracket and 3 * 4"), "a [dangling bracket and 3 * 4");
    }

    #[test]
    fn chat_renders_a_deadline_reminder_as_a_list_of_links() {
        let md = "Website refresh: deadlines\n\
• Make sure forms reach us ([SUN-20](<https://linear.app/sunbrewedhomestead/issue/SUN-20>)): due in 9 days, Fri Oct 9\n\
• Say we're a 501(c)(3) ([SUN-18](<https://linear.app/sunbrewedhomestead/issue/SUN-18>)): due **today**, Fri Oct 9";
        let html = chat(md).into_string();
        assert!(html.starts_with("<p>Website refresh: deadlines</p><ul><li>"));
        assert!(html.contains("<a href=\"https://linear.app/sunbrewedhomestead/issue/SUN-20\">SUN-20</a>"));
        assert!(html.contains("<strong>today</strong>"));
        assert!(html.contains("501(c)(3)"));
        assert!(!html.contains("](<"), "{html}");
        assert!(!html.contains("**"), "{html}");
    }

    #[test]
    fn chat_drops_an_unsafe_url_and_keeps_the_words() {
        let html = chat("see [click](<javascript:alert(1)>) and `homestead.toml`").into_string();
        assert!(!html.contains("javascript:"), "{html}");
        assert!(html.contains("see click and"), "{html}");
        assert!(html.contains("<code>homestead.toml</code>"), "{html}");
        let escaped = chat("3 * 4 and \\[members\\]").into_string();
        assert_eq!(escaped, "<p>3 * 4 and [members]</p>");
    }

    #[test]
    fn ages_read_like_a_person_would_say_them() {
        let m = time::Duration::minutes;
        let said: Vec<String> = [0, 1, 7, 61, 300, 3000].map(|n| ago(m(n))).to_vec();
        assert_eq!(said, ["just now", "a minute ago", "7 minutes ago", "an hour ago", "5 hours ago", "2 days ago"]);
    }
}

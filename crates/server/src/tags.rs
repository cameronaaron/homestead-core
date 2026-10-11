//! QR tags: printing them, and the page a scan opens.
//!
//! `/t/{code}` is public on purpose. Most of the people doing the work are
//! in the group chat and never make an account, so a printed code stuck on a
//! tool, a bin or the coop is their way in: the tag is the credential, like
//! a setup link. Each action there is one small, bounded write, attributed
//! to the signed-in person when their cookie came along, otherwise to the
//! name they typed plus the tag's code. Retiring a tag revokes it.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use homestead_domain::LocationKind;
use homestead_domain::animals::Species;
use homestead_domain::auth::Capability;
use homestead_domain::community::WorkingGroup;
use homestead_domain::equipment::EquipmentStatus;
use homestead_domain::inventory::{MovementKind, NewMovement};
use homestead_domain::stand::NewCompostDropoff;
use homestead_domain::tags::{NewLoan, Tag, TagKind, normalise_code, visitor_name};
use maud::{Markup, PreEscaped, html};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Can, Principal, cap, deny};
use crate::ui::{Back, UiError, UiResult, layout, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new().route("/tags", get(sheet).post(make)).route("/tags/{code}/retire", post(retire))
}

/// Reachable without signing in; see the module docs.
pub fn public_routes() -> Router<AppState> {
    Router::new()
        .route("/t/{code}", get(page))
        .route("/t/{code}/checkout", post(checkout))
        .route("/t/{code}/return", post(give_back))
        .route("/t/{code}/problem", post(problem))
        .route("/t/{code}/use", post(use_stock))
        .route("/t/{code}/eggs", post(eggs))
        .route("/t/{code}/compost", post(compost))
        .route("/t/{code}/in", post(gate_in))
        .route("/t/{code}/out", post(gate_out))
}

// ── Printing ────────────────────────────────────────────────────────────────

/// What a scan opens: the configured public address, else this request's.
pub(crate) fn base_url(s: &AppState, headers: &HeaderMap) -> String {
    if let Some(u) = &s.public_url {
        return u.to_string();
    }
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost");
    let scheme = if s.secure_cookies { "https" } else { "http" };
    format!("{scheme}://{host}")
}

/// A QR code as inline SVG, dark on white whatever the page's theme (a
/// scanner needs the contrast).
pub(crate) fn qr(text: &str, size: u32) -> Markup {
    match qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::M) {
        Ok(code) => {
            let svg = code
                .render::<qrcode::render::svg::Color<'_>>()
                .min_dimensions(size, size)
                .dark_color(qrcode::render::svg::Color("#000000"))
                .light_color(qrcode::render::svg::Color("#ffffff"))
                .build();
            // Drop the XML declaration; the <svg> is going inside HTML.
            let start = svg.find("<svg").unwrap_or(0);
            PreEscaped(svg[start..].to_owned())
        }
        Err(_) => html! { span.warn { "(too long for a QR code)" } },
    }
}

const PRINT_CSS: &str = r"
.labels{display:grid;grid-template-columns:repeat(auto-fill,minmax(2.3in,1fr));gap:.2in;margin:1rem 0}
.label{background:#fff;color:#000;border:1px dashed #999;border-radius:8px;padding:.12in;text-align:center;break-inside:avoid}
.label svg{display:block;width:1.7in;height:1.7in;margin:0 auto}
.label strong{display:block;font-size:14pt;line-height:1.2;margin-top:.05in}
.label small{display:block;font:10pt ui-monospace,monospace;letter-spacing:.08em}
@media print{
  header.appbar,nav,.noprint,#field-queue,.banner,.skip,h1{display:none!important}
  body,main{background:#fff!important;margin:0!important;padding:0!important}
  .label{border:1px solid #bbb}
}";

#[derive(Deserialize)]
struct SheetQuery {
    #[serde(default)]
    kind: String,
}

/// Every live tag the viewer may manage, as a printable sheet.
async fn sheet(
    c: Can<cap::TasksRead>,
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<SheetQuery>,
) -> UiResult {
    let base = base_url(&s, &headers);
    let only: Option<TagKind> = q.kind.parse().ok();
    let tags: Vec<_> = c
        .store
        .list_tags()
        .await
        .back("/")?
        .into_iter()
        .filter(|t| c.who.can(t.tag.kind.manage_cap()) && only.is_none_or(|k| k == t.tag.kind))
        .collect();
    let can_gate = c.who.can(TagKind::Gate.manage_cap());
    Ok(layout(
        "QR tags",
        html! {
            style { (PreEscaped(PRINT_CSS)) }
            div.noprint {
                p {
                    "Stick one on a tool, a feed bin, the coop or a bed. Anyone who scans it with their phone's "
                    "camera can check the tool out, say they used some feed or log eggs, with no account. "
                    "The code is the key: if a tag is lost or misused, retire it and print a new one."
                }
                p.muted {
                    "Make a tag from a tool's, item's, place's or animal's page. Outdoors, print on weatherproof "
                    "(polyester) labels or laminate them."
                }
                form.inline method="get" action="/tags" {
                    select name="kind" {
                        option value="" { "Every kind" }
                        @for k in TagKind::ALL { @if c.who.can(k.manage_cap()) {
                            option value=(k) selected[only == Some(*k)] { (k) }
                        } }
                    }
                    button.small { "Show" }
                    " "
                    button.small type="button" onclick="print()" { "Print" }
                }
                @if can_gate {
                    form.inline method="post" action="/tags" {
                        input type="hidden" name="kind" value="gate";
                        button.small { "Make the gate sign-in tag" }
                    }
                }
            }
            @if tags.is_empty() { p.muted { "No tags yet." } } @else {
                div.labels {
                    @for t in &tags {
                        @let url = format!("{base}/t/{}", t.tag.code);
                        div.label {
                            (qr(&url, 160))
                            strong { (t.tag.label) }
                            small { (t.tag.code) }
                            form.noprint method="post" action={"/tags/" (t.tag.code) "/retire"} {
                                a href=(url) { "Open" } " "
                                button.small type="submit" { "Retire" }
                            }
                        }
                    }
                }
            }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct MakeForm {
    kind: String,
    #[serde(default)]
    target: String,
}

/// Makes (or finds) the tag for a thing, then shows the sheet for its kind.
async fn make(c: Can<cap::TasksRead>, Form(f): Form<MakeForm>) -> UiResult {
    const B: &str = "/tags";
    let kind = f.kind.parse::<TagKind>().back(B)?;
    if !c.who.can(kind.manage_cap()) {
        return Ok(deny(&c.who, kind.manage_cap()));
    }
    let target = crate::ui::opt_uuid(&f.target, B)?;
    let mut tries = 0;
    let tag = loop {
        match c.store.ensure_tag(kind, target, &fresh_code()).await {
            Err(homestead_store::StoreError::Conflict(_)) if tries < 3 => tries += 1,
            other => break other.back(B)?,
        }
    };
    Ok(see(&format!("/tags?kind={}", tag.kind)))
}

fn fresh_code() -> String {
    homestead_domain::tags::new_code(|| {
        let mut b = [0u8; 1];
        getrandom::fill(&mut b).expect("the OS random source is available");
        b[0]
    })
}

async fn retire(c: Can<cap::TasksRead>, Path(code): Path<String>) -> UiResult {
    const B: &str = "/tags";
    let code = normalise_code(&code).ok_or_else(|| UiError::invalid("that isn't a tag code", B))?;
    let tag = c.store.tag(&code).await.back(B)?;
    if !c.who.can(tag.kind.manage_cap()) {
        return Ok(deny(&c.who, tag.kind.manage_cap()));
    }
    c.store.retire_tag(&code).await.back(B)?;
    Ok(see(B))
}

/// The "Make a QR tag" button for a thing's own page.
pub(crate) fn make_button(who: &Principal, kind: TagKind, target: Uuid) -> Markup {
    html! {
        @if who.can(kind.manage_cap()) {
            form.inline method="post" action="/tags" {
                input type="hidden" name="kind" value=(kind);
                input type="hidden" name="target" value=(target);
                button.small { "QR tag" }
            }
        }
    }
}

// ── The page a scan opens ───────────────────────────────────────────────────

/// A scan's URL carries the credential: keep it out of other sites' logs
/// and out of search engines.
fn private(page: Markup) -> Response {
    let mut r = page.into_response();
    let h = r.headers_mut();
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert("x-robots-tag", HeaderValue::from_static("noindex, nofollow"));
    r
}

/// Remembers the name typed at a tag in this browser, so the next scan is
/// one tap. Nothing is sent anywhere but the form it fills.
const REMEMBER_NAME: &str = r#"<script>
(function(){var k="sbh-tag-name",v="";try{v=localStorage.getItem(k)||""}catch(e){}
document.querySelectorAll('input[name=name]').forEach(function(i){if(!i.value)i.value=v;
i.form.addEventListener("submit",function(){try{localStorage.setItem(k,i.value.trim())}catch(e){}})})})()
</script>"#;

/// The short confirmations a finished action shows. A fixed list, so the
/// query string can't put words on the page.
fn done_message(key: &str) -> Option<&'static str> {
    Some(match key {
        "out" => "Checked out to you. Scan it again to return it.",
        "back" => "Returned. Thanks!",
        "problem" => "Thanks. It's marked as needing repair and a fix-it task is on the list.",
        "noted" => "Thanks. Someone will look into it; it's on the task list.",
        "used" => "Recorded. Thanks!",
        "eggs" => "Eggs logged. Thanks!",
        "compost" => "Drop-off logged. Thanks for feeding the pile!",
        "arrived" => "You're signed in. Scan again on your way out.",
        "left" => "Signed out, and your hours are recorded. Thanks for your time!",
        "left-long" => "Signed out. That was over 12 hours, so a lead will fill in your hours.",
        _ => return None,
    })
}

#[derive(Deserialize)]
struct PageQuery {
    #[serde(default)]
    done: String,
}

async fn resolve(s: &AppState, raw: &str) -> Result<Tag, UiError> {
    let code = normalise_code(raw).ok_or_else(|| UiError::invalid("That isn't one of our tags.", "/login"))?;
    s.tag_store(None, "", &code).tag(&code).await.back("/login")
}

fn concern_form(here: &str, viewer: Option<&Principal>, title: &str, hint: &str) -> Markup {
    html! {
        form.card method="post" action={(here) "/problem"} {
            strong { (title) }
            (who_field(viewer))
            label { "What you noticed" input name="problem" required maxlength="500" placeholder=(hint); }
            button { "Tell the team" }
        }
    }
}

/// The name field, for someone who isn't signed in.
fn who_field(viewer: Option<&Principal>) -> Markup {
    html! {
        @match viewer {
            Some(p) => p.muted { "As " (p.actor.display_name) "." }
            None => label { "Your name" input name="name" required maxlength="80" autocomplete="name"; }
        }
    }
}

async fn page(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Query(q): Query<PageQuery>,
) -> Result<Response, UiError> {
    let tag = resolve(&s, &raw).await?;
    let viewer = crate::auth::viewer(&s, &headers).await;
    let store = s.tag_store(viewer.as_ref(), "", &tag.code);
    let here = format!("/t/{}", tag.code);
    let today = store.today().await.back_to(&here)?;
    let done = done_message(&q.done);
    let body = match (tag.kind, tag.target_id) {
        (TagKind::Equipment, Some(id)) => {
            let e = store.get_equipment(id).await.back_to(&here)?;
            let loan = store.open_loan(id).await.back_to(&here)?;
            let usable = !matches!(e.status, EquipmentStatus::OutOfService | EquipmentStatus::Retired);
            html! {
                p {
                    span.pill { (e.status) } " " (e.category)
                    @if let Some(l) = &e.location_name { " · lives at " (l) }
                }
                @if let Some(l) = &loan {
                    p class=[l.overdue(today).then_some("warn")] {
                        "With " strong { (l.borrower) } " since " (l.out_at.date())
                        @if let Some(d) = l.due_on { ", due back " (crate::ui::Day(d)) }
                        "."
                    }
                    form.card method="post" action={(here) "/return"} {
                        strong { "Bring it back" }
                        (who_field(viewer.as_ref()))
                        label { "Anything to know? (optional)" input name="note" maxlength="500" placeholder="Needs sharpening, left in the shed…"; }
                        button { "Return it" }
                    }
                } @else if usable {
                    form.card method="post" action={(here) "/checkout"} {
                        strong { "Take it" }
                        (who_field(viewer.as_ref()))
                        label { "Back by (optional)" input name="due_on" type="date" min=(today); }
                        button { "Check it out" }
                    }
                } @else {
                    p.warn { "It's " (e.status) ": please don't use it." }
                }
                form.card method="post" action={(here) "/problem"} {
                    strong { "Something wrong with it?" }
                    (who_field(viewer.as_ref()))
                    label { "What's wrong" input name="problem" required maxlength="500" placeholder="Blade chipped, won't start…"; }
                    button { "Report it" }
                }
                @if viewer.as_ref().is_some_and(|v| v.can(Capability::EquipmentRead)) {
                    p { a href={"/equipment/" (id)} { "Open it in the app" } }
                }
            }
        }
        (TagKind::InventoryItem, Some(id)) => {
            let item = store.get_inventory_item(id).await.back_to(&here)?;
            let places: Vec<_> = store
                .item_balances(id)
                .await
                .back_to(&here)?
                .into_iter()
                .filter(|b| b.on_hand > rust_decimal::Decimal::ZERO && b.location_id.is_some())
                .collect();
            html! {
                p class=[item.is_low().then_some("warn")] {
                    strong { (item.on_hand.normalize()) " " (item.unit) } " on hand"
                    @if item.is_low() { " — running low; it's on the reorder list." }
                }
                form.card method="post" action={(here) "/use"} {
                    strong { "Used some?" }
                    (who_field(viewer.as_ref()))
                    label { "How many " (item.unit) input name="quantity" inputmode="decimal" required placeholder="1"; }
                    @if places.len() > 1 {
                        label { "Taken from" select name="place" {
                            @for b in &places {
                                option value=[b.location_id] { (b.location_name.as_deref().unwrap_or("")) " (" (b.on_hand.normalize()) ")" }
                            }
                        } }
                    }
                    button { "Record it" }
                }
                @if viewer.as_ref().is_some_and(|v| v.can(Capability::InventoryRead)) {
                    p { a href={"/inventory/" (id)} { "Open it in the app" } }
                }
            }
        }
        (TagKind::Animal, Some(id)) => {
            let a = store.get_animal(id).await.back_to(&here)?;
            let birds = matches!(a.species, Species::Chicken | Species::Duck);
            let (laid, cracked) = if birds { store.eggs_on(id, today).await.back_to(&here)? } else { (0, 0) };
            html! {
                p { span.pill { (a.species) } @if a.head_count > 1 { " " (a.head_count) " birds" } }
                @if birds {
                    p { "Today so far: " strong { (laid) " eggs" } @if cracked > 0 { " (" (cracked) " cracked)" } }
                    form.card method="post" action={(here) "/eggs"} {
                        strong { "Collected eggs" }
                        (who_field(viewer.as_ref()))
                        div.grid {
                            label { "How many" input name="collected" type="number" min="1" max="200" required inputmode="numeric"; }
                            label { "Cracked or dirty" input name="damaged" type="number" min="0" max="200" value="0" inputmode="numeric"; }
                        }
                        button { "Log them" }
                    }
                }
                (concern_form(&here, viewer.as_ref(), "Something wrong with them?", "Limping, not eating, a hole in the fence…"))
                @if viewer.as_ref().is_some_and(|v| v.can(Capability::AnimalsRead)) {
                    p { a href={"/animals/" (id)} { "Open in the app" } }
                }
            }
        }
        (TagKind::Location, Some(id)) => {
            let l = store.get_location(id).await.back_to(&here)?;
            html! {
                p { span.pill { (l.kind) } }
                @if l.kind == LocationKind::Compost {
                    form.card method="post" action={(here) "/compost"} {
                        strong { "Dropping off scraps?" }
                        (who_field(viewer.as_ref()))
                        label { "About how many pounds (optional)" input name="weight_lb" inputmode="decimal" placeholder="5"; }
                        label { "What was it (optional)" input name="note" maxlength="500" placeholder="Kitchen scraps, coffee grounds…"; }
                        button { "Log it" }
                    }
                }
                (concern_form(&here, viewer.as_ref(), "Something wrong here?", "Leak, broken latch, pests…"))
                @if viewer.as_ref().is_some_and(|v| v.can(Capability::GardenRead)) {
                    p { a href={"/locations/" (id)} { "Open in the app" } }
                }
            }
        }
        (TagKind::Gate, None) => html! {
            p { "Sign in when you arrive and out when you leave. It records your volunteer hours and tells us who's on site in an emergency." }
            div.grid {
                form.card method="post" action={(here) "/in"} {
                    strong { "Arriving" }
                    (who_field(viewer.as_ref()))
                    label { "Here for" select name="group" {
                        @for g in WorkingGroup::ALL { option value=(g) selected[*g == WorkingGroup::FarmLabor] { (g.label()) } }
                    } }
                    button { "Sign in" }
                }
                form.card method="post" action={(here) "/out"} {
                    strong { "Leaving" }
                    (who_field(viewer.as_ref()))
                    button { "Sign out" }
                }
            }
            p.muted { "First time here? Ask a lead for the volunteer waiver before you start." }
        },
        _ => html! { p.muted { "Nothing to do with this tag yet." } },
    };
    let title = tag.label.clone();
    Ok(private(layout(
        &title,
        html! {
            @if let Some(m) = done { p.banner role="status" { (m) } }
            (body)
            (PreEscaped(REMEMBER_NAME))
        },
    )))
}

#[derive(Deserialize)]
struct ActionForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    due_on: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    problem: String,
    #[serde(default)]
    quantity: String,
    #[serde(default)]
    place: String,
    #[serde(default)]
    collected: String,
    #[serde(default)]
    damaged: String,
    #[serde(default)]
    weight_lb: String,
    #[serde(default)]
    group: String,
}

fn target_of(tag: &Tag, kind: TagKind, what: &str) -> Result<Uuid, UiError> {
    match tag.target_id {
        Some(id) if tag.kind == kind => Ok(id),
        _ => Err(UiError::invalid_at(format!("That isn't {what}'s tag."), &format!("/t/{}", tag.code))),
    }
}

/// Who's acting, and the store that writes as them.
async fn actor(
    s: &AppState,
    headers: &HeaderMap,
    tag: &Tag,
    typed: &str,
) -> Result<(String, homestead_store::Store), UiError> {
    let here = format!("/t/{}", tag.code);
    let viewer = crate::auth::viewer(s, headers).await;
    let name = match &viewer {
        Some(p) => p.actor.display_name.clone(),
        None => visitor_name(typed).back_to(&here)?,
    };
    let store = s.tag_store(viewer.as_ref(), &name, &tag.code);
    Ok((name, store))
}

fn equipment_of(tag: &Tag) -> Result<Uuid, UiError> {
    target_of(tag, TagKind::Equipment, "a tool")
}

async fn checkout(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Form(f): Form<ActionForm>,
) -> UiResult {
    let tag = resolve(&s, &raw).await?;
    let here = format!("/t/{}", tag.code);
    let id = equipment_of(&tag)?;
    let (name, store) = actor(&s, &headers, &tag, &f.name).await?;
    let due_on = match f.due_on.trim() {
        "" => None,
        d => Some(
            time::Date::parse(d, time::macros::format_description!("[year]-[month]-[day]"))
                .map_err(|_| UiError::invalid_at("back by: expected a date", &here))?,
        ),
    };
    store.check_out(id, NewLoan { borrower: name, due_on }).await.back_to(&here)?;
    Ok(see(&format!("{here}?done=out")))
}

async fn give_back(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Form(f): Form<ActionForm>,
) -> UiResult {
    let tag = resolve(&s, &raw).await?;
    let here = format!("/t/{}", tag.code);
    let id = equipment_of(&tag)?;
    let (_, store) = actor(&s, &headers, &tag, &f.name).await?;
    store.return_equipment(id, &f.note).await.back_to(&here)?;
    Ok(see(&format!("{here}?done=back")))
}

async fn problem(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Form(f): Form<ActionForm>,
) -> UiResult {
    let tag = resolve(&s, &raw).await?;
    let here = format!("/t/{}", tag.code);
    let (name, store) = actor(&s, &headers, &tag, &f.name).await?;
    match (tag.kind, tag.target_id) {
        (TagKind::Equipment, Some(id)) => {
            store.report_equipment_problem(id, &name, &f.problem).await.back_to(&here)?;
            Ok(see(&format!("{here}?done=problem")))
        }
        (kind @ (TagKind::Location | TagKind::Animal), Some(id)) => {
            store.report_concern(kind, id, &name, &f.problem).await.back_to(&here)?;
            Ok(see(&format!("{here}?done=noted")))
        }
        _ => Err(UiError::invalid_at("There's nothing to report on this tag.", &here)),
    }
}

/// A positive whole number from a form, within `max`.
fn count(s: &str, field: &str, max: i32, here: &str) -> Result<i32, UiError> {
    match s.trim() {
        "" => Ok(0),
        v => v
            .parse::<i32>()
            .ok()
            .filter(|n| (0..=max).contains(n))
            .ok_or_else(|| UiError::invalid_at(format!("{field}: a whole number up to {max}"), here)),
    }
}

async fn use_stock(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Form(f): Form<ActionForm>,
) -> UiResult {
    let tag = resolve(&s, &raw).await?;
    let here = format!("/t/{}", tag.code);
    let id = target_of(&tag, TagKind::InventoryItem, "a supply")?;
    let (_, store) = actor(&s, &headers, &tag, &f.name).await?;
    let quantity: rust_decimal::Decimal = f
        .quantity
        .trim()
        .parse()
        .ok()
        .filter(|q: &rust_decimal::Decimal| *q > rust_decimal::Decimal::ZERO && *q <= MAX_USE)
        .ok_or_else(|| UiError::invalid_at("how many: a number above zero", &here))?;
    let location_id = match f.place.trim() {
        "" => None,
        p => Some(p.parse().map_err(|_| UiError::invalid_at("unknown place", &here))?),
    };
    let movement = NewMovement { kind: MovementKind::Consumption, quantity, location_id, note: String::new() };
    store.record_movement(id, movement).await.back_to(&here)?;
    Ok(see(&format!("{here}?done=used")))
}

/// A typo guard on "used some": more than this at once goes through the app.
const MAX_USE: rust_decimal::Decimal = rust_decimal::Decimal::from_parts(1000, 0, 0, false, 0);

async fn eggs(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Form(f): Form<ActionForm>,
) -> UiResult {
    let tag = resolve(&s, &raw).await?;
    let here = format!("/t/{}", tag.code);
    let id = target_of(&tag, TagKind::Animal, "a flock")?;
    let (_, store) = actor(&s, &headers, &tag, &f.name).await?;
    let max = homestead_store::ops::MAX_EGGS_AT_ONCE;
    let collected = count(&f.collected, "eggs collected", max, &here)?;
    let damaged = count(&f.damaged, "cracked", max, &here)?;
    let today = store.today().await.back_to(&here)?;
    store.add_eggs(id, today, collected, damaged).await.back_to(&here)?;
    Ok(see(&format!("{here}?done=eggs")))
}

async fn compost(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Form(f): Form<ActionForm>,
) -> UiResult {
    let tag = resolve(&s, &raw).await?;
    let here = format!("/t/{}", tag.code);
    let id = target_of(&tag, TagKind::Location, "the compost's")?;
    let (name, store) = actor(&s, &headers, &tag, &f.name).await?;
    if store.get_location(id).await.back_to(&here)?.kind != LocationKind::Compost {
        return Err(UiError::invalid_at("This tag isn't on a compost pile.", &here));
    }
    let weight_lb = match f.weight_lb.trim() {
        "" => None,
        w => Some(
            w.parse::<rust_decimal::Decimal>()
                .ok()
                .filter(|w| *w > rust_decimal::Decimal::ZERO && *w <= MAX_USE)
                .ok_or_else(|| UiError::invalid_at("pounds: a number above zero", &here))?,
        ),
    };
    let notes = homestead_domain::tags::note("what it was (500 characters at most)", &f.note).back_to(&here)?;
    let received_on = store.today().await.back_to(&here)?;
    store
        .record_compost_dropoff(NewCompostDropoff { received_on, contributor: name, weight_lb, notes })
        .await
        .back_to(&here)?;
    Ok(see(&format!("{here}?done=compost")))
}

fn gate_of(tag: &Tag) -> Result<(), UiError> {
    match tag.kind {
        TagKind::Gate => Ok(()),
        _ => Err(UiError::invalid_at("This isn't the gate's tag.", &format!("/t/{}", tag.code))),
    }
}

async fn gate_in(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Form(f): Form<ActionForm>,
) -> UiResult {
    let tag = resolve(&s, &raw).await?;
    let here = format!("/t/{}", tag.code);
    gate_of(&tag)?;
    let (name, store) = actor(&s, &headers, &tag, &f.name).await?;
    let group = match f.group.trim() {
        "" => WorkingGroup::FarmLabor,
        g => g.parse::<WorkingGroup>().back_to(&here)?,
    };
    store.sign_in_at_gate(&name, group).await.back_to(&here)?;
    Ok(see(&format!("{here}?done=arrived")))
}

async fn gate_out(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Form(f): Form<ActionForm>,
) -> UiResult {
    let tag = resolve(&s, &raw).await?;
    let here = format!("/t/{}", tag.code);
    gate_of(&tag)?;
    let (name, store) = actor(&s, &headers, &tag, &f.name).await?;
    let (_, shift) = store.sign_out_at_gate(&name).await.back_to(&here)?;
    Ok(see(&format!("{here}?done={}", if shift.is_some() { "left" } else { "left-long" })))
}

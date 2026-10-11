//! Readings: any number taken off a place, planting, animal or machine,
//! with the trend of each. Anyone who reads the farm opens the page; each
//! reading shows only to people who may see what it measured, and taking
//! one needs the right to record that thing's work.

use axum::extract::Query;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Form, Router};
use homestead_domain::readings::{NewReading, Reading, ReadingSubject, Trend, parse_values, trends};
use homestead_store::Store;
use homestead_store::readings::{ReadingFilter, ReadingScope};
use maud::{Markup, html};
use rust_decimal::prelude::ToPrimitive;
use serde::Deserialize;
use time::PrimitiveDateTime;
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Can, Principal, cap};
use crate::ui::{Back, UiError, UiResult, layout, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new().route("/readings", get(page).post(record))
}

const B: &str = "/readings";

#[derive(Deserialize, Default)]
struct ListQuery {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    subject: String,
}

async fn page(c: Can<cap::GardenRead>, Query(q): Query<ListQuery>) -> UiResult {
    let subject = match q.subject.trim() {
        "" => None,
        s => Some(s.parse::<ReadingSubject>().back(B)?),
    };
    let kind = Some(q.kind.trim().to_lowercase()).filter(|k| !k.is_empty());
    let filter = ReadingFilter { kind: kind.clone(), subject, ..Default::default() };
    let list = c.store.readings(&filter, ReadingScope::read(c.who.caps), 300).await.back(B)?;
    let kinds = c.store.reading_kinds().await.back(B)?;
    let form = form(&c.who, &c.store, None, B).await.back(B)?;
    Ok(layout(
        "Readings",
        html! {
            p.muted { "Numbers taken off anything on the farm: a compost pile's temperature, a bed's soil moisture, "
                "what the flock ate. A new kind of reading needs no new page; type its name. Harvests, soil tests, "
                "eggs and watering keep their own pages." }
            @if !kinds.is_empty() {
                form.inline method="get" action="/readings" {
                    label { "Kind " select name="kind" {
                        option value="" { "All" }
                        @for k in &kinds { option value=(k) selected[kind.as_deref() == Some(k.as_str())] { (k) } }
                    } }
                    button.small { "Show" }
                }
            }
            (form)
            (history(&c.store, &list).await.back(B)?)
        },
    )
    .into_response())
}

/// A place's readings, counting what's inside it, and the form to add one.
pub(crate) async fn place_section(who: &Principal, store: &Store, location_id: Uuid) -> Result<Markup, UiError> {
    let back = format!("/locations/{location_id}");
    let filter = ReadingFilter { within: Some(location_id), ..Default::default() };
    let list = store.readings(&filter, ReadingScope::read(who.caps), 100).await.back_to(&back)?;
    let form = form(who, store, Some(ReadingSubject::Location(location_id)), &back).await.back_to(&back)?;
    Ok(html! {
        h2 { "Readings" }
        @if list.is_empty() { p.muted { "None taken here yet." } }
        (history(store, &list).await.back_to(&back)?)
        (form)
    })
}

/// Trends first (one line per quantity, with its shape over time), then
/// the most recent readings.
async fn history(store: &Store, list: &[Reading]) -> Result<Markup, homestead_store::StoreError> {
    let times = store.farm_times(&list.iter().map(|r| r.observed_at).collect::<Vec<_>>()).await?;
    let ts = trends(list);
    Ok(html! {
        @if !ts.is_empty() {
            table {
                tr { th { "Reading" } th { "Latest" } th { "Trend" } }
                @for t in &ts { tr {
                    td { (trend_name(t)) br; span.muted { (t.subject_name) } }
                    td { (amount(t.latest(), &t.unit)) br; span.muted {
                        @if t.points.len() > 1 { (t.min().normalize()) "–" (t.max().normalize()) " · " }
                        (t.points.len()) " reading" @if t.points.len() != 1 { "s" } } }
                    td { (sparkline(t)) }
                } }
            }
        }
        @if !list.is_empty() {
            details {
                summary { "Each reading" }
                table { @for (r, at) in list.iter().zip(&times).take(60) { tr {
                    td { (hm(at)) }
                    td { (r.kind) br; span.muted { (r.subject_name) } }
                    td { (r.summary()) @if !r.notes.is_empty() { br; span.muted { (r.notes) } } }
                } } }
            }
        }
    })
}

/// "compost temperature", or "compost check · moisture": the value's name
/// only when the kind doesn't already say it.
fn trend_name(t: &Trend) -> String {
    let name = t.name.to_lowercase();
    if name == "value" || t.kind.contains(&name) { t.kind.clone() } else { format!("{} · {}", t.kind, t.name) }
}

fn amount(v: rust_decimal::Decimal, unit: &str) -> String {
    if unit.is_empty() { v.normalize().to_string() } else { format!("{} {unit}", v.normalize()) }
}

fn hm(t: &PrimitiveDateTime) -> String {
    format!("{} {:02}:{:02}", t.date(), t.hour(), t.minute())
}

/// The last 30 values as a line, spaced by time, scaled to their range.
fn sparkline(t: &Trend) -> Markup {
    let pts = &t.points[t.points.len().saturating_sub(30)..];
    if pts.len() < 2 {
        return html! {};
    }
    let (w, h) = (96.0, 28.0);
    let t0 = pts[0].0.unix_timestamp() as f64;
    let span = (pts[pts.len() - 1].0.unix_timestamp() as f64 - t0).max(1.0);
    let vals: Vec<f64> = pts.iter().map(|p| p.1.to_f64().unwrap_or(0.0)).collect();
    let (lo, hi) = vals.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    let range = if hi > lo { hi - lo } else { 1.0 };
    let line = pts
        .iter()
        .zip(&vals)
        .map(|(p, v)| {
            let x = (p.0.unix_timestamp() as f64 - t0) / span * w;
            let y = h - 2.0 - (v - lo) / range * (h - 4.0);
            format!("{x:.1},{y:.1}")
        })
        .collect::<Vec<_>>()
        .join(" ");
    html! {
        svg.spark viewBox={"0 0 " (w) " " (h)} width="96" height="28" role="img"
            aria-label={(t.name) " from " (t.points[0].1.normalize()) " to " (t.latest().normalize()) " " (t.unit)} {
            polyline points=(line) {}
        }
    }
}

/// The form, offering only what the viewer may take readings of; nothing
/// when that's nothing. `fixed` pins the subject (a place's own page).
async fn form(
    who: &Principal,
    store: &Store,
    fixed: Option<ReadingSubject>,
    back: &str,
) -> Result<Markup, homestead_store::StoreError> {
    let scope = ReadingScope::write(who.caps);
    let fixed = fixed.filter(|s| who.can(s.write_cap()));
    if !scope.any() || (back != B && fixed.is_none()) {
        return Ok(html! {});
    }
    let subjects = if fixed.is_some() { Vec::new() } else { store.reading_subjects(scope).await? };
    if fixed.is_none() && subjects.is_empty() {
        return Ok(html! {});
    }
    let kinds = store.reading_kinds().await?;
    let group = |s: &ReadingSubject| match s {
        ReadingSubject::Location(_) => "Places",
        ReadingSubject::Planting(_) => "Plantings",
        ReadingSubject::Animal(_) => "Animals",
        ReadingSubject::Equipment(_) => "Equipment",
    };
    Ok(html! {
        details.card {
            summary { "Take a reading" }
            form method="post" action="/readings" style="display:grid;gap:.85rem" {
                input type="hidden" name="back" value=(back);
                @if let Some(s) = fixed { input type="hidden" name="subject" value=(s.key()); } @else {
                    label { "Of" select name="subject" required {
                        option value="" { "—" }
                        @for g in ["Places", "Plantings", "Animals", "Equipment"] {
                            @let here: Vec<_> = subjects.iter().filter(|(s, _)| group(s) == g).collect();
                            @if !here.is_empty() {
                                optgroup label=(g) { @for (s, name) in here { option value=(s.key()) { (name) } } }
                            }
                        }
                    } }
                }
                div.grid {
                    label { "Kind" input name="kind" required list="reading-kinds" placeholder="compost temperature"; }
                    label { "When" input name="observed_at" type="datetime-local"; }
                }
                datalist id="reading-kinds" { @for k in &kinds { option value=(k) {} } }
                label { "Values, one per line" textarea name="values" rows="3" required placeholder="temperature = 142 °F\nmoisture = 55 %" {} }
                p.muted { "A single number can be just " code { "142 °F" } ". Units are kept as typed and never converted, so use the same one each time." }
                label { "Notes" input name="notes"; }
                button { "Save reading" }
            }
        }
    })
}

#[derive(Deserialize)]
struct ReadingForm {
    #[serde(default)]
    subject: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    observed_at: String,
    #[serde(default)]
    values: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    back: String,
}

async fn record(c: Can<cap::GardenRead>, Form(f): Form<ReadingForm>) -> UiResult {
    // Only back to this app's own pages that show readings.
    let back =
        if f.back.starts_with("/locations/") && f.back[11..].parse::<Uuid>().is_ok() { f.back } else { B.into() };
    let subject = f.subject.parse::<ReadingSubject>().back_to(&back)?;
    if !c.who.can(subject.write_cap()) {
        return Ok(crate::auth::forbidden(&c.who, subject.write_cap()));
    }
    let observed_at = match f.observed_at.trim() {
        "" => time::OffsetDateTime::now_utc(),
        s => {
            let local = crate::ui::local_time_at(s, "when", &back)?;
            c.store.farm_time(local).await.back_to(&back)?
        }
    };
    let values = parse_values(&f.values).back_to(&back)?;
    c.store
        .record_reading(NewReading { observed_at, kind: f.kind, subject, values, notes: f.notes })
        .await
        .back_to(&back)?;
    Ok(see(&back))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use time::macros::datetime;

    #[test]
    fn a_sparkline_spans_its_box_and_puts_the_high_at_the_top() {
        let t = Trend {
            kind: "k".into(),
            subject_name: "s".into(),
            name: "t".into(),
            unit: "°F".into(),
            points: vec![
                (datetime!(2026-09-01 0:00 UTC), Decimal::from(120)),
                (datetime!(2026-09-02 0:00 UTC), Decimal::from(150)),
                (datetime!(2026-09-05 0:00 UTC), Decimal::from(141)),
            ],
        };
        let svg = sparkline(&t).into_string();
        assert!(svg.contains("points=\"0.0,26.0 24.0,2.0 96.0,"), "{svg}");
        let one = Trend { points: t.points[..1].to_vec(), ..t };
        assert_eq!(sparkline(&one).into_string(), "", "one reading has no shape");
    }
}

//! The farm map: every place with a shape, drawn on the plan, each a link
//! to its page. Drawn on the server as SVG, so it works with JavaScript off
//! and on a phone; a small script only helps pick points by tapping.

use axum::extract::Path;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Router};
use homestead_domain::auth::Capability;
use homestead_domain::map::{Pt, Shape, ShapeKind, parse_shape};
use homestead_domain::{Location, LocationKind};
use homestead_store::map::MapPlace;
use maud::{Markup, PreEscaped, html};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Can, Principal, cap};
use crate::ui::{Back, UiResult, layout, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new().route("/map", get(page)).route("/locations/{id}/shape", post(set_shape))
}

async fn page(c: Can<cap::GardenRead>) -> UiResult {
    const B: &str = "/garden";
    let places = c.store.farm_map().await.back(B)?;
    let watered =
        c.store.farm_times(&places.iter().filter_map(|p| p.last_watered).collect::<Vec<_>>()).await.back(B)?;
    let mut watered = watered.into_iter();
    let unmapped = c.store.list_locations().await.back(B)?.into_iter().filter(|l| l.active).count() - places.len();
    Ok(layout(
        "Farm map",
        html! {
            @if places.is_empty() {
                p.muted { "Nothing is on the map yet. Open a place from the " a href="/garden" { "garden" }
                    " page and give it a shape under “On the map”." }
            } @else {
                (plan(&places, None, None))
                p.muted { "Feet from the farm's corner, north up. Tap a place to open it." }
                table {
                    tr { th { "Place" } th { "Growing" } th { "Next harvest" } th { "Last watered" } th { "Open" } }
                    @for p in &places { tr {
                        td { a href={"/locations/" (p.id)} { (p.name) } br; span.muted { (p.kind) (measure(&p.shape)) } }
                        td { @if p.growing.is_empty() { span.muted { "—" } } @else { (p.growing.join(", ")) } }
                        td { @if let Some(d) = p.next_harvest { (crate::ui::Day(d)) } }
                        td { @if p.last_watered.is_some() { @if let Some(t) = watered.next() { (t.date()) } } }
                        td {
                            @if p.open_tasks > 0 { (p.open_tasks) " task" @if p.open_tasks != 1 { "s" } }
                            @if p.open_tasks > 0 && p.open_issues > 0 { br; }
                            @if p.open_issues > 0 { span.warn { (p.open_issues) " health issue" @if p.open_issues != 1 { "s" } } }
                        }
                    } }
                }
            }
            @if unmapped > 0 {
                p.muted { (unmapped) " place" @if unmapped != 1 { "s" } " in use " @if unmapped != 1 { "aren't" } @else { "isn't" }
                    " on the map yet." }
            }
        },
    )
    .into_response())
}

/// " · 48 sq ft" or " · 70 ft", from the shape itself.
fn measure(s: &Shape) -> String {
    match (s.area_sqft(), s.length_ft()) {
        (Some(a), _) => format!(" · {} sq ft", round(a)),
        (_, Some(l)) => format!(" · {} ft", round(l)),
        _ => String::new(),
    }
}

fn round(v: f64) -> String {
    if v >= 100.0 { format!("{v:.0}") } else { format!("{:.1}", v).trim_end_matches(".0").to_owned() }
}

/// The hover (and screen reader) text for a place on the plan.
fn summary(p: &MapPlace) -> String {
    let mut s = p.name.clone();
    if !p.growing.is_empty() {
        s += &format!(" — growing {}", p.growing.join(", "));
    }
    if let Some(d) = p.next_harvest {
        s += &format!("; harvest from {d}");
    }
    if p.open_tasks > 0 {
        s += &format!("; {} open task{}", p.open_tasks, if p.open_tasks == 1 { "" } else { "s" });
    }
    if p.open_issues > 0 {
        s += &format!("; {} health issue{}", p.open_issues, if p.open_issues == 1 { "" } else { "s" });
    }
    s
}

fn tone(k: LocationKind) -> &'static str {
    use LocationKind::*;
    match k {
        Garden | Field | Bed | Row | Orchard | Greenhouse => "grow",
        AnimalArea | Pen | Coop => "animal",
        Compost => "compost",
        Site | Stand | Storage | Other => "other",
    }
}

/// A "nice" grid step (1, 2 or 5 × 10ⁿ ft) giving about eight lines.
fn grid_step(extent: f64) -> f64 {
    let raw = extent / 8.0;
    let mag = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 5.0, 10.0].into_iter().map(|m| m * mag).find(|s| *s >= raw).unwrap_or(10.0 * mag)
}

fn n(v: f64) -> String {
    format!("{v:.2}").trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// SVG's y grows downward; the plan's grows north.
fn svg_points(pts: &[Pt]) -> String {
    pts.iter().map(|Pt(x, y)| format!("{},{}", n(*x), n(-y))).collect::<Vec<_>>().join(" ")
}

/// The plan as SVG. `here` is outlined and the rest dimmed; `focus` is the
/// area to show (the whole farm when `None`).
fn plan(places: &[MapPlace], here: Option<Uuid>, focus: Option<(f64, f64, f64, f64)>) -> Markup {
    let all =
        places.iter().map(|p| p.shape.bounds()).reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)));
    let (x0, y0, x1, y1) = focus.or(all).unwrap_or((0.0, 0.0, 100.0, 100.0));
    // Room around the edge, and never narrower than 20 ft so a lone point has a place to sit.
    let extent = (x1 - x0).max(y1 - y0).max(20.0);
    let pad = extent * 0.08;
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (w, h) = ((x1 - x0).max(20.0) + 2.0 * pad, (y1 - y0).max(20.0) + 2.0 * pad);
    let (left, top) = (cx - w / 2.0, -(cy + h / 2.0));
    let step = grid_step(extent);
    let font = extent / 38.0;
    let dot = extent / 90.0;
    let lines = |from: f64, span: f64| {
        let first = (from / step).ceil() as i64;
        (first..).map(move |i| i as f64 * step).take_while(move |v| *v <= from + span)
    };
    html! {
        svg.plan id="plan" viewBox={(n(left)) " " (n(top)) " " (n(w)) " " (n(h))} role="group" aria-label="Farm map" {
            g.grid {
                @for gx in lines(left, w) { line x1=(n(gx)) y1=(n(top)) x2=(n(gx)) y2=(n(top + h)) {} }
                @for gy in lines(top, h) { line x1=(n(left)) y1=(n(gy)) x2=(n(left + w)) y2=(n(gy)) {} }
            }
            @for p in places {
                a href={"/locations/" (p.id)} class={"shape " (tone(p.kind)) @if here == Some(p.id) { " here" } @else if here.is_some() { " dim" }} {
                    title { (summary(p)) }
                    @match p.shape.kind {
                        ShapeKind::Polygon => polygon points=(svg_points(&p.shape.points)) {},
                        ShapeKind::Line => polyline points=(svg_points(&p.shape.points)) {},
                        ShapeKind::Point => circle cx=(n(p.shape.points[0].0)) cy=(n(-p.shape.points[0].1)) r=(n(dot)) {},
                    }
                }
            }
            g.labels font-size=(n(font)) {
                @for p in places {
                    @if here.is_none() || here == Some(p.id) || p.depth == 0 || focus.is_some() {
                        @if let Some(l) = label(p, places, font, dot).filter(|l| {
                            // A focused view crops outer places; a corner name is only
                            // meaningful (and uncut) when its whole place is in view.
                            let (x0, y0, x1, y1) = p.shape.bounds();
                            let whole = x0 >= left && x1 <= left + w && -y1 >= top && -y0 <= top + h;
                            let (x, y) = (l.at.0, -l.at.1);
                            (l.anchor != "start" || whole) && x > left && x < left + w && y > top + font && y < top + h
                        }) {
                            @if l.upright {
                                text x=(n(l.at.0)) y=(n(-l.at.1)) text-anchor=(l.anchor)
                                    transform={"rotate(-90 " (n(l.at.0)) " " (n(-l.at.1)) ")"} { (p.name) }
                            } @else {
                                text x=(n(l.at.0)) y=(n(-l.at.1)) text-anchor=(l.anchor) { (p.name) }
                            }
                        }
                    }
                }
            }
            text.scale x=(n(left + pad * 0.3)) y=(n(top + h - pad * 0.3)) font-size=(n(font * 0.8)) { "grid " (n(step)) " ft" }
        }
    }
}

struct Label {
    at: Pt,
    anchor: &'static str,
    /// Read bottom to top, for a bed longer north–south than its name.
    upright: bool,
}

/// Where a place's name goes, if it fits. A place with mapped places inside
/// is named in its top-left corner so its name stays clear of theirs; an
/// outline too narrow for its name but tall enough is named upright.
fn label(p: &MapPlace, places: &[MapPlace], font: f64, dot: f64) -> Option<Label> {
    let (x0, y0, x1, y1) = p.shape.bounds();
    let text = font * 0.6 * p.name.chars().count() as f64;
    let mid = |c: Pt| Label { at: Pt(c.0, c.1 - font * 0.35), anchor: "middle", upright: false };
    match p.shape.kind {
        ShapeKind::Point => Some(Label { at: Pt(x0, y0 + dot * 1.6), anchor: "middle", upright: false }),
        ShapeKind::Line => Some(mid(p.shape.label_at())),
        ShapeKind::Polygon if places.iter().any(|c| c.parent_id == Some(p.id)) && x1 - x0 >= text + font => {
            Some(Label { at: Pt(x0 + font * 0.4, y1 - font * 1.2), anchor: "start", upright: false })
        }
        ShapeKind::Polygon if x1 - x0 >= text => Some(mid(p.shape.label_at())),
        ShapeKind::Polygon if y1 - y0 >= text && x1 - x0 >= font => {
            let c = p.shape.label_at();
            Some(Label { at: Pt(c.0 + font * 0.35, c.1), anchor: "middle", upright: true })
        }
        ShapeKind::Polygon => None,
    }
}

/// A place's part of the map: where it is, and for people who may, the
/// form to draw it.
pub async fn place_section(
    who: &Principal,
    store: &homestead_store::Store,
    loc: &Location,
) -> Result<Markup, homestead_store::StoreError> {
    let places = store.farm_map().await?;
    let shape = store.location_shape(loc.id).await?;
    let parent = loc.parent_id.and_then(|p| places.iter().find(|m| m.id == p)).map(|m| m.shape.bounds());
    let own = shape.as_ref().map(Shape::bounds);
    let focus = match (own, parent) {
        (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))),
        (a, b) => a.or(b),
    };
    let write = who.can(Capability::GardenWrite);
    let how = match shape.as_ref().map(|s| s.kind) {
        Some(ShapeKind::Line) => "line",
        Some(ShapeKind::Point) => "point",
        _ if shape.is_some() => "outline",
        _ => "rectangle",
    };
    Ok(html! {
        h2 { "On the map" }
        @if let Some(s) = &shape {
            @if !loc.active { p.muted { "It's out of use, so the farm map leaves it off." } }
            @else { (plan(&places, Some(loc.id), focus)) }
            p.muted {
                @match (s.area_sqft(), s.length_ft()) {
                    (Some(a), _) => { "The outline measures " (round(a)) " sq ft" @if let Some(set) = loc.area_sqft { " (recorded area: " (set.normalize()) ")" } ". " }
                    (_, Some(l)) => { "The line runs " (round(l)) " ft. " }
                    _ => {}
                }
                a href="/map" { "Whole farm" }
            }
        } @else {
            p.muted { "Not on the map." @if !places.is_empty() { " " a href="/map" { "See the farm map" } } }
        }
        @if write {
            details.card id="shape-edit" {
                summary { @if shape.is_some() { "Redraw on the map" } @else { "Put it on the map" } }
                form method="post" action={"/locations/" (loc.id) "/shape"} style="display:grid;gap:.85rem" {
                    p.muted { "Measure in feet east (x) and north (y) from one fixed corner of the farm, the same corner for every place. "
                        "A rectangle is two opposite corners, like " code { "10,20 14,32" } " for a 4 × 12 ft bed. "
                        @if !places.is_empty() && shape.is_none() { "Or open this and tap the map above to add points." } }
                    label { "Shape" select name="how" {
                        @for (v, label) in [("rectangle", "Rectangle (two corners)"), ("outline", "Outline"), ("line", "Line (fence, drip line)"), ("point", "Point (tap, hive)")] {
                            option value=(v) selected[v == how] { (label) }
                        }
                    } }
                    label { "Points (x,y …)" textarea name="points" rows="2" required { @if let Some(s) = &shape { (s.points_text()) } } }
                    button { "Save shape" }
                }
                @if shape.is_some() {
                    form method="post" action={"/locations/" (loc.id) "/shape"} {
                        input type="hidden" name="how" value="none";
                        button.small { "Take it off the map" }
                    }
                }
            }
            (PreEscaped(TAP_TO_ADD))
        }
    })
}

/// While the editor is open, a tap on the plan adds that spot (to the
/// half foot) to the points instead of following a link.
const TAP_TO_ADD: &str = r#"<script>
(() => {
  const svg = document.getElementById('plan'), box = document.getElementById('shape-edit');
  if (!svg || !box) return;
  svg.addEventListener('click', e => {
    if (!box.open) return;
    e.preventDefault();
    const p = new DOMPoint(e.clientX, e.clientY).matrixTransform(svg.getScreenCTM().inverse());
    const r = v => Math.round(v * 2) / 2;
    const t = box.querySelector('textarea[name=points]');
    t.value = (t.value.trim() + ' ' + r(p.x) + ',' + r(-p.y)).trim();
  });
})();
</script>"#;

#[derive(Deserialize)]
struct ShapeForm {
    how: String,
    #[serde(default)]
    points: String,
}

async fn set_shape(c: Can<cap::GardenWrite>, Path(id): Path<Uuid>, Form(f): Form<ShapeForm>) -> UiResult {
    let back = format!("/locations/{id}");
    let shape = match f.how.as_str() {
        "none" => None,
        how => Some(parse_shape(how, &f.points).back_to(&back)?),
    };
    c.store.set_location_shape(id, shape).await.back_to(&back)?;
    Ok(see(&back))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_steps_are_round_numbers_near_an_eighth_of_the_view() {
        assert_eq!(grid_step(100.0), 20.0);
        assert_eq!(grid_step(20.0), 5.0);
        assert_eq!(grid_step(400.0), 50.0);
        assert_eq!(grid_step(8.0), 1.0);
        for e in [13.0, 77.0, 999.0, 4321.0] {
            let s = grid_step(e);
            assert!(s >= e / 8.0 && s <= e / 8.0 * 2.5, "{e} → {s}");
        }
    }

    #[test]
    fn north_is_up() {
        assert_eq!(svg_points(&[Pt(1.0, 2.5), Pt(-3.0, -4.0)]), "1,-2.5 -3,4");
    }
}

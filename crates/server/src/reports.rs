//! The fiscal-year report: page, CSV and JSON. `reports.read` opens it;
//! each section inside also needs the capability to read what it sums.

use axum::extract::Query;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use homestead_domain::reports::{Count, FiscalYear, Measure, Report, ReportSection};
use maud::{Markup, html};
use serde::Deserialize;

use crate::AppState;
use crate::api::ApiResult;
use crate::auth::{Can, Principal, cap};
use crate::contacts::csv_cell;
use crate::ui::{Back, UiError, UiResult, layout};

pub fn ui_routes() -> Router<AppState> {
    Router::new().route("/report", get(page)).route("/report.csv", get(csv))
}

pub fn api_routes() -> Router<AppState> {
    Router::new().route("/report", get(api))
}

#[derive(Deserialize)]
struct YearQuery {
    /// The calendar year the fiscal year starts in; this one if absent.
    #[serde(default)]
    year: Option<String>,
}

const B: &str = "/report";

/// The report `who` may see for the requested year, as of the farm's today.
async fn build(c: &Can<cap::ReportsRead>, q: &YearQuery) -> Result<Report, UiError> {
    let today = c.store.today().await.back(B)?;
    let fy = match q.year.as_deref().map(str::trim).filter(|y| !y.is_empty()) {
        None => FiscalYear::containing(today),
        Some(y) => {
            let y = y.parse().map_err(|_| UiError::invalid("year: expected a year like 2026", B))?;
            FiscalYear::starting(y).back(B)?
        }
    };
    let through =
        fy.through(today).ok_or_else(|| UiError::invalid_at(format!("{} hasn't started yet", fy.label()), B))?;
    c.store.fiscal_report(fy, through, &sections(&c.who)).await.back(B)
}

fn sections(who: &Principal) -> Vec<ReportSection> {
    ReportSection::visible(who.caps)
}

async fn page(c: Can<cap::ReportsRead>, Query(q): Query<YearQuery>) -> UiResult {
    let r = build(&c, &q).await?;
    let fy = r.fiscal_year;
    let hidden: Vec<_> = ReportSection::ALL.iter().filter(|s| !sections(&c.who).contains(s)).collect();
    let this_year = FiscalYear::containing(c.store.today().await.back(B)?);
    Ok(layout(
        &format!("Report · {}", fy.label()),
        html! {
            p.muted {
                (r.from) " through " (r.through)
                @if r.through < fy.last_day() { " (the year is still running)" }
                ". Counted from the day-to-day records each time you open this page. Quantities in different units are never added together."
            }
            form.inline method="get" action="/report" {
                select name="year" {
                    @for y in (this_year.start_year - 4..=this_year.start_year).rev() {
                        @let f = FiscalYear { start_year: y };
                        option value=(y) selected[y == fy.start_year] { (f.label()) }
                    }
                }
                " " button.small { "Show" }
                " " a href={"/report.csv?year=" (fy.start_year)} { "Download CSV" }
            }
            @if let Some(v) = &r.volunteers {
                h2 { "Volunteers" }
                p { strong { (v.hours.normalize()) " hours" } " over " (v.shifts) " shifts by " (v.people) " people" }
                table { @for g in &v.hours_by_group { tr { td { (g.key) } td { (g.amount.normalize()) " h" } } } }
            }
            @if let Some(e) = &r.events {
                h2 { "Community events" }
                p { strong { (e.held) " held" } ", " (e.attendance) " attendees"
                    @if e.without_attendance > 0 { " (" (e.without_attendance) " events have no attendance recorded)" } }
                (count_table(&e.by_kind))
            }
            @if let Some(a) = &r.animals {
                h2 { "Animals" }
                p.muted { "Head counts. Assisted: in our care at some point this period (arrived, placed, or still with us). A lower bound." }
                div.grid {
                    div { h3 { "Assisted" } (count_table(&a.assisted)) }
                    div { h3 { "Arrived" } (count_table(&a.intake)) }
                    div { h3 { "Placed" } (count_table(&a.placements)) }
                }
                p { "Eggs: " (a.eggs_collected) " collected, " (a.eggs_damaged) " damaged" }
            }
            @if let Some(h) = &r.harvest {
                h2 { "Harvest" }
                (measure_table(&h.harvested))
                h3 { "Where it went" } (measure_table(&h.dispositions))
                h3 { "Produce donated" } (measure_table(&h.donated))
                h3 { "Losses" } (measure_table(&h.losses))
            }
            @if let Some(s) = &r.stand {
                h2 { "Farm stand" }
                p { strong { "$" (s.revenue.round_dp(2)) } " from " (s.sales) " sales" }
                (measure_table(&s.sold))
                p { "Compost drop-offs: " (s.compost_dropoffs) ", " (s.compost_lb.normalize()) " lb weighed" }
            }
            @if let Some(g) = &r.gifts {
                h2 { "Gifts recorded here" }
                p.muted { "Cash, checks, sponsorships and in-kind gifts entered in Homestead. Zeffy donations are totalled in Zeffy and not included." }
                table {
                    tr { td { "Cash and checks" } td { (g.cash_count) } td { "$" (g.cash.amount.round_dp(2)) } }
                    tr { td { "Sponsorships" } td { (g.sponsorship_count) } td { "$" (g.sponsorship.amount.round_dp(2)) } }
                    tr { td { "In-kind" } td { (g.in_kind_count) }
                         td { "$" (g.in_kind_estimated.round_dp(2)) " donor-estimated"
                              @if g.in_kind_unvalued > 0 { " (" (g.in_kind_unvalued) " without an estimate)" } } }
                }
            }
            @if let Some(i) = &r.inventory {
                h2 { "Inventory" }
                h3 { "Used" } (measure_table(&i.consumed))
                h3 { "Wasted" } (measure_table(&i.wasted))
                h3 { "Received from purchases" } (measure_table(&i.received))
                h3 { "Added from harvest" } (measure_table(&i.harvested_in))
                h3 { "Count corrections (net)" } (measure_table(&i.adjusted))
            }
            @if let Some(c) = &r.costs {
                h2 { "Spending" }
                p { strong { "$" (c.total.round_dp(2)) } " by program, or purchase category where no program is linked" }
                table { @for m in &c.by_program { tr { td { (m.key) } td { "$" (m.amount.round_dp(2)) } } } }
            }
            @if let Some(i) = &r.incidents {
                h2 { "Incidents" }
                p { (i.reported) " reported, " (i.still_open) " still open" }
                (count_table(&i.by_kind))
            }
            @if !hidden.is_empty() {
                p.muted { "Not shown, because your account can't read those records: "
                    @for (n, s) in hidden.iter().enumerate() { @if n > 0 { ", " } (s) } "." }
            }
        },
    )
    .into_response())
}

fn count_table(rows: &[Count]) -> Markup {
    html! {
        @if rows.is_empty() { p.muted { "None." } } @else {
            table { @for c in rows { tr { td { (c.key.replace('_', " ")) } td { (c.count) } } } }
        }
    }
}

fn measure_table(rows: &[Measure]) -> Markup {
    html! {
        @if rows.is_empty() { p.muted { "None." } } @else {
            table { @for m in rows { tr {
                @if !m.key.is_empty() { td { (m.key.replace('_', " ")) } }
                td { (m.quantity.normalize()) " " (m.unit) }
            } } }
        }
    }
}

async fn csv(c: Can<cap::ReportsRead>, Query(q): Query<YearQuery>) -> UiResult {
    let r = build(&c, &q).await?;
    let internal = |e: csv::Error| UiError::invalid(format!("CSV: {e}"), B);
    let mut w = csv::Writer::from_writer(vec![]);
    w.write_record(["section", "metric", "key", "unit", "value"]).map_err(internal)?;
    for [section, metric, key, unit, value] in r.rows() {
        // Keys and units can be typed by people (crop, item, program), so
        // they're neutralised; values are numbers we formatted ourselves,
        // and a negative adjustment must stay a number.
        w.write_record([section, metric, csv_cell(&key), csv_cell(&unit), value]).map_err(internal)?;
    }
    let bytes = w.into_inner().map_err(|e| UiError::invalid(format!("CSV: {e}"), B))?;
    let name = format!("homestead-report-fy{}.csv", r.fiscal_year.start_year);
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
        ],
        bytes,
    )
        .into_response())
}

async fn api(c: Can<cap::ReportsRead>, Query(q): Query<YearQuery>) -> ApiResult {
    match build(&c, &q).await {
        Ok(r) => Ok(Json(r).into_response()),
        Err(e) => Ok(e.into_json()),
    }
}

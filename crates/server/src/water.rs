//! Weather and water: what the sky did and what we gave the beds.

use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Form, Json, Router};
use homestead_domain::auth::Capability;
use homestead_domain::water::{
    IrrigationMethod, IrrigationProblem, MANUAL, NewIrrigation, NewWeather, celsius_to_fahrenheit,
    fahrenheit_to_celsius, gallons_to_litres, inches_to_mm, litres_to_gallons, mm_to_inches, mph_to_kph,
};
use maud::html;
use rust_decimal::Decimal;
use serde::Deserialize;
use time::PrimitiveDateTime;
use time::macros::format_description;
use uuid::Uuid;

use crate::AppState;
use crate::api::{ApiResult, created};
use crate::auth::{Can, cap};
use crate::ui::{Back, UiError, UiResult, decimal, layout, options, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/water", get(page))
        .route("/water/weather", axum::routing::post(add_weather))
        .route("/water/irrigation", axum::routing::post(add_irrigation))
}

pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/weather", get(api_weather).post(api_add_weather))
        .route("/irrigation", get(api_irrigation).post(api_add_irrigation))
}

const B: &str = "/water";

fn opt_decimal(s: &str, what: &str) -> Result<Option<Decimal>, UiError> {
    match s.trim() {
        "" => Ok(None),
        v => decimal(v, what, B).map(Some),
    }
}

fn local_time(s: &str, what: &str) -> Result<PrimitiveDateTime, UiError> {
    let s = s.trim();
    PrimitiveDateTime::parse(s, format_description!("[year]-[month]-[day]T[hour]:[minute]"))
        .or_else(|_| PrimitiveDateTime::parse(s, format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]")))
        .map_err(|_| UiError::invalid(format!("{what}: expected a date and time"), B))
}

fn hm(t: &PrimitiveDateTime) -> String {
    format!("{} {:02}:{:02}", t.date(), t.hour(), t.minute())
}

async fn page(c: Can<cap::GardenRead>) -> UiResult {
    let today = c.store.today().await.back(B)?;
    let weather = c.store.recent_weather(30).await.back(B)?;
    let runs = c.store.recent_irrigation(30).await.back(B)?;
    let wtimes = c.store.farm_times(&weather.iter().map(|w| w.observed_at).collect::<Vec<_>>()).await.back(B)?;
    let rtimes = c.store.farm_times(&runs.iter().map(|r| r.started_at).collect::<Vec<_>>()).await.back(B)?;
    // The rain season here runs October to September.
    let season = time::Date::from_calendar_date(
        if today.month() >= time::Month::October { today.year() } else { today.year() - 1 },
        time::Month::October,
        1,
    )
    .unwrap_or(today);
    let rain = c.store.rain_since(season).await.back(B)?;
    let water = c.store.water_by_location_since(today - time::Duration::days(30)).await.back(B)?;
    let locations = c.store.list_locations().await.back(B)?;
    let plantings = c.store.list_active_plantings().await.back(B)?;
    let can_write = c.who.can(Capability::GardenWrite);
    Ok(layout(
        "Weather & water",
        html! {
            div.grid {
                div.card {
                    strong { "Rain since " (season) }
                    @if rain.is_empty() { p.muted { "None recorded." } }
                    @for (source, mm) in &rain { p { (mm_to_inches(*mm)) " in " span.muted { "(" (source) ")" } } }
                }
                div.card {
                    strong { "Water, last 30 days" }
                    @if water.is_empty() { p.muted { "None recorded." } }
                    @for (loc, litres, runs, unmeasured) in &water {
                        p { (loc) ": " (litres_to_gallons(*litres)) " gal" span.muted { " · " (runs) " runs" @if *unmeasured > 0 { ", " (unmeasured) " not measured" } } }
                    }
                }
            }
            @if can_write {
                div.grid {
                    form.card method="post" action="/water/irrigation" {
                        strong { "Log watering" }
                        label { "Planting (or choose a place below)" select name="planting_id" {
                            option value="" { "—" }
                            @for (p, loc) in &plantings { option value=(p.id) { (p.crop) " — " (loc) } }
                        } }
                        label { "Place" select name="location_id" {
                            option value="" { "—" }
                            @for l in &locations { option value=(l.id) { (l.name) } }
                        } }
                        label { "Started" input name="started_at" type="datetime-local" required; }
                        div.grid {
                            label { "Minutes" input name="minutes" type="number" min="1" max="2880"; }
                            label { "Gallons (if measured)" input name="gallons" inputmode="decimal"; }
                        }
                        label { "How" select name="method" { (options(IrrigationMethod::ALL, Some(IrrigationMethod::Drip))) } }
                        label { "Notes" input name="notes"; }
                        button { "Log" }
                    }
                    form.card method="post" action="/water/weather" {
                        strong { "Record the weather" }
                        label { "When" input name="observed_at" type="datetime-local" required; }
                        div.grid {
                            label { "Temp (°F)" input name="temperature_f" inputmode="decimal"; }
                            label { "Rain (in)" input name="rain_in" inputmode="decimal"; }
                            label { "Wind (mph)" input name="wind_mph" inputmode="decimal"; }
                            label { "Humidity (%)" input name="humidity" inputmode="decimal"; }
                        }
                        label { "Conditions" input name="conditions" placeholder="clear, frost on the beds, Santa Anas…"; }
                        label { "Where (if it's a spot reading)" select name="location_id" {
                            option value="" { "—" }
                            @for l in &locations { option value=(l.id) { (l.name) } }
                        } }
                        button { "Record" }
                    }
                }
            }
            h2 { "Watering" }
            @if runs.is_empty() { p.muted { "Nothing logged yet." } } @else {
                table {
                    tr { th { "Started" } th { "Where" } th { "How" } th { "Minutes" } th { "Gallons" } th { "Source" } }
                    @for (r, t) in runs.iter().zip(&rtimes) {
                        tr {
                            td { (hm(t)) }
                            td { (r.location_name) @if let Some(crop) = &r.planting_crop { " · " (crop) } }
                            td { (r.method) } td { @if let Some(m) = r.minutes() { (m) } }
                            td { @if let Some(l) = r.water_liters { (litres_to_gallons(l)) } }
                            td.muted { (r.source) }
                        }
                    }
                }
            }
            h2 { "Weather" }
            @if weather.is_empty() { p.muted { "Nothing recorded yet." } } @else {
                table {
                    tr { th { "When" } th { "Temp" } th { "Rain" } th { "Wind" } th { "Humidity" } th { "Conditions" } th { "Source" } }
                    @for (w, t) in weather.iter().zip(&wtimes) {
                        tr {
                            td { (hm(t)) }
                            td { @if let Some(c) = w.temperature_c { (celsius_to_fahrenheit(c)) "°F" } }
                            td { @if let Some(p) = w.precipitation_mm { (mm_to_inches(p)) " in" } }
                            td { @if let Some(k) = w.wind_kph { (k) " km/h" } }
                            td { @if let Some(h) = w.humidity_pct { (h.normalize()) "%" } }
                            td { (w.conditions) @if let Some(l) = &w.location_name { span.muted { " · " (l) } } }
                            td.muted { (w.source) }
                        }
                    }
                }
            }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct WeatherForm {
    observed_at: String,
    #[serde(default)]
    temperature_f: String,
    #[serde(default)]
    rain_in: String,
    #[serde(default)]
    wind_mph: String,
    #[serde(default)]
    humidity: String,
    #[serde(default)]
    conditions: String,
    #[serde(default)]
    location_id: String,
    #[serde(default)]
    soil_f: String,
    frost: Option<String>,
}

fn opt_uuid(s: &str) -> Result<Option<Uuid>, UiError> {
    match s.trim() {
        "" => Ok(None),
        v => v.parse().map(Some).map_err(|_| UiError::invalid("unknown selection", B)),
    }
}

async fn add_weather(c: Can<cap::GardenWrite>, Form(f): Form<WeatherForm>) -> UiResult {
    let observed_at = c.store.farm_time(local_time(&f.observed_at, "when")?).await.back(B)?;
    let input = NewWeather {
        observed_at,
        source: MANUAL.into(),
        location_id: opt_uuid(&f.location_id)?,
        temperature_c: opt_decimal(&f.temperature_f, "temperature")?.map(fahrenheit_to_celsius),
        precipitation_mm: opt_decimal(&f.rain_in, "rain")?.map(inches_to_mm),
        wind_kph: opt_decimal(&f.wind_mph, "wind")?.map(mph_to_kph),
        humidity_pct: opt_decimal(&f.humidity, "humidity")?,
        conditions: f.conditions,
        raw: None,
        soil_temperature_c: opt_decimal(&f.soil_f, "soil")?.map(fahrenheit_to_celsius),
        frost: f.frost.is_some(),
    };
    c.store.record_weather(input).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct IrrigationForm {
    #[serde(default)]
    planting_id: String,
    #[serde(default)]
    location_id: String,
    started_at: String,
    #[serde(default)]
    minutes: String,
    #[serde(default)]
    gallons: String,
    method: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    zone_id: String,
    #[serde(default)]
    problem: String,
}

async fn add_irrigation(c: Can<cap::GardenWrite>, Form(f): Form<IrrigationForm>) -> UiResult {
    let started_at = c.store.farm_time(local_time(&f.started_at, "started")?).await.back(B)?;
    let ended_at = match f.minutes.trim() {
        "" => None,
        m => {
            let m: i64 = m.parse().map_err(|_| UiError::invalid("minutes: expected a whole number", B))?;
            Some(started_at + time::Duration::minutes(m))
        }
    };
    let input = NewIrrigation {
        location_id: opt_uuid(&f.location_id)?,
        planting_id: opt_uuid(&f.planting_id)?,
        started_at,
        ended_at,
        water_liters: opt_decimal(&f.gallons, "gallons")?.map(gallons_to_litres),
        method: f.method.parse::<IrrigationMethod>().back(B)?,
        notes: f.notes,
        source: MANUAL.into(),
        external_id: None,
        zone_id: opt_uuid(&f.zone_id)?,
        problem: match f.problem.trim() {
            "" => None,
            p => Some(p.parse::<IrrigationProblem>().back(B)?),
        },
    };
    c.store.record_irrigation(input).await.back(B)?;
    Ok(see(B))
}

async fn api_weather(c: Can<cap::GardenRead>) -> ApiResult {
    Ok(Json(c.store.recent_weather(500).await?).into_response())
}

async fn api_add_weather(c: Can<cap::GardenWrite>, Json(b): Json<NewWeather>) -> ApiResult {
    Ok(created(c.store.record_weather(b).await?))
}

async fn api_irrigation(c: Can<cap::GardenRead>) -> ApiResult {
    Ok(Json(c.store.recent_irrigation(500).await?).into_response())
}

async fn api_add_irrigation(c: Can<cap::GardenWrite>, Json(b): Json<NewIrrigation>) -> ApiResult {
    Ok(created(c.store.record_irrigation(b).await?))
}

//! A JSON file of the farm's places, flocks, stock, and tools.
//!
//! People, passwords, sessions, contacts, and incidents stay out of it.
//! Import creates a record whose name is new and leaves an existing name
//! alone, so running it twice does not duplicate the farm.

use axum::Form;
use axum::Router;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use homestead_domain::animals::{AnimalOrigin, AnimalStatus, NewAnimal, Sex, Species};
use homestead_domain::equipment::{Condition, NewEquipment};
use homestead_domain::{LocationKind, NewInventoryItem, NewLocation, StockAdjustment};
use maud::html;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::{Can, cap};
use crate::ui::{Back, UiError, UiResult, layout};

const B: &str = "/carry";

#[derive(Debug, Serialize, Deserialize)]
struct CarryFile {
    version: u32,
    #[serde(default)]
    locations: Vec<CarryPlace>,
    #[serde(default)]
    animals: Vec<CarryAnimal>,
    #[serde(default)]
    inventory: Vec<CarryItem>,
    #[serde(default)]
    equipment: Vec<CarryTool>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CarryPlace {
    name: String,
    kind: String,
    #[serde(default)]
    notes: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CarryAnimal {
    name: String,
    species: String,
    #[serde(default = "one")]
    head_count: i32,
}

#[derive(Debug, Serialize, Deserialize)]
struct CarryItem {
    name: String,
    unit: String,
    #[serde(default)]
    reorder_level: Decimal,
    #[serde(default)]
    on_hand: Decimal,
}

#[derive(Debug, Serialize, Deserialize)]
struct CarryTool {
    name: String,
    category: String,
}

fn one() -> i32 {
    1
}

pub fn ui_routes() -> Router<AppState> {
    Router::new().route("/carry", get(page).post(import)).route("/carry/export.json", get(export))
}

async fn snapshot(c: &Can<cap::SystemsAdmin>) -> Result<CarryFile, UiError> {
    let locations = c.store.list_locations().await.back(B)?;
    let animals = c.store.list_animals().await.back(B)?;
    let inventory = c.store.list_inventory().await.back(B)?;
    let equipment = c.store.list_equipment().await.back(B)?;
    Ok(CarryFile {
        version: 1,
        locations: locations
            .into_iter()
            .map(|l| CarryPlace { name: l.name, kind: l.kind.as_str().to_owned(), notes: l.notes })
            .collect(),
        animals: animals
            .into_iter()
            .map(|a| CarryAnimal { name: a.name, species: a.species.as_str().to_owned(), head_count: a.head_count })
            .collect(),
        inventory: inventory
            .into_iter()
            .map(|i| CarryItem { name: i.name, unit: i.unit, reorder_level: i.reorder_level, on_hand: i.on_hand })
            .collect(),
        equipment: equipment.into_iter().map(|e| CarryTool { name: e.name, category: e.category }).collect(),
    })
}

async fn page(c: Can<cap::SystemsAdmin>) -> UiResult {
    let file = snapshot(&c).await?;
    Ok(layout(
        "Carry",
        html! {
            p { "A file of places, flocks, stock, and tools. People, passwords, contacts, and incidents stay here." }
            p {
                a href="/carry/export.json" { "Download the file" }
                span.muted { " · " (file.locations.len()) " places, " (file.animals.len()) " flocks, " (file.inventory.len()) " items, " (file.equipment.len()) " tools" }
            }
            form.card method="post" action="/carry" {
                label { "Bring a file in" textarea name="document" rows="8" required placeholder="Paste the JSON file." {} }
                button type="submit" { "Import" }
                p.muted { "A name that already exists is left as it is. Running this twice does not duplicate the farm." }
            }
        },
    )
    .into_response())
}

async fn export(c: Can<cap::SystemsAdmin>) -> UiResult {
    let file = snapshot(&c).await?;
    let bytes = serde_json::to_vec_pretty(&file).map_err(|e| UiError::invalid(format!("file: {e}"), B))?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/json; charset=utf-8".to_owned()),
            (header::CONTENT_DISPOSITION, "attachment; filename=\"homestead-carry.json\"".to_owned()),
        ],
        bytes,
    )
        .into_response())
}

#[derive(Deserialize)]
struct ImportForm {
    document: String,
}

struct Counts {
    created: u32,
    skipped: u32,
}

async fn import(c: Can<cap::SystemsAdmin>, Form(f): Form<ImportForm>) -> UiResult {
    let file: CarryFile = serde_json::from_str(&f.document).map_err(|e| UiError::invalid(format!("file: {e}"), B))?;
    if file.version != 1 {
        return Err(UiError::invalid("this file is a newer version than this homestead can read", B));
    }
    let mut counts = Counts { created: 0, skipped: 0 };
    bring_places(&c, &file.locations, &mut counts).await?;
    bring_animals(&c, &file.animals, &mut counts).await?;
    bring_stock(&c, &file.inventory, &mut counts).await?;
    bring_tools(&c, &file.equipment, &mut counts).await?;
    Ok(layout(
        "Carry",
        html! {
            p { "Created " strong { (counts.created) } " and left " strong { (counts.skipped) } " that were already here." }
            p { a href="/carry" { "Back" } }
        },
    )
    .into_response())
}

async fn bring_places(c: &Can<cap::SystemsAdmin>, rows: &[CarryPlace], counts: &mut Counts) -> Result<(), UiError> {
    let mut have: Vec<String> = c.store.list_locations().await.back(B)?.into_iter().map(|l| l.name).collect();
    for row in rows {
        if have.iter().any(|n| n.eq_ignore_ascii_case(row.name.trim())) {
            counts.skipped += 1;
            continue;
        }
        let kind: LocationKind =
            row.kind.parse().map_err(|_| UiError::invalid(format!("unknown place kind {}", row.kind), B))?;
        let mut place = NewLocation::named(row.name.clone(), kind);
        place.notes = row.notes.clone();
        c.store.create_location(place).await.back(B)?;
        have.push(row.name.clone());
        counts.created += 1;
    }
    Ok(())
}

async fn bring_animals(c: &Can<cap::SystemsAdmin>, rows: &[CarryAnimal], counts: &mut Counts) -> Result<(), UiError> {
    let mut have: Vec<String> = c.store.list_animals().await.back(B)?.into_iter().map(|a| a.name).collect();
    let today = c.store.today().await.back(B)?;
    for row in rows {
        if have.iter().any(|n| n.eq_ignore_ascii_case(row.name.trim())) {
            counts.skipped += 1;
            continue;
        }
        let species: Species =
            row.species.parse().map_err(|_| UiError::invalid(format!("unknown species {}", row.species), B))?;
        c.store
            .create_animal(NewAnimal {
                species,
                name: row.name.clone(),
                head_count: row.head_count,
                sex: Sex::Unknown,
                status: AnimalStatus::InCare,
                origin: AnimalOrigin::Purchased,
                arrived_on: today,
                born_on: None,
                microchip: String::new(),
                notes: String::new(),
            })
            .await
            .back(B)?;
        have.push(row.name.clone());
        counts.created += 1;
    }
    Ok(())
}

async fn bring_stock(c: &Can<cap::SystemsAdmin>, rows: &[CarryItem], counts: &mut Counts) -> Result<(), UiError> {
    let mut have: Vec<String> = c.store.list_inventory().await.back(B)?.into_iter().map(|i| i.name).collect();
    for row in rows {
        if have.iter().any(|n| n.eq_ignore_ascii_case(row.name.trim())) {
            counts.skipped += 1;
            continue;
        }
        let item = c
            .store
            .create_inventory_item(NewInventoryItem {
                name: row.name.clone(),
                unit: row.unit.clone(),
                reorder_level: row.reorder_level,
            })
            .await
            .back(B)?;
        if row.on_hand > Decimal::ZERO {
            c.store
                .adjust_stock(item.id, StockAdjustment { delta: row.on_hand, reason: "carried in".into() })
                .await
                .back(B)?;
        }
        have.push(row.name.clone());
        counts.created += 1;
    }
    Ok(())
}

async fn bring_tools(c: &Can<cap::SystemsAdmin>, rows: &[CarryTool], counts: &mut Counts) -> Result<(), UiError> {
    let mut have: Vec<String> = c.store.list_equipment().await.back(B)?.into_iter().map(|e| e.name).collect();
    for row in rows {
        if have.iter().any(|n| n.eq_ignore_ascii_case(row.name.trim())) {
            counts.skipped += 1;
            continue;
        }
        c.store
            .create_equipment(NewEquipment {
                name: row.name.clone(),
                category: row.category.clone(),
                manufacturer: String::new(),
                model: String::new(),
                serial_number: String::new(),
                purchased_on: None,
                purchase_cost: None,
                purchase_id: None,
                location_id: None,
                condition: Condition::Good,
                notes: String::new(),
            })
            .await
            .back(B)?;
        have.push(row.name.clone());
        counts.created += 1;
    }
    Ok(())
}

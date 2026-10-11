//! Readings: recorded against one thing, found by place, kind and subject,
//! and shown only to people who may see what was measured.

use homestead_domain::animals::*;
use homestead_domain::auth::{Capability, CapabilitySet};
use homestead_domain::equipment::{Condition, NewEquipment};
use homestead_domain::readings::{NewReading, ReadingSubject, parse_values, trends};
use homestead_domain::*;
use homestead_store::Store;
use homestead_store::readings::{ReadingFilter, ReadingScope};
use rust_decimal::Decimal;
use time::macros::{date, datetime};

fn reading(kind: &str, subject: ReadingSubject, at: time::OffsetDateTime, values: &str) -> NewReading {
    NewReading {
        observed_at: at,
        kind: kind.into(),
        subject,
        values: parse_values(values).expect("values"),
        notes: String::new(),
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_new_measurement_needs_no_new_table(pool: sqlx::PgPool) {
    let store = Store::from_pool(pool);
    let yard = store.create_location(NewLocation::named("Compost yard", LocationKind::Compost)).await.expect("yard");
    let pile = store
        .create_location(NewLocation {
            parent_id: Some(yard.id),
            ..NewLocation::named("Pile 2", LocationKind::Compost)
        })
        .await
        .expect("pile");
    let pile2 = ReadingSubject::Location(pile.id);
    for (day, temp) in [(1, "120"), (3, "150"), (5, "141")] {
        let at = datetime!(2026-09-01 08:00 UTC) + time::Duration::days(day);
        store
            .record_reading(reading(
                "Compost Temperature",
                pile2,
                at,
                &format!("temperature = {temp} °F\nmoisture = 55 %"),
            ))
            .await
            .expect("reading");
    }
    let all = ReadingScope::read(CapabilitySet::all());
    let got = store.readings(&ReadingFilter::default(), all, 50).await.expect("list");
    assert_eq!(got.len(), 3);
    assert_eq!(got[0].subject_name, "Pile 2");
    assert_eq!(got[0].kind, "compost temperature", "stored in the form that groups");
    assert_eq!(got[0].summary(), "temperature 141 °F · moisture 55 %", "values keep their order");

    // The compost yard's page sees the pile inside it.
    let within = ReadingFilter { within: Some(yard.id), ..Default::default() };
    assert_eq!(store.readings(&within, all, 50).await.expect("within").len(), 3);
    let t = trends(&got);
    let temp = t.iter().find(|t| t.name == "temperature").expect("temperature trend");
    assert_eq!((temp.min(), temp.max(), temp.latest()), (Decimal::from(120), Decimal::from(150), Decimal::from(141)));
    assert_eq!(store.reading_kinds().await.expect("kinds"), ["compost temperature"]);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn readings_are_shown_only_to_people_who_may_see_their_subject(pool: sqlx::PgPool) {
    let store = Store::from_pool(pool);
    let bed = store.create_location(NewLocation::named("Bed A", LocationKind::Bed)).await.expect("bed");
    let tomato = store.create_planting(NewPlanting::at(bed.id, "Tomato", date!(2026 - 06 - 01))).await.expect("t");
    let hens = store
        .create_animal(NewAnimal {
            species: Species::Chicken,
            name: "Hens".into(),
            head_count: 12,
            sex: Sex::Female,
            status: AnimalStatus::Resident,
            origin: AnimalOrigin::Purchased,
            arrived_on: date!(2026 - 03 - 01),
            born_on: None,
            microchip: String::new(),
            notes: String::new(),
        })
        .await
        .expect("hens");
    let tiller = store
        .create_equipment(NewEquipment {
            name: "Tiller".into(),
            category: "Power tools".into(),
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
        .expect("tiller");
    let at = datetime!(2026-09-29 08:00 UTC);
    for (kind, s, v) in [
        ("canopy temperature", ReadingSubject::Planting(tomato.id), "88 °F"),
        ("feed eaten", ReadingSubject::Animal(hens.id), "3.2 lb"),
        ("engine hours", ReadingSubject::Equipment(tiller.id), "41.5 h"),
    ] {
        store.record_reading(reading(kind, s, at, v)).await.expect(kind);
    }
    let kinds = |scope| {
        let store = store.clone();
        async move {
            let mut k: Vec<String> = store
                .readings(&ReadingFilter::default(), scope, 50)
                .await
                .expect("list")
                .into_iter()
                .map(|r| r.kind)
                .collect();
            k.sort();
            k
        }
    };
    assert_eq!(kinds(ReadingScope::read(CapabilitySet::all())).await.len(), 3);
    let service = ReadingScope::read(CapabilitySet::of(&[Capability::GardenRead, Capability::EquipmentRead]));
    assert_eq!(kinds(service).await, ["canopy temperature", "engine hours"], "no animals.read, no flock");
    let volunteer = ReadingScope::read(CapabilitySet::of(&[Capability::GardenRead, Capability::AnimalsRead]));
    assert_eq!(kinds(volunteer).await, ["canopy temperature", "feed eaten"], "no equipment.read, no tiller");

    // A planting's reading is part of its bed's history, named for the crop.
    let on_bed = ReadingFilter { within: Some(bed.id), ..Default::default() };
    let r = store.readings(&on_bed, ReadingScope::read(CapabilitySet::all()), 50).await.expect("bed");
    assert_eq!(r.iter().map(|r| r.subject_name.as_str()).collect::<Vec<_>>(), ["Tomato in Bed A"]);

    // The form offers only what the person may take readings of.
    let crew_writes = ReadingScope::write(CapabilitySet::of(&[Capability::GardenWrite]));
    let subjects = store.reading_subjects(crew_writes).await.expect("subjects");
    assert!(subjects.iter().all(|(s, _)| matches!(s, ReadingSubject::Location(_) | ReadingSubject::Planting(_))));
    assert_eq!(subjects.len(), 2, "the bed and the tomato growing in it");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_database_holds_a_reading_to_one_subject(pool: sqlx::PgPool) {
    let store = Store::from_pool(pool.clone());
    let bed = store.create_location(NewLocation::named("Bed A", LocationKind::Bed)).await.expect("bed");
    let tomato = store.create_planting(NewPlanting::at(bed.id, "Tomato", date!(2026 - 06 - 01))).await.expect("t");
    let insert = |sql: &'static str| {
        let pool = pool.clone();
        async move { sqlx::query(sql).bind(bed.id).bind(tomato.id).execute(&pool).await }
    };
    let two =
        "insert into homestead.readings (observed_at, kind, location_id, planting_id) values (now(), 'x', $1, $2)";
    assert!(insert(two).await.is_err(), "two subjects");
    let none = "insert into homestead.readings (observed_at, kind) select now(), 'x' where $1 <> $2";
    assert!(insert(none).await.is_err(), "no subject");
    let upper = "insert into homestead.readings (observed_at, kind, location_id) select now(), 'Compost', $1 where $2 is not null";
    assert!(insert(upper).await.is_err(), "a kind that wouldn't group");
}

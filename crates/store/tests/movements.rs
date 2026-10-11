//! Movement history: every change of place is recorded, however it's made.

use homestead_domain::animals::*;
use homestead_domain::equipment::{Condition, EquipmentState, EquipmentStatus, NewEquipment};
use homestead_domain::movements::{Mover, NewMove, stays};
use homestead_domain::*;
use homestead_store::{Store, StoreError};
use time::macros::{date, datetime};

async fn place(store: &Store, name: &str) -> Location {
    store.create_location(NewLocation::named(name, LocationKind::AnimalArea)).await.expect(name)
}

fn goat() -> NewAnimal {
    NewAnimal {
        species: Species::Goat,
        name: "Clover".into(),
        head_count: 1,
        sex: Sex::Female,
        status: AnimalStatus::Resident,
        origin: AnimalOrigin::Purchased,
        arrived_on: date!(2026 - 07 - 01),
        born_on: None,
        microchip: String::new(),
        notes: String::new(),
    }
}

fn to(p: &Location, at: time::OffsetDateTime, note: &str) -> NewMove {
    NewMove { to: Some(p.id), at: Some(at), note: note.into() }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_goat_rotated_through_pastures_keeps_its_history(pool: sqlx::PgPool) {
    let store = Store::from_pool(pool);
    let (a, b, barn) =
        (place(&store, "Pasture A").await, place(&store, "Pasture B").await, place(&store, "Barn").await);
    let clover = store.create_animal(goat()).await.expect("goat");
    let me = Mover::Animal(clover.id);

    store.move_to(me, to(&a, datetime!(2026-08-01 9:00 UTC), "")).await.expect("A");
    store.move_to(me, to(&b, datetime!(2026-08-15 9:00 UTC), "A grazed down")).await.expect("B");
    // The old "Lives in" form, with no time given, is recorded too.
    store.set_animal_home(clover.id, Some(barn.id)).await.expect("barn");

    let moves = store.movements_of(me).await.expect("moves");
    let path: Vec<_> = moves.iter().map(|m| m.to.as_ref().map(|t| t.1.as_str())).collect();
    assert_eq!(path, [Some("Barn"), Some("Pasture B"), Some("Pasture A")]);
    assert_eq!(moves[1].from.as_ref().map(|f| f.1.as_str()), Some("Pasture A"));
    assert_eq!(moves[1].note, "A grazed down");
    assert_eq!(moves[0].note, "", "a later move doesn't inherit an earlier move's note");
    let s = stays(&moves);
    assert_eq!(s[2].days(time::OffsetDateTime::now_utc()), 14, "two weeks on Pasture A");
    assert_eq!(s[0].until, None, "still in the barn");
    assert_eq!(store.get_animal(clover.id).await.expect("a").home_location_id, Some(barn.id), "where it is now");

    // Moving it where it already is records nothing.
    store.move_to(me, NewMove { to: Some(barn.id), at: None, note: String::new() }).await.expect("same");
    assert_eq!(store.movements_of(me).await.expect("m").len(), 3);

    // A move dated before the last one would make "where it is now" wrong.
    let late = store.move_to(me, to(&a, datetime!(2026-08-20 9:00 UTC), "")).await;
    assert!(matches!(late, Err(StoreError::Conflict(ref m)) if m.contains("record moves in order")), "{late:?}");
    let future = time::OffsetDateTime::now_utc() + time::Duration::days(2);
    assert!(matches!(store.move_to(me, to(&a, future, "")).await, Err(StoreError::Conflict(_))));
    assert_eq!(
        store.get_animal(clover.id).await.expect("a").home_location_id,
        Some(barn.id),
        "refused moves change nothing"
    );
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn equipment_moves_are_recorded_from_every_form_and_shown_by_place(pool: sqlx::PgPool) {
    let store = Store::from_pool(pool);
    let (shed, garden) = (place(&store, "Tool shed").await, place(&store, "Garden").await);
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
            location_id: Some(shed.id),
            condition: Condition::Good,
            notes: String::new(),
        })
        .await
        .expect("tiller");
    let me = Mover::Equipment(tiller.id);
    assert_eq!(store.movements_of(me).await.expect("m").len(), 1, "arriving in the shed is the first move");

    store
        .set_equipment_state(
            tiller.id,
            EquipmentState {
                status: EquipmentStatus::InService,
                condition: Condition::Good,
                location_id: Some(garden.id),
            },
        )
        .await
        .expect("to the garden");
    // Out for repair: somewhere the farm doesn't track.
    store.move_to(me, NewMove { to: None, at: None, note: "Repair shop".into() }).await.expect("out");

    let moves = store.movements_of(me).await.expect("moves");
    assert_eq!(moves.len(), 3);
    assert_eq!((moves[0].to.clone(), moves[0].note.as_str()), (None, "Repair shop"));
    assert_eq!(moves[1].from.as_ref().map(|f| f.1.as_str()), Some("Tool shed"));

    let at_garden = store.movements_at(garden.id, true, true, 20).await.expect("garden");
    assert_eq!(at_garden.len(), 2, "arrived and left");
    assert!(at_garden.iter().all(|m| m.mover_name == "Tiller"));
    assert!(store.movements_at(garden.id, true, false, 20).await.expect("animals only").is_empty());
    assert!(matches!(
        store
            .move_to(Mover::Equipment(uuid::Uuid::new_v4()), NewMove { to: None, at: None, note: String::new() })
            .await,
        Err(StoreError::NotFound("equipment"))
    ));
}

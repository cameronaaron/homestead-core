//! Animals and Kiki's Cat Club against a real Postgres.

use homestead_domain::animals::*;
use homestead_store::{Store, StoreError};
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::macros::date;

fn cat(name: &str) -> NewAnimal {
    NewAnimal {
        species: Species::Cat,
        name: name.into(),
        head_count: 1,
        sex: Sex::Unknown,
        status: AnimalStatus::InCare,
        origin: AnimalOrigin::Trapped,
        arrived_on: date!(2026 - 09 - 01),
        born_on: None,
        microchip: String::new(),
        notes: String::new(),
    }
}

fn placement(kind: PlacementKind, person: &str) -> NewPlacement {
    NewPlacement {
        kind,
        person_name: person.into(),
        started_on: date!(2026 - 09 - 27),
        fee: (kind == PlacementKind::Adoption).then(|| Decimal::from(75)),
        notes: String::new(),
    }
}

async fn events(pool: &PgPool) -> Vec<(String, serde_json::Value)> {
    sqlx::query_as("select kind, payload from homestead.events order by id").fetch_all(pool).await.expect("events")
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn every_animal_enum_literal_is_accepted_by_the_database(pool: PgPool) {
    let store = Store::from_pool(pool);
    for s in Species::ALL {
        store
            .create_animal(NewAnimal { species: *s, ..cat(&format!("{s}")) })
            .await
            .unwrap_or_else(|e| panic!("{s}: {e}"));
    }
    for (i, (sex, origin)) in Sex::ALL.iter().cycle().zip(AnimalOrigin::ALL).enumerate() {
        store
            .create_animal(NewAnimal { sex: *sex, origin: *origin, ..cat(&format!("o{i}")) })
            .await
            .unwrap_or_else(|e| panic!("{origin}: {e}"));
    }
    let a = store.create_animal(cat("Status")).await.expect("cat");
    for st in AnimalStatus::ALL {
        store.set_animal_status(a.id, *st).await.unwrap_or_else(|e| panic!("{st}: {e}"));
    }
    for k in CareKind::ALL {
        let entry = NewCareEntry {
            occurred_on: date!(2026 - 09 - 02),
            kind: *k,
            details: "x".into(),
            provider: String::new(),
            cost: None,
        };
        store.log_care(a.id, entry).await.unwrap_or_else(|e| panic!("{k}: {e}"));
    }
    for k in PlacementKind::ALL {
        store.place_animal(a.id, placement(*k, "Someone")).await.unwrap_or_else(|e| panic!("{k}: {e}"));
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn adoption_moves_status_and_announces_only_the_cat(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let mochi = store.create_animal(cat("Mochi")).await.expect("cat");
    store.place_animal(mochi.id, placement(PlacementKind::Foster, "Ava")).await.expect("foster");
    assert_eq!(store.get_animal(mochi.id).await.expect("get").status, AnimalStatus::Fostered);
    assert!(events(&pool).await.is_empty(), "fostering is not announced");

    store.place_animal(mochi.id, placement(PlacementKind::Adoption, "Jordan Adopter")).await.expect("adopt");
    assert_eq!(store.get_animal(mochi.id).await.expect("get").status, AnimalStatus::Adopted);
    let ev = events(&pool).await;
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].0, "animal.adopted");
    assert!(!ev[0].1.to_string().contains("Jordan"), "adopter leaked into the event: {}", ev[0].1);
    assert_eq!(store.list_placements(mochi.id).await.expect("list").len(), 2);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn placing_a_missing_animal_writes_nothing(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let err = store.place_animal(uuid::Uuid::new_v4(), placement(PlacementKind::Adoption, "X")).await;
    assert!(matches!(err, Err(StoreError::NotFound("animal"))), "{err:?}");
    let n: i64 =
        sqlx::query_scalar("select count(*) from homestead.animal_placements").fetch_one(&pool).await.expect("n");
    assert_eq!(n, 0);
    assert!(events(&pool).await.is_empty());
    let care = store
        .log_care(
            uuid::Uuid::new_v4(),
            NewCareEntry {
                occurred_on: date!(2026 - 09 - 02),
                kind: CareKind::Vaccine,
                details: String::new(),
                provider: String::new(),
                cost: None,
            },
        )
        .await;
    assert!(matches!(care, Err(StoreError::NotFound("animal"))), "{care:?}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn cat_club_numbers_count_what_happened_since_the_date(pool: PgPool) {
    let store = Store::from_pool(pool);
    let neuter = |on| NewCareEntry {
        occurred_on: on,
        kind: CareKind::SpayNeuter,
        details: String::new(),
        provider: "County TNR".into(),
        cost: None,
    };
    // Two adopted (one before the window), one trapped-neutered-returned,
    // one fostered, one still with us, and a hen that must not count.
    let a = store.create_animal(cat("A")).await.expect("a");
    store
        .place_animal(
            a.id,
            NewPlacement { started_on: date!(2026 - 07 - 01), ..placement(PlacementKind::Adoption, "P") },
        )
        .await
        .expect("old adoption");
    let b = store.create_animal(cat("B")).await.expect("b");
    store.place_animal(b.id, placement(PlacementKind::Adoption, "Q")).await.expect("adoption");
    let c = store.create_animal(cat("C")).await.expect("c");
    store.log_care(c.id, neuter(date!(2026 - 09 - 05))).await.expect("neuter");
    store.place_animal(c.id, placement(PlacementKind::ReturnToColony, "")).await.expect("tnr");
    let d = store.create_animal(cat("D")).await.expect("d");
    store.place_animal(d.id, placement(PlacementKind::Foster, "Ava")).await.expect("foster");
    store.create_animal(cat("E")).await.expect("e");
    store.create_animal(NewAnimal { species: Species::Chicken, head_count: 26, ..cat("Flock") }).await.expect("flock");

    let n = store.cat_club_numbers(date!(2026 - 08 - 01)).await.expect("numbers");
    assert_eq!(
        n,
        CatClubNumbers { adoptions: 1, returned_to_colony: 1, spay_neuters: 1, in_our_care: 1, fostered_now: 1 }
    );
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_database_refuses_a_group_of_cats_even_past_validation(pool: PgPool) {
    let r = sqlx::query(
        "insert into homestead.animals (species, name, head_count, arrived_on) values ('cat', 'Litter', 4, '2026-09-01')",
    )
    .execute(&pool)
    .await;
    assert!(r.is_err(), "cats must be recorded one per row");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_chip_finds_exactly_one_animal_however_it_was_typed(pool: PgPool) {
    let store = Store::from_pool(pool);
    let mochi =
        store.create_animal(NewAnimal { microchip: "985 112 003 456 789".into(), ..cat("Mochi") }).await.expect("cat");
    assert_eq!(mochi.microchip, "985112003456789");
    let found = store.find_by_chip("985-112-003-456-789").await.expect("find").expect("Mochi");
    assert_eq!(found.id, mochi.id);
    assert!(store.find_by_chip("985112003456788").await.expect("find").is_none());
    assert!(store.find_by_chip("").await.expect("blank").is_none());
    assert!(matches!(store.find_by_chip("nope").await, Err(StoreError::Invalid(_))));

    // The same chip on a second animal is refused, on arrival or later.
    let twin = NewAnimal { microchip: "985112003456789".into(), ..cat("Twin") };
    assert!(matches!(store.create_animal(twin).await, Err(StoreError::Conflict(m)) if m.contains("microchip")));
    let tux = store.create_animal(cat("Tux")).await.expect("tux");
    let same = Identity {
        microchip: "985112003456789".into(),
        chip_registry: String::new(),
        chip_registered_to: ChipRegistrant::Us,
        band: String::new(),
    };
    assert!(matches!(store.set_identity(tux.id, same).await, Err(StoreError::Conflict(_))));
    let own = Identity {
        microchip: "avid*012*345*678".into(),
        chip_registry: "Found.org".into(),
        chip_registered_to: ChipRegistrant::Us,
        band: "red collar".into(),
    };
    let tux = store.set_identity(tux.id, own).await.expect("identity");
    assert_eq!((tux.microchip.as_str(), tux.band.as_str()), ("012345678", "red collar"));
    assert_eq!(tux.chip_registered_to, ChipRegistrant::Us);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn adopting_a_chipped_cat_leaves_a_task_to_transfer_the_chip(pool: PgPool) {
    let store = Store::from_pool(pool);
    let mochi =
        store.create_animal(NewAnimal { microchip: "985112003456789".into(), ..cat("Mochi") }).await.expect("cat");
    let bare = store.create_animal(cat("Bare")).await.expect("cat");
    store.place_animal(mochi.id, placement(PlacementKind::Adoption, "Randy")).await.expect("adopt");
    store.place_animal(bare.id, placement(PlacementKind::Adoption, "Ava")).await.expect("adopt");
    let tasks = store.list_open_tasks().await.expect("tasks");
    let titles: Vec<&str> = tasks.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["Transfer Mochi's microchip to Randy"], "only the chipped cat");
    let t = &tasks[0];
    assert!(t.details.contains("985112003456789") && t.details.contains("its registry"), "{}", t.details);
    assert_eq!(t.due_on, Some(date!(2026 - 10 - 04)), "a week after the adoption");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_course_of_medication_moves_on_with_each_dose_and_ends(pool: PgPool) {
    let store = Store::from_pool(pool);
    let mochi = store.create_animal(cat("Mochi")).await.expect("cat");
    let course = NewCareSchedule {
        kind: CareKind::Medication,
        details: "Clavamox 0.5 ml".into(),
        every_days: 1,
        first_due_on: date!(2026 - 10 - 01),
        ends_on: Some(date!(2026 - 10 - 03)),
    };
    let s = store.add_care_schedule(mochi.id, course).await.expect("schedule");
    assert_eq!((s.animal_name.as_str(), s.next_due_on), ("Mochi", date!(2026 - 10 - 01)));
    let dose = |on| NewCareEntry {
        occurred_on: on,
        kind: CareKind::Medication,
        details: String::new(),
        provider: String::new(),
        cost: None,
    };
    let next = |store: Store| async move {
        store.care_schedules(None, None).await.expect("list").first().map(|s| s.next_due_on)
    };

    // A late-typed record from before the course leaves it alone; so does other care.
    store.log_care(mochi.id, dose(date!(2026 - 09 - 20))).await.expect("old");
    store
        .log_care(mochi.id, NewCareEntry { kind: CareKind::Vaccine, ..dose(date!(2026 - 10 - 01)) })
        .await
        .expect("vaccine");
    assert_eq!(next(store.clone()).await, Some(date!(2026 - 10 - 01)));

    store.log_care(mochi.id, dose(date!(2026 - 10 - 01))).await.expect("dose 1");
    assert_eq!(next(store.clone()).await, Some(date!(2026 - 10 - 02)));
    let due = store.care_schedules(None, Some(date!(2026 - 10 - 01))).await.expect("due");
    assert!(due.is_empty(), "nothing due once today's dose is in");
    store.log_care(mochi.id, dose(date!(2026 - 10 - 02))).await.expect("dose 2");
    store.log_care(mochi.id, dose(date!(2026 - 10 - 03))).await.expect("dose 3");
    assert_eq!(next(store.clone()).await, None, "the last dose ends the course");
}

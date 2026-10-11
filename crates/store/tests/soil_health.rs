//! A place's soil history, and crop-health observations from seen to closed.

use homestead_domain::health::{NewObservation, ObservationCategory, Resolution, Severity};
use homestead_domain::soil::{AmendmentKind, Measurements, NewAmendment, NewSoilTest, SoilEntry};
use homestead_domain::*;
use homestead_store::Store;
use homestead_store::health::ObservationFilter;
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::macros::date;

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_beds_soil_history_includes_its_rows_in_date_order(pool: PgPool) {
    let store = Store::from_pool(pool);
    let bed = store.create_location(NewLocation::named("A3", LocationKind::Bed)).await.expect("bed");
    let row = store
        .create_location(NewLocation { parent_id: Some(bed.id), ..NewLocation::named("A3 row 1", LocationKind::Row) })
        .await
        .expect("row");
    let test = store
        .add_soil_test(NewSoilTest {
            location_id: bed.id,
            tested_on: date!(2026 - 03 - 01),
            ph: Some(Decimal::new(58, 1)),
            nitrogen: String::new(),
            phosphorus: "high".into(),
            potassium: String::new(),
            organic_matter: String::new(),
            lab: "UC Davis".into(),
            measurements: Measurements::from([("Calcium".to_owned(), "1,200 ppm".to_owned())]),
            notes: String::new(),
        })
        .await
        .expect("test");
    store
        .add_amendment(NewAmendment {
            location_id: row.id,
            applied_on: date!(2026 - 03 - 08),
            kind: AmendmentKind::Lime,
            material: String::new(),
            amount: Some(Decimal::from(5)),
            unit: "lb".into(),
            reason: "pH 5.8".into(),
            cost: Some(Decimal::new(1299, 2)),
            notes: String::new(),
        })
        .await
        .expect("amendment");
    let h = store.soil_history(bed.id).await.expect("history");
    assert_eq!(h.len(), 2);
    assert!(matches!(&h[0], (SoilEntry::Amendment(a), place) if a.material == "lime" && place == "A3 row 1"));
    assert!(matches!(&h[1], (SoilEntry::Test(t), place) if t.id == test.id && place == "A3"));
    let (back, _) = store.get_soil_test(test.id).await.expect("get");
    assert_eq!(back.measurements.get("Calcium").map(String::as_str), Some("1,200 ppm"), "kept as the lab wrote it");
    assert!(store.soil_history(row.id).await.expect("row").len() == 1, "a row doesn't see its bed's test");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn an_observation_follows_up_and_closing_it_ticks_the_task(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let bed = store.create_location(NewLocation::named("A3", LocationKind::Bed)).await.expect("bed");
    let kale = store.create_planting(NewPlanting::at(bed.id, "Kale", date!(2026 - 03 - 01))).await.expect("kale");
    let seen = store
        .record_observation(NewObservation {
            observed_on: date!(2026 - 06 - 01),
            location_id: None,
            planting_id: Some(kale.id),
            category: ObservationCategory::Pest,
            observation: "Aphids under the leaves".into(),
            severity: Severity::High,
            affected: "6 plants".into(),
            action_taken: "Sprayed water".into(),
            follow_up_on: Some(date!(2026 - 06 - 04)),
        })
        .await
        .expect("seen");
    assert_eq!((seen.location_id, seen.crop.as_deref()), (Some(bed.id), Some("Kale")), "place from the planting");
    let task = store.list_open_tasks().await.expect("tasks").into_iter().find(|t| t.title.starts_with("Follow up"));
    let task = task.expect("a follow-up task");
    assert_eq!((task.due_on, task.priority), (Some(date!(2026 - 06 - 04)), TaskPriority::High));

    let open = ObservationFilter { crop: Some("kale".into()), open_only: true, ..Default::default() };
    assert_eq!(store.observations(&open, 10).await.expect("open").len(), 1);
    let closed = store
        .resolve_observation(seen.id, Resolution { resolved_on: date!(2026 - 06 - 10), outcome: "Gone".into() })
        .await
        .expect("closed");
    assert_eq!(closed.resolved_on, Some(date!(2026 - 06 - 10)));
    assert_eq!(store.get_task(task.id).await.expect("task").status, TaskStatus::Done);
    assert!(store.observations(&open, 10).await.expect("open").is_empty());
    let tally = store.health_tally(date!(2026 - 01 - 01)).await.expect("tally");
    assert_eq!((tally[0].subject.as_str(), tally[0].seen, tally[0].high), ("Kale", 1, 1));
    let early = Resolution { resolved_on: date!(2026 - 05 - 01), outcome: "x".into() };
    let again = store.resolve_observation(seen.id, early).await.expect("already closed: no change");
    assert_eq!(again.resolved_on, Some(date!(2026 - 06 - 10)));
}

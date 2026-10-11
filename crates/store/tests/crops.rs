//! Crop plans make planned plantings and ordinary tasks; ticking the tasks
//! records the planting's real dates.

use homestead_domain::crops::{Due, NewCropPlan, Succession, succession_dates};
use homestead_domain::*;
use homestead_store::Store;
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::Duration;
use time::macros::date;

async fn events(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("select kind from homestead.events order by id").fetch_all(pool).await.expect("events")
}

fn lettuce(bed: uuid::Uuid, first: time::Date, count: u8) -> NewCropPlan {
    NewCropPlan {
        crop: "Lettuce".into(),
        variety: "Jericho".into(),
        family: "aster".into(),
        notes: String::new(),
        method: SowMethod::Direct,
        days_to_transplant: None,
        days_to_harvest: Some(45),
        harvest_window_days: Some(21),
        seed_lot_id: None,
        expected_yield: Some(Decimal::from(8)),
        expected_yield_unit: Some(HarvestUnit::Lb),
        successions: succession_dates(first, 14, count)
            .into_iter()
            .map(|sow_on| Succession { location_id: bed, sow_on })
            .collect(),
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_succession_plan_makes_planned_plantings_and_their_tasks(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let bed = store.create_location(NewLocation::named("A3", LocationKind::Bed)).await.expect("bed");
    let plan = store.create_crop_plan(lettuce(bed.id, date!(2026 - 03 - 01), 4)).await.expect("plan");
    let (_, successions) = store.get_crop_plan(plan.id).await.expect("get");
    let sown: Vec<_> = successions.iter().map(|s| s.planting.sown_on).collect();
    assert_eq!(sown, [date!(2026 - 03 - 01), date!(2026 - 03 - 15), date!(2026 - 03 - 29), date!(2026 - 04 - 12)]);
    let first = &successions[0].planting;
    assert_eq!(first.status, PlantingStatus::Planned);
    assert_eq!(
        (first.expected_harvest_on, first.expected_harvest_until),
        (Some(date!(2026 - 04 - 15)), Some(date!(2026 - 05 - 06)))
    );
    assert_eq!((first.family.as_str(), first.crop_plan_id), ("aster", Some(plan.id)));
    let tasks = store.list_open_tasks().await.expect("tasks");
    assert_eq!(tasks.iter().filter(|t| t.title == "Sow Lettuce").count(), 4);
    assert!(events(&pool).await.is_empty(), "a plan announces nothing: nothing is in the ground yet");
    let plans = store.list_crop_plans().await.expect("list");
    assert_eq!((plans[0].successions, plans[0].planned), (4, 4));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn ticking_a_late_sowing_moves_the_plantings_dates(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let today = store.today().await.expect("today");
    let bed = store.create_location(NewLocation::named("A3", LocationKind::Bed)).await.expect("bed");
    let planned_for = today - Duration::days(3);
    let plan =
        NewCropPlan { method: SowMethod::Transplant, days_to_transplant: Some(28), ..lettuce(bed.id, planned_for, 1) };
    let plan = store.create_crop_plan(plan).await.expect("plan");
    let agenda = store.crop_agenda().await.expect("agenda");
    assert_eq!(agenda.plantings.iter().map(|(_, _, d)| *d).collect::<Vec<_>>(), [Due::SowOverdue]);

    let tasks = store.list_open_tasks().await.expect("tasks");
    let sow = tasks.iter().find(|t| t.title == "Start seed for Lettuce").expect("sow task");
    let transplant = tasks.iter().find(|t| t.title == "Transplant Lettuce").expect("transplant task");
    assert_eq!(transplant.due_on, Some(planned_for + Duration::days(28)));
    store.complete_task(sow.id).await.expect("sown");
    store.complete_task(sow.id).await.expect("a second tick is a no-op");

    let p = &store.get_crop_plan(plan.id).await.expect("get").1[0].planting;
    assert_eq!((p.status, p.sown_on), (PlantingStatus::Growing, today), "sown today, three days late");
    assert_eq!(p.transplant_on, Some(today + Duration::days(28)));
    assert_eq!(p.expected_harvest_on, Some(today + Duration::days(45)));
    assert_eq!(store.get_task(transplant.id).await.expect("t").due_on, Some(today + Duration::days(28)));
    assert_eq!(events(&pool).await, ["task.completed", "planting.created"], "announced once, when it went in");

    store.complete_task(transplant.id).await.expect("transplanted early");
    let p = &store.get_crop_plan(plan.id).await.expect("get").1[0].planting;
    assert_eq!(p.transplant_on, Some(today), "the day it actually went out");
    assert_eq!(events(&pool).await.iter().filter(|k| *k == "planting.created").count(), 1);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn yields_compare_with_the_plan_and_harvests_come_due(pool: PgPool) {
    let store = Store::from_pool(pool);
    let today = store.today().await.expect("today");
    let bed = store.create_location(NewLocation::named("A3", LocationKind::Bed)).await.expect("bed");
    // Sown 40 days ago: harvest expected in 5 days.
    let plan = store.create_crop_plan(lettuce(bed.id, today - Duration::days(40), 1)).await.expect("plan");
    let planting = store.get_crop_plan(plan.id).await.expect("get").1[0].planting.id;
    store.set_planting_status(planting, PlantingStatus::Growing).await.expect("growing");
    let agenda = store.crop_agenda().await.expect("agenda");
    assert_eq!(agenda.plantings.iter().map(|(_, _, d)| *d).collect::<Vec<_>>(), [Due::HarvestSoon]);

    for (q, unit) in [(3, HarvestUnit::Lb), (4, HarvestUnit::Lb), (2, HarvestUnit::Bunch)] {
        store
            .log_harvest(NewHarvest {
                planting_id: planting,
                harvested_on: today,
                quantity: Decimal::from(q),
                unit,
                notes: String::new(),
            })
            .await
            .expect("harvest");
    }
    let y = &store.get_crop_plan(plan.id).await.expect("get").1[0].yields;
    assert_eq!(y.actual, Some((Decimal::from(7), HarvestUnit::Lb)));
    assert_eq!(y.other, [(Decimal::from(2), HarvestUnit::Bunch)]);
    assert_eq!(y.percent(), Some(Decimal::from(88)));
    assert!(store.crop_agenda().await.expect("agenda").plantings.is_empty(), "being harvested now");
}

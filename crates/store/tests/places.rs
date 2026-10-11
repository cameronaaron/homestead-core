//! A place's history covers what's inside it, and plantings remember when
//! they left the ground.

use homestead_domain::*;
use homestead_store::Store;
use rust_decimal::Decimal;
use time::macros::date;

async fn bed_in_garden(store: &Store) -> (Location, Location, Location) {
    let garden = store.create_location(NewLocation::named("Kitchen garden", LocationKind::Garden)).await.expect("g");
    let bed = store
        .create_location(NewLocation { parent_id: Some(garden.id), ..NewLocation::named("A3", LocationKind::Bed) })
        .await
        .expect("bed");
    let row = store
        .create_location(NewLocation { parent_id: Some(bed.id), ..NewLocation::named("A3 row 1", LocationKind::Row) })
        .await
        .expect("row");
    (garden, bed, row)
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_places_history_includes_what_grew_inside_it(pool: sqlx::PgPool) {
    let store = Store::from_pool(pool);
    let (garden, bed, row) = bed_in_garden(&store).await;
    let tomato = store
        .create_planting(NewPlanting {
            family: "nightshade".into(),
            ..NewPlanting::at(row.id, "Tomato", date!(2026 - 04 - 01))
        })
        .await
        .expect("tomato");
    store
        .log_harvest(NewHarvest {
            planting_id: tomato.id,
            harvested_on: date!(2026 - 07 - 04),
            quantity: Decimal::from(42),
            unit: HarvestUnit::Lb,
            notes: String::new(),
        })
        .await
        .expect("harvest");

    let h = store.location_history(bed.id).await.expect("history");
    assert_eq!(h.ancestors.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["Kitchen garden"]);
    assert_eq!(h.children.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["A3 row 1"]);
    let (p, place) = &h.plantings[0];
    assert_eq!((p.crop.as_str(), p.family.as_str(), place.as_str()), ("Tomato", "nightshade", "A3 row 1"));
    assert_eq!(h.harvests[0].quantity, Decimal::from(42));
    assert_eq!(store.location_history(garden.id).await.expect("g").plantings.len(), 1, "a garden sees its beds");
    let row_history = store.location_history(row.id).await.expect("row");
    assert_eq!(row_history.ancestors.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["Kitchen garden", "A3"]);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn finishing_a_planting_records_its_end_and_reviving_it_clears_it(pool: sqlx::PgPool) {
    let store = Store::from_pool(pool);
    let (_, bed, _) = bed_in_garden(&store).await;
    let p = store.create_planting(NewPlanting::at(bed.id, "Lettuce", date!(2026 - 03 - 01))).await.expect("p");
    store.set_planting_status(p.id, PlantingStatus::Finished).await.expect("finish");
    let (done, _) = store.get_planting(p.id).await.expect("get");
    assert_eq!(done.ended_on, Some(store.today().await.expect("today")));
    store.set_planting_status(p.id, PlantingStatus::Growing).await.expect("revive");
    assert_eq!(store.get_planting(p.id).await.expect("get").0.ended_on, None);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_place_can_be_retired_but_not_put_inside_itself(pool: sqlx::PgPool) {
    let store = Store::from_pool(pool);
    let (garden, bed, row) = bed_in_garden(&store).await;
    let retired = store
        .update_location(
            bed.id,
            NewLocation {
                parent_id: Some(garden.id),
                active: false,
                area_sqft: Some(Decimal::from(32)),
                ..NewLocation::named("A3", LocationKind::Bed)
            },
        )
        .await
        .expect("retire");
    assert!(!retired.active);
    assert_eq!(retired.area_sqft, Some(Decimal::from(32)));
    let looped = store
        .update_location(
            garden.id,
            NewLocation { parent_id: Some(row.id), ..NewLocation::named("G", LocationKind::Garden) },
        )
        .await
        .expect_err("a loop");
    assert!(looped.to_string().contains("loop"), "{looped}");
}

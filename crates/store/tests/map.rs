//! The farm map: places' shapes, and what the map says about each.

use homestead_domain::map::{Pt, ShapeKind, parse_shape};
use homestead_domain::*;
use homestead_store::Store;
use time::macros::date;

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_shape_is_saved_replaced_and_removed(pool: sqlx::PgPool) {
    let store = Store::from_pool(pool);
    let bed = store.create_location(NewLocation::named("A3", LocationKind::Bed)).await.expect("bed");
    assert_eq!(store.location_shape(bed.id).await.expect("none"), None);

    let first = parse_shape("rectangle", "0,0 4,12").expect("rect");
    store.set_location_shape(bed.id, Some(first.clone())).await.expect("set");
    assert_eq!(store.location_shape(bed.id).await.expect("get"), Some(first));

    let moved = parse_shape("rectangle", "10,0 14,12").expect("moved");
    store.set_location_shape(bed.id, Some(moved.clone())).await.expect("replace");
    assert_eq!(store.location_shape(bed.id).await.expect("get").expect("some").points[0], Pt(10.0, 0.0));

    store.set_location_shape(bed.id, None).await.expect("remove");
    assert_eq!(store.location_shape(bed.id).await.expect("gone"), None);
    assert!(store.set_location_shape(uuid::Uuid::new_v4(), Some(moved)).await.is_err(), "no such place");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_map_sums_up_each_place_with_what_is_inside_it(pool: sqlx::PgPool) {
    let store = Store::from_pool(pool);
    let today = store.today().await.expect("today");
    let garden = store.create_location(NewLocation::named("Garden", LocationKind::Garden)).await.expect("g");
    let bed = store
        .create_location(NewLocation { parent_id: Some(garden.id), ..NewLocation::named("A3", LocationKind::Bed) })
        .await
        .expect("bed");
    // A row with no shape of its own still counts toward the bed and the garden.
    let row = store
        .create_location(NewLocation { parent_id: Some(bed.id), ..NewLocation::named("A3 r1", LocationKind::Row) })
        .await
        .expect("row");
    let shed = store.create_location(NewLocation::named("Shed", LocationKind::Storage)).await.expect("shed");
    store.set_location_shape(garden.id, Some(parse_shape("rectangle", "0,0 60,40").expect("g"))).await.expect("g");
    store.set_location_shape(bed.id, Some(parse_shape("rectangle", "2,2 6,14").expect("b"))).await.expect("b");
    store.set_location_shape(shed.id, Some(parse_shape("point", "70,5").expect("s"))).await.expect("s");

    let soon = today + time::Duration::days(20);
    store
        .create_planting(NewPlanting { expected_harvest_on: Some(soon), ..NewPlanting::at(row.id, "Tomato", today) })
        .await
        .expect("tomato");
    store.create_planting(NewPlanting::at(bed.id, "Basil", today)).await.expect("basil");
    let done = store.create_planting(NewPlanting::at(bed.id, "Pea", date!(2026 - 02 - 01))).await.expect("pea");
    store.set_planting_status(done.id, PlantingStatus::Finished).await.expect("finish");
    store.create_task(NewTask { location_id: Some(row.id), ..NewTask::titled("Stake tomatoes") }).await.expect("t");

    let map = store.farm_map().await.expect("map");
    let names: Vec<_> = map.iter().map(|p| (p.name.as_str(), p.depth)).collect();
    assert_eq!(names, [("Garden", 0), ("Shed", 0), ("A3", 1)], "outer places first, so they're drawn under");
    let a3 = map.iter().find(|p| p.id == bed.id).expect("a3");
    assert_eq!(a3.growing, ["Basil", "Tomato"], "the row's tomato counts; the finished pea doesn't");
    assert_eq!((a3.next_harvest, a3.open_tasks, a3.open_issues), (Some(soon), 1, 0));
    assert_eq!(a3.shape.kind, ShapeKind::Polygon);
    let g = map.iter().find(|p| p.id == garden.id).expect("garden");
    assert_eq!((g.growing.len(), g.open_tasks), (2, 1), "a garden sees its beds' rows");
    let s = map.iter().find(|p| p.id == shed.id).expect("shed");
    assert!(s.growing.is_empty() && s.next_harvest.is_none() && s.last_watered.is_none());

    // A place taken out of use leaves the map but keeps its shape.
    store
        .update_location(shed.id, NewLocation { active: false, ..NewLocation::named("Shed", LocationKind::Storage) })
        .await
        .expect("retire");
    assert!(store.farm_map().await.expect("map").iter().all(|p| p.id != shed.id));
    assert!(store.location_shape(shed.id).await.expect("kept").is_some());
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_database_refuses_a_shape_the_domain_would(pool: sqlx::PgPool) {
    let store = Store::from_pool(pool.clone());
    let bed = store.create_location(NewLocation::named("A3", LocationKind::Bed)).await.expect("bed");
    for (kind, points) in [("circle", "[[0,0]]"), ("point", "[]"), ("point", "{\"x\":1}")] {
        let r =
            sqlx::query("insert into homestead.location_shapes (location_id, kind, points) values ($1, $2, $3::jsonb)")
                .bind(bed.id)
                .bind(kind)
                .bind(points)
                .execute(&pool)
                .await;
        assert!(r.is_err(), "{kind} {points} got in");
    }
}

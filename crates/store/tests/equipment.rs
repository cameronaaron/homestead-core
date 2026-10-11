//! Equipment, its service history, and the reminder it leaves.

use homestead_domain::equipment::*;
use homestead_store::{Store, StoreError};
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::macros::date;

fn tiller(serial: &str) -> NewEquipment {
    NewEquipment {
        name: "Tiller".into(),
        category: "Power tools".into(),
        manufacturer: "Honda".into(),
        model: "FG110".into(),
        serial_number: serial.into(),
        purchased_on: Some(date!(2024 - 04 - 01)),
        purchase_cost: Some(Decimal::from(499)),
        purchase_id: None,
        location_id: None,
        condition: Condition::Good,
        notes: String::new(),
    }
}

fn service(on: time::Date, next: Option<time::Date>, remind: bool) -> NewMaintenance {
    NewMaintenance {
        performed_on: on,
        work: "Oil change".into(),
        cost: Some(Decimal::from(20)),
        performed_by: "Sam".into(),
        vendor_id: None,
        next_service_on: next,
        notes: String::new(),
        remind,
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_next_service_comes_from_the_latest_record_and_becomes_a_task(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let t = store.create_equipment(tiller("GCAAH-1001")).await.expect("tiller");
    assert!(matches!(store.create_equipment(tiller("GCAAH-1001")).await, Err(StoreError::Conflict(_))), "same serial");
    store.create_equipment(tiller("")).await.expect("unknown serials don't collide");

    store
        .add_maintenance(t.id, service(date!(2026 - 03 - 01), Some(date!(2026 - 09 - 01)), false))
        .await
        .expect("spring");
    let m = store
        .add_maintenance(t.id, service(date!(2026 - 09 - 05), Some(date!(2027 - 03 - 01)), true))
        .await
        .expect("fall");
    let t = store.get_equipment(t.id).await.expect("tiller");
    assert_eq!((t.last_serviced_on, t.next_service_on), (Some(date!(2026 - 09 - 05)), Some(date!(2027 - 03 - 01))));
    assert!(!t.service_due(date!(2026 - 10 - 01)) && t.service_due(date!(2027 - 03 - 01)));

    let task = store.get_task(m.task_id.expect("reminder")).await.expect("task");
    assert_eq!((task.title.as_str(), task.due_on), ("Service Tiller", Some(date!(2027 - 03 - 01))));
    let announced: i64 = sqlx::query_scalar("select count(*) from homestead.events where kind = 'task.created'")
        .fetch_one(&pool)
        .await
        .expect("n");
    assert_eq!(announced, 1, "the reminder is an ordinary task, announced like one");
    assert_eq!(store.maintenance_history(t.id).await.expect("history").len(), 2);

    let retired = store
        .set_equipment_state(
            t.id,
            EquipmentState { status: EquipmentStatus::Retired, condition: Condition::Poor, location_id: None },
        )
        .await
        .expect("retire");
    assert!(!retired.service_due(date!(2030 - 01 - 01)));
    assert!(store.list_equipment().await.expect("list").last().is_some_and(|e| e.id == t.id), "retired sorts last");
}

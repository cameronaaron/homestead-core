//! QR tags and tool loans.

use homestead_domain::equipment::*;
use homestead_domain::tags::{NewLoan, TagKind};
use homestead_store::{Store, StoreError};
use sqlx::PgPool;

async fn tool(store: &Store, name: &str) -> Equipment {
    store
        .create_equipment(NewEquipment {
            name: name.into(),
            category: "traps".into(),
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
        .expect("tool")
}

fn to(name: &str) -> NewLoan {
    NewLoan { borrower: name.into(), due_on: None }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_thing_keeps_one_live_tag_until_it_is_retired(pool: PgPool) {
    let store = Store::from_pool(pool);
    let trap = tool(&store, "Trap 3").await;
    let first = store.ensure_tag(TagKind::Equipment, Some(trap.id), "ABCDEFGH23").await.expect("tag");
    assert_eq!((first.label.as_str(), first.target_id), ("Trap 3", Some(trap.id)));
    // Reprinting returns the same code, not a second one.
    let again = store.ensure_tag(TagKind::Equipment, Some(trap.id), "ZZZZZZZZZZ").await.expect("again");
    assert_eq!(again.code, "ABCDEFGH23");
    assert_eq!(store.list_tags().await.expect("list").len(), 1);

    store.retire_tag("ABCDEFGH23").await.expect("retire");
    assert!(matches!(store.tag("ABCDEFGH23").await, Err(StoreError::NotFound(_))));
    let fresh = store.ensure_tag(TagKind::Equipment, Some(trap.id), "ZZZZZZZZZZ").await.expect("fresh");
    assert_eq!(fresh.code, "ZZZZZZZZZZ");

    // A code outside the alphabet is refused before it reaches the table.
    assert!(matches!(
        store.ensure_tag(TagKind::Equipment, Some(trap.id), "OOOOOOOOOO").await,
        Err(StoreError::Invalid(_))
    ));
    // The gate has no target; everything else must have one that exists.
    let gate = store.ensure_tag(TagKind::Gate, None, "GATE234567").await.expect("gate");
    assert_eq!((gate.target_id, gate.label.as_str()), (None, "Sign in at the gate"));
    assert!(store.ensure_tag(TagKind::Equipment, None, "HHHHHHHHHH").await.is_err());
    assert!(matches!(
        store.ensure_tag(TagKind::Equipment, Some(uuid::Uuid::new_v4()), "JJJJJJJJJJ").await,
        Err(StoreError::NotFound(_))
    ));
    // A code already used by something else is a conflict the caller retries.
    let other = tool(&store, "Trap 4").await;
    assert!(matches!(
        store.ensure_tag(TagKind::Equipment, Some(other.id), "ZZZZZZZZZZ").await,
        Err(StoreError::Conflict(_))
    ));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_tool_is_with_one_person_at_a_time(pool: PgPool) {
    let store = Store::from_pool(pool);
    let trap = tool(&store, "Trap 3").await;
    let loan = store.check_out(trap.id, to("Sam")).await.expect("out");
    assert_eq!((loan.borrower.as_str(), loan.equipment_name.as_str()), ("Sam", "Trap 3"));
    let err = store.check_out(trap.id, to("Ava")).await.expect_err("Sam has it");
    assert!(matches!(&err, StoreError::Conflict(m) if m.contains("Sam has it")), "{err}");
    assert_eq!(store.open_loans().await.expect("open").len(), 1);

    let back = store.return_equipment(trap.id, " door sticks ").await.expect("back");
    assert!(back.returned_at.is_some() && back.return_note == "door sticks");
    assert!(store.open_loan(trap.id).await.expect("none").is_none());
    assert!(matches!(store.return_equipment(trap.id, "").await, Err(StoreError::Conflict(_))));

    store.check_out(trap.id, to("Ava")).await.expect("Ava's turn");
    let history = store.loan_history(trap.id, 10).await.expect("history");
    assert_eq!(history.iter().map(|l| l.borrower.as_str()).collect::<Vec<_>>(), ["Ava", "Sam"]);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn broken_tools_stay_put_and_reports_become_tasks(pool: PgPool) {
    let store = Store::from_pool(pool);
    let trap = tool(&store, "Trap 3").await;
    let task = store.report_equipment_problem(trap.id, "Sam", "door spring snapped").await.expect("report");
    assert_eq!(task.title, "Fix Trap 3");
    assert!(task.details.contains("Sam reported: door spring snapped"));
    assert_eq!(store.get_equipment(trap.id).await.expect("e").status, EquipmentStatus::NeedsRepair);
    // Needing repair still lends (the reporter may be wrong); out of service doesn't.
    store.check_out(trap.id, to("Ava")).await.expect("needs repair can still go out");
    store.return_equipment(trap.id, "").await.expect("back");
    store
        .set_equipment_state(
            trap.id,
            EquipmentState { status: EquipmentStatus::OutOfService, condition: Condition::Poor, location_id: None },
        )
        .await
        .expect("state");
    assert!(matches!(store.check_out(trap.id, to("Ava")).await, Err(StoreError::Conflict(_))));
    // A report on an out-of-service tool doesn't bring it back into service.
    store.report_equipment_problem(trap.id, "Ava", "still broken").await.expect("report");
    assert_eq!(store.get_equipment(trap.id).await.expect("e").status, EquipmentStatus::OutOfService);
    assert!(store.report_equipment_problem(trap.id, "Ava", "  ").await.is_err(), "says what's wrong");
}

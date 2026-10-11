//! The planning surface around a community event: who can come, the work to
//! finish, and the stock and equipment that need to be ready.

use homestead_domain::community::*;
use homestead_domain::equipment::{Condition, EquipmentState, EquipmentStatus, NewEquipment};
use homestead_domain::inventory::{MovementKind, NewMovement};
use homestead_domain::{NewInventoryItem, NewTask, TaskPriority};
use homestead_store::{Store, StoreError};
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::macros::date;
use uuid::Uuid;

fn d(n: i64) -> Decimal {
    Decimal::from(n)
}

async fn event(store: &Store) -> CommunityEvent {
    store
        .create_community_event(NewCommunityEvent {
            title: "Fall Volunteer Day".into(),
            kind: EventKind::WorkDay,
            held_on: date!(2026 - 10 - 24),
            time_note: String::new(),
            place: String::new(),
            description: String::new(),
        })
        .await
        .expect("event")
}

/// A volunteer is made by logging an hour under a name.
async fn volunteer(store: &Store, name: &str) -> Uuid {
    store
        .log_volunteer_shift(NewVolunteerShift {
            person_name: name.into(),
            worked_on: date!(2026 - 09 - 01),
            hours: d(1),
            working_group: WorkingGroup::FarmLabor,
            event_id: None,
            notes: String::new(),
        })
        .await
        .expect("shift");
    store
        .active_volunteer_names()
        .await
        .expect("names")
        .into_iter()
        .find(|(_, n)| n == name)
        .unwrap_or_else(|| panic!("{name} is a volunteer"))
        .0
}

async fn task(store: &Store, title: &str) -> Uuid {
    store
        .create_task(NewTask {
            title: title.into(),
            details: String::new(),
            location_id: None,
            due_on: None,
            priority: TaskPriority::Normal,
            mirror_to_linear: false,
            assigned_to: None,
        })
        .await
        .expect("task")
        .id
}

async fn item_with(store: &Store, name: &str, on_hand: i64) -> Uuid {
    let id = store
        .create_inventory_item(NewInventoryItem { name: name.into(), unit: "bag".into(), reorder_level: d(0) })
        .await
        .expect("item")
        .id;
    store
        .record_movement(
            id,
            NewMovement {
                kind: MovementKind::Adjustment,
                quantity: d(on_hand),
                location_id: None,
                note: "count".into(),
            },
        )
        .await
        .expect("stock");
    id
}

async fn tiller(store: &Store) -> Uuid {
    store
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
        .expect("tiller")
        .id
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_new_event_is_scheduled_and_can_be_cancelled_and_restored(pool: PgPool) {
    let store = Store::from_pool(pool);
    let e = event(&store).await;
    assert_eq!(e.status, EventStatus::Scheduled);

    store.set_community_event_status(e.id, EventStatus::Cancelled).await.expect("cancel");
    assert_eq!(store.get_community_event(e.id).await.expect("event").status, EventStatus::Cancelled);
    store.set_community_event_status(e.id, EventStatus::Scheduled).await.expect("restore");
    assert_eq!(store.get_community_event(e.id).await.expect("event").status, EventStatus::Scheduled);

    let missing = store.set_community_event_status(Uuid::new_v4(), EventStatus::Cancelled).await;
    assert!(matches!(missing, Err(StoreError::NotFound("event"))), "{missing:?}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn editing_an_event_changes_its_details_and_validates_them(pool: PgPool) {
    let store = Store::from_pool(pool);
    let e = event(&store).await;
    let edit = |title: &str| EventEdit {
        title: title.into(),
        kind: EventKind::Potluck,
        held_on: date!(2026 - 10 - 31),
        time_note: "noon".into(),
        place: "Barn".into(),
        description: "Bring a dish".into(),
    };

    let changed = store.update_community_event(e.id, edit("  Harvest potluck ")).await.expect("edit");
    assert_eq!(
        (changed.title.as_str(), changed.kind, changed.held_on),
        ("Harvest potluck", EventKind::Potluck, date!(2026 - 10 - 31))
    );
    assert_eq!((changed.time_note.as_str(), changed.place.as_str()), ("noon", "Barn"));
    assert_eq!(changed.status, EventStatus::Scheduled, "an edit leaves the status alone");

    assert!(store.update_community_event(e.id, edit("   ")).await.is_err(), "a blank title is refused");
    assert_eq!(store.get_community_event(e.id).await.expect("event").title, "Harvest potluck", "and changes nothing");
    let gone = store.update_community_event(Uuid::new_v4(), edit("x")).await;
    assert!(matches!(gone, Err(StoreError::NotFound("event"))), "{gone:?}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_volunteer_has_one_rsvp_per_event_and_the_latest_answer_wins(pool: PgPool) {
    let store = Store::from_pool(pool);
    let e = event(&store).await;
    let ava = volunteer(&store, "Ava Rosate").await;
    let cam = volunteer(&store, "Cameron").await;

    store.set_event_rsvp(e.id, ava, Availability::Maybe, "  after noon ").await.expect("maybe");
    store.set_event_rsvp(e.id, cam, Availability::Attending, "").await.expect("attending");
    store.set_event_rsvp(e.id, ava, Availability::Attending, "free all day").await.expect("changed mind");

    let plan = store.event_plan(e.id).await.expect("plan");
    assert_eq!(plan.rsvps.len(), 2, "answering twice doesn't add a row: {:?}", plan.rsvps);
    let ava_rsvp = plan.rsvps.iter().find(|r| r.volunteer_id == ava).expect("ava");
    assert_eq!((ava_rsvp.availability, ava_rsvp.note.as_str()), (Availability::Attending, "free all day"));
    assert_eq!(ava_rsvp.volunteer_name, "Ava Rosate");

    let nobody = store.set_event_rsvp(e.id, Uuid::new_v4(), Availability::Attending, "").await;
    assert!(matches!(nobody, Err(StoreError::NotFound("event or volunteer"))), "{nobody:?}");
    let nowhere = store.set_event_rsvp(Uuid::new_v4(), ava, Availability::Attending, "").await;
    assert!(matches!(nowhere, Err(StoreError::NotFound("event or volunteer"))), "{nowhere:?}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn linking_a_task_twice_lists_it_once_and_unlinking_keeps_the_task(pool: PgPool) {
    let store = Store::from_pool(pool);
    let e = event(&store).await;
    let t = task(&store, "Stack the lumber").await;

    store.link_event_task(e.id, t).await.expect("link");
    store.link_event_task(e.id, t).await.expect("again");
    let plan = store.event_plan(e.id).await.expect("plan");
    assert_eq!(plan.tasks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["Stack the lumber"]);

    store.unlink_event_resource(e.id, "task", t).await.expect("unlink");
    assert!(store.event_plan(e.id).await.expect("plan").tasks.is_empty());
    assert!(store.get_task(t).await.is_ok(), "unlinking removes the link, never the task");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn planned_stock_is_shown_against_what_is_on_hand(pool: PgPool) {
    let store = Store::from_pool(pool);
    let e = event(&store).await;
    let mulch = item_with(&store, "Mulch", 4).await;

    store.link_event_inventory(e.id, mulch, d(10), "for the beds").await.expect("link");
    let plan = store.event_plan(e.id).await.expect("plan");
    let line = &plan.inventory[0];
    assert_eq!((line.name.as_str(), line.quantity, line.on_hand), ("Mulch", d(10), d(4)), "short by six");

    store.link_event_inventory(e.id, mulch, d(3), "fewer").await.expect("re-link changes the amount");
    let plan = store.event_plan(e.id).await.expect("plan");
    assert_eq!((plan.inventory.len(), plan.inventory[0].quantity), (1, d(3)));

    for bad in [d(0), d(-2)] {
        let refused = store.link_event_inventory(e.id, mulch, bad, "").await;
        assert!(refused.is_err(), "a quantity of {bad} is refused");
    }
    assert_eq!(store.event_plan(e.id).await.expect("plan").inventory[0].quantity, d(3), "and changes nothing");

    store.unlink_event_resource(e.id, "inventory", mulch).await.expect("unlink");
    assert!(store.event_plan(e.id).await.expect("plan").inventory.is_empty());
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn equipment_out_of_service_is_flagged_as_unavailable(pool: PgPool) {
    let store = Store::from_pool(pool);
    let e = event(&store).await;
    let t = tiller(&store).await;

    store.link_event_equipment(e.id, t, "for the beds").await.expect("link");
    assert!(store.event_plan(e.id).await.expect("plan").equipment[0].available, "in service by default");

    store
        .set_equipment_state(
            t,
            EquipmentState { status: EquipmentStatus::NeedsRepair, condition: Condition::Poor, location_id: None },
        )
        .await
        .expect("breaks");
    let plan = store.event_plan(e.id).await.expect("plan");
    assert_eq!((plan.equipment[0].name.as_str(), plan.equipment[0].available), ("Tiller", false));

    store.unlink_event_resource(e.id, "equipment", t).await.expect("unlink");
    assert!(store.event_plan(e.id).await.expect("plan").equipment.is_empty());
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn links_to_things_that_do_not_exist_are_refused_not_crashes(pool: PgPool) {
    let store = Store::from_pool(pool);
    let e = event(&store).await;
    let nowhere = Uuid::new_v4();

    for refused in [
        store.link_event_task(e.id, nowhere).await,
        store.link_event_task(nowhere, task(&store, "x").await).await,
        store.link_event_equipment(e.id, nowhere, "").await,
        store.link_event_inventory(e.id, nowhere, d(1), "").await,
    ] {
        assert!(matches!(refused, Err(StoreError::NotFound(_))), "{refused:?}");
    }
    let unknown_kind = store.unlink_event_resource(e.id, "volunteer", nowhere).await;
    assert!(matches!(unknown_kind, Err(StoreError::NotFound("event resource"))), "{unknown_kind:?}");
    assert!(store.event_plan(nowhere).await.is_err(), "no plan for an event that isn't there");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn deleting_an_event_takes_its_plan_but_not_the_things_in_it(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let e = event(&store).await;
    let (v, t, i, q) = (
        volunteer(&store, "Ava Rosate").await,
        task(&store, "Stack the lumber").await,
        item_with(&store, "Mulch", 5).await,
        tiller(&store).await,
    );
    store.set_event_rsvp(e.id, v, Availability::Attending, "").await.expect("rsvp");
    store.link_event_task(e.id, t).await.expect("task");
    store.link_event_inventory(e.id, i, d(2), "").await.expect("stock");
    store.link_event_equipment(e.id, q, "").await.expect("equipment");

    sqlx::query("delete from homestead.community_events where id = $1")
        .bind(e.id)
        .execute(&pool)
        .await
        .expect("delete");

    // sqlx 0.9 takes only static SQL, so the four tables are counted in one query.
    let counts: (i64, i64, i64, i64) = sqlx::query_as(
        "select (select count(*) from homestead.event_rsvps), (select count(*) from homestead.event_tasks),
                (select count(*) from homestead.event_inventory), (select count(*) from homestead.event_equipment)",
    )
    .fetch_one(&pool)
    .await
    .expect("counts");
    assert_eq!(counts, (0, 0, 0, 0), "rsvp, task, stock and equipment rows go with the event");
    assert!(store.get_task(t).await.is_ok() && store.get_equipment(q).await.is_ok(), "the task and tiller remain");
    assert!(
        store.active_volunteer_names().await.expect("names").iter().any(|(id, _)| *id == v),
        "so does the volunteer"
    );
}

//! The stock ledger and purchasing against a real Postgres.

use homestead_domain::inventory::*;
use homestead_domain::{LocationKind, NewInventoryItem, NewLocation};
use homestead_store::{Store, StoreError};
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::macros::date;
use uuid::Uuid;

fn d(n: i64) -> Decimal {
    Decimal::from(n)
}

async fn place(store: &Store, name: &str) -> Uuid {
    store.create_location(NewLocation::named(name, LocationKind::Storage)).await.expect("location").id
}

async fn item(store: &Store, name: &str, reorder: i64) -> Uuid {
    store
        .create_inventory_item(NewInventoryItem { name: name.into(), unit: "bag".into(), reorder_level: d(reorder) })
        .await
        .expect("item")
        .id
}

fn movement(kind: MovementKind, qty: i64, at: Option<Uuid>) -> NewMovement {
    NewMovement { kind, quantity: d(qty), location_id: at, note: "count".into() }
}

/// On hand, per place and in total, is exactly the sum of the ledger.
async fn assert_ledger_consistent(pool: &PgPool) {
    let bad: i64 = sqlx::query_scalar(
        "select count(*) from homestead.inventory_items i
         where i.on_hand <> coalesce((select sum(delta) from homestead.stock_movements m where m.item_id = i.id), 0)
            or i.on_hand <> coalesce((select sum(on_hand) from homestead.inventory_balances b where b.item_id = i.id), 0)
            or exists (
              select 1 from homestead.inventory_balances b where b.item_id = i.id
                and b.on_hand <> coalesce((select sum(delta) from homestead.stock_movements m
                                           where m.item_id = i.id and m.location_id is not distinct from b.location_id), 0))",
    )
    .fetch_one(pool)
    .await
    .expect("consistency");
    assert_eq!(bad, 0, "balances disagree with the ledger");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn stock_moves_between_places_and_never_goes_negative_anywhere(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let (shed, coop) = (place(&store, "Feed shed").await, place(&store, "Coop").await);
    let feed = item(&store, "Layer feed", 0).await;

    store.record_movement(feed, movement(MovementKind::Harvest, 10, Some(shed))).await.expect("in");
    let after = store
        .transfer_stock(
            feed,
            Transfer { quantity: d(4), from_location_id: Some(shed), to_location_id: Some(coop), note: String::new() },
        )
        .await
        .expect("transfer");
    assert_eq!(after.on_hand, d(10), "a transfer doesn't change the total");
    let balances = store.item_balances(feed).await.expect("balances");
    let at = |id| balances.iter().find(|b| b.location_id == Some(id)).map(|b| b.on_hand);
    assert_eq!((at(shed), at(coop)), (Some(d(6)), Some(d(4))));

    // The coop has 4; taking 5 from it fails even though 10 exist overall.
    let err = store.record_movement(feed, movement(MovementKind::Consumption, 5, Some(coop))).await;
    assert!(matches!(err, Err(StoreError::InsufficientStock)), "{err:?}");
    // With stock in two places, "from where?" must be answered.
    let err = store.record_movement(feed, movement(MovementKind::Consumption, 1, None)).await;
    assert!(matches!(err, Err(StoreError::Invalid(_))), "{err:?}");
    store.record_movement(feed, movement(MovementKind::Consumption, 4, Some(coop))).await.expect("use coop's");
    // Now only the shed has any, so it's the obvious source.
    let after = store.record_movement(feed, movement(MovementKind::Waste, 1, None)).await.expect("wet bag");
    assert_eq!(after.on_hand, d(5));
    let ledger = store.item_movements(feed, 50).await.expect("ledger");
    assert_eq!(ledger.len(), 5);
    assert_eq!(ledger[0].location_id, Some(shed), "defaulted to the only stocked place");
    assert!(ledger.iter().filter(|m| m.transfer_id.is_some()).count() == 2);
    assert_ledger_consistent(&pool).await;
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_ledger_is_the_only_way_to_change_what_we_have(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let feed = item(&store, "Layer feed", 0).await;
    store.record_movement(feed, movement(MovementKind::Harvest, 3, None)).await.expect("in");
    for sql in [
        "update homestead.inventory_items set on_hand = 100",
        "update homestead.inventory_balances set on_hand = 100",
        "delete from homestead.inventory_balances",
        "update homestead.stock_movements set delta = 100",
        "delete from homestead.stock_movements",
        "insert into homestead.inventory_items (name, unit, on_hand) values ('Ghost', 'bag', 5)",
    ] {
        assert!(sqlx::query(sql).execute(&pool).await.is_err(), "{sql} was allowed");
    }
    // Renaming an item, or changing its reorder level, is still fine.
    sqlx::query("update homestead.inventory_items set reorder_level = 2, name = 'Layer pellets'")
        .execute(&pool)
        .await
        .expect("edit");
    assert_ledger_consistent(&pool).await;
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_purchase_line_is_received_into_stock_exactly_once(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let shed = place(&store, "Feed shed").await;
    let feed = item(&store, "Layer feed", 2).await;
    let vendor = store
        .create_vendor(NewVendor {
            name: "Valley Feed & Seed".into(),
            email: "Orders@ValleyFeed.example".into(),
            phone: String::new(),
            website: String::new(),
            notes: String::new(),
        })
        .await
        .expect("vendor");
    assert_eq!(vendor.email, "orders@valleyfeed.example");
    let purchase = store
        .create_purchase(NewPurchase {
            vendor_id: Some(vendor.id),
            purchased_on: date!(2026 - 09 - 10),
            amount: Decimal::new(8650, 2),
            currency: Currency::Usd,
            category: "Feed".into(),
            description: "Monthly feed".into(),
            external_reference: Some("INV-2291".into()),
        })
        .await
        .expect("purchase");
    assert_eq!(purchase.vendor_name.as_deref(), Some("Valley Feed & Seed"));
    let bags = store
        .add_purchase_line(
            purchase.id,
            NewPurchaseLine {
                item_id: Some(feed),
                description: String::new(),
                quantity: d(4),
                unit_cost: Some(Decimal::new(1950, 2)),
            },
        )
        .await
        .expect("line");
    let fee = store
        .add_purchase_line(
            purchase.id,
            NewPurchaseLine { item_id: None, description: "Delivery".into(), quantity: d(1), unit_cost: Some(d(8)) },
        )
        .await
        .expect("fee line");

    let received = store.receive_purchase_line(bags.id, Some(shed)).await.expect("receive");
    assert!(received.received_at.is_some());
    let again = store.receive_purchase_line(bags.id, Some(shed)).await.expect("idempotent");
    assert_eq!(again.received_at, received.received_at);
    assert_eq!(store.get_inventory_item(feed).await.expect("item").on_hand, d(4), "counted once");
    assert!(
        matches!(store.receive_purchase_line(fee.id, None).await, Err(StoreError::Conflict(_))),
        "nothing to stock"
    );

    // The database backstops the idempotency even if the code didn't.
    let dup = sqlx::query(
        "insert into homestead.stock_movements (item_id, delta, reason, kind, purchase_line_id) values ($1, 4, 'x', 'receipt', $2)",
    )
    .bind(feed)
    .bind(bags.id)
    .execute(&pool)
    .await;
    assert!(dup.is_err(), "a second receipt for the same line");

    // Where did the feed come from?
    let history = store.item_purchase_history(feed).await.expect("history");
    assert_eq!(history.len(), 1);
    assert_eq!((history[0].0.external_reference.as_deref(), history[0].1.quantity), (Some("INV-2291"), d(4)));
    let ledger = store.item_movements(feed, 10).await.expect("ledger");
    assert_eq!((ledger[0].kind, ledger[0].purchase_line_id), (MovementKind::Receipt, Some(bags.id)));
    assert_ledger_consistent(&pool).await;
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn receiving_stock_above_the_reorder_level_does_not_alert_but_using_it_does(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let feed = item(&store, "Layer feed", 2).await;
    store.record_movement(feed, movement(MovementKind::Harvest, 5, None)).await.expect("in");
    store.record_movement(feed, movement(MovementKind::Consumption, 3, None)).await.expect("to 2");
    store.record_movement(feed, movement(MovementKind::Waste, 1, None)).await.expect("to 1, still low");
    let n: i64 = sqlx::query_scalar("select count(*) from homestead.events where kind = 'inventory.low_stock'")
        .fetch_one(&pool)
        .await
        .expect("n");
    assert_eq!(n, 1, "one alert per downward crossing");
}

/// Appends a movement dated `days_ago` (the ledger is append-only, so
/// history is written, never rewritten).
async fn moved(pool: &PgPool, item: Uuid, kind: &str, delta: i64, days_ago: i32) {
    sqlx::query(
        "insert into homestead.stock_movements (item_id, delta, reason, kind, at)
         values ($1, $2, 'test', $3, now() - make_interval(days => $4))",
    )
    .bind(item)
    .bind(Decimal::from(delta))
    .bind(kind)
    .bind(days_ago)
    .execute(pool)
    .await
    .expect("movement");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn how_long_stock_lasts_comes_from_how_fast_it_has_gone(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    // Tracked for 10 days, 5 used: half a bag a day, so 15 bags last 30 days.
    let feed = item(&store, "Layer feed", 2).await;
    moved(&pool, feed, "adjustment", 20, 10).await;
    moved(&pool, feed, "consumption", -5, 2).await;
    // New three days ago and three used: a bag a day, however young.
    let grit = item(&store, "Grit", 0).await;
    moved(&pool, grit, "adjustment", 7, 3).await;
    moved(&pool, grit, "consumption", -3, 1).await;
    // Use outside the window doesn't count; waste does.
    let hay = item(&store, "Hay", 0).await;
    moved(&pool, hay, "adjustment", 50, 90).await;
    moved(&pool, hay, "consumption", -20, 60).await;
    moved(&pool, hay, "waste", -3, 5).await;
    // Never used: no pace to speak of.
    let spare = item(&store, "Spare hose", 0).await;
    moved(&pool, spare, "adjustment", 2, 40).await;

    let paces = store.usage_paces().await.expect("paces");
    let left = |id: &Uuid, on_hand: i64| paces[id].days_left(d(on_hand));
    assert_eq!(paces[&feed], Pace { used: d(5), over_days: 10 });
    assert_eq!(left(&feed, 15), Some(30));
    assert_eq!(paces[&grit], Pace { used: d(3), over_days: 3 });
    assert_eq!(left(&grit, 4), Some(4));
    assert_eq!(paces[&hay], Pace { used: d(3), over_days: 30 });
    assert_eq!(left(&spare, 2), None);
}

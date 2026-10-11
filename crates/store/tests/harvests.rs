//! Where a harvest went, traced back to its seed and forward to sales and
//! donations, without ever exceeding or mixing up its unit.

use homestead_domain::harvests::{DispositionKind, Fate, NewDisposition, shares};
use homestead_domain::seeds::NewSeedLot;
use homestead_domain::stand::{NewStandSale, Payment, SaleUnit};
use homestead_domain::*;
use homestead_store::{Store, StoreError};
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::macros::date;

fn d(n: i64) -> Decimal {
    Decimal::from(n)
}

async fn tomatoes(store: &Store, lb: i64) -> (Harvest, Option<uuid::Uuid>) {
    let bed = store.create_location(NewLocation::named("A3", LocationKind::Bed)).await.expect("bed");
    let lot = store
        .create_seed_lot(NewSeedLot {
            crop: "Tomato".into(),
            variety: "Sungold".into(),
            supplier: "Baker Creek".into(),
            supplier_lot: "A-2291".into(),
            vendor_id: None,
            acquired_on: None,
            packed_for_year: None,
            organic: None,
            quantity: None,
            unit: None,
            notes: String::new(),
        })
        .await
        .expect("lot");
    let planting = store
        .create_planting(NewPlanting {
            seed_lot_id: Some(lot.id),
            ..NewPlanting::at(bed.id, "Tomato", date!(2026 - 04 - 01))
        })
        .await
        .expect("planting");
    let harvest = store
        .log_harvest(NewHarvest {
            planting_id: planting.id,
            harvested_on: date!(2026 - 07 - 04),
            quantity: d(lb),
            unit: HarvestUnit::Lb,
            notes: String::new(),
        })
        .await
        .expect("harvest");
    (harvest, Some(lot.id))
}

fn donation(q: i64, to: &str) -> NewDisposition {
    NewDisposition { recipient: to.into(), ..NewDisposition::of(DispositionKind::Donation, d(q)) }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_42_lb_of_tomatoes_split_four_ways(pool: PgPool) {
    let store = Store::from_pool(pool);
    let (h, lot) = tomatoes(&store, 42).await;
    store.split_harvest(h.id, NewDisposition::of(DispositionKind::Sale, d(22))).await.expect("sold");
    let gift = store.split_harvest(h.id, donation(15, "Valley Food Pantry")).await.expect("donated");
    store.split_harvest(h.id, NewDisposition::of(DispositionKind::Community, d(3))).await.expect("used");
    store.split_harvest(h.id, NewDisposition::of(DispositionKind::Compost, d(2))).await.expect("composted");
    let over = store.split_harvest(h.id, NewDisposition::of(DispositionKind::Feed, d(1))).await.expect_err("over");
    assert!(matches!(&over, StoreError::Conflict(m) if m.contains("exceed")), "{over}");

    let trace = store.get_harvest(h.id).await.expect("trace");
    assert_eq!(trace.seed_lot.map(|l| l.id), lot, "traced back to its seed");
    let parts: Vec<_> = trace.dispositions.iter().map(|x| (x.kind, x.quantity)).collect();
    let s = shares(trace.harvest.quantity, &parts);
    assert_eq!(
        s.parts.iter().map(|(f, q, _)| (*f, *q)).collect::<Vec<_>>(),
        [(Fate::Sold, d(22)), (Fate::Donated, d(15)), (Fate::Used, d(3)), (Fate::Lost, d(2))]
    );

    let donations = store.donations_between(date!(2026 - 07 - 01), date!(2026 - 07 - 31)).await.expect("donations");
    assert_eq!(donations.len(), 1);
    let g = &donations[0];
    assert_eq!(
        (g.recipient.as_str(), g.product.as_str(), g.quantity, g.unit, g.harvest_id, g.donated_on),
        ("Valley Food Pantry", "Tomato", d(15), HarvestUnit::Lb, Some(h.id), date!(2026 - 07 - 04))
    );

    // A mistake is undone with its donation record.
    assert_eq!(store.remove_disposition(gift.id).await.expect("removed"), h.id);
    assert!(store.donations_between(date!(2026 - 07 - 01), date!(2026 - 07 - 31)).await.expect("d").is_empty());
    assert_eq!(store.get_harvest(h.id).await.expect("t").dispositions.len(), 3);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_stand_sale_counts_against_its_harvest_only_in_the_same_unit(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let (h, _) = tomatoes(&store, 10).await;
    let sale = |q: i64, unit: SaleUnit| NewStandSale {
        sold_on: date!(2026 - 07 - 05),
        item: "Tomatoes".into(),
        quantity: d(q),
        unit,
        amount: d(q * 4),
        payment: Payment::Cash,
        notes: String::new(),
        harvest_id: Some(h.id),
    };
    let sold = store.record_sale(sale(4, SaleUnit::Lb)).await.expect("sold");
    let bag = store.record_sale(sale(1, SaleUnit::Bag)).await.expect_err("bags aren't pounds");
    assert!(matches!(bag, StoreError::Invalid(_)), "{bag}");
    let over = store.record_sale(sale(7, SaleUnit::Lb)).await.expect_err("only 6 lb left");
    assert!(matches!(over, StoreError::Conflict(_)), "{over}");
    let sales: i64 =
        sqlx::query_scalar("select count(*) from homestead.stand_sales").fetch_one(&pool).await.expect("n");
    assert_eq!(sales, 1, "a refused link records no sale either");
    let trace = store.get_harvest(h.id).await.expect("trace");
    assert_eq!(trace.dispositions.len(), 1);
    assert_eq!(
        (trace.dispositions[0].kind, trace.dispositions[0].stand_sale_id),
        (DispositionKind::Sale, Some(sold.id))
    );
}

/// Two splits in flight at once: A has inserted 6 of 10 lb but not yet
/// committed when B tries 6 more. B must wait for A and then be refused,
/// not both see room. (Racing two store calls didn't overlap reliably
/// enough to fail without the lock; holding A open does, every time.)
#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn concurrent_splits_cannot_together_exceed_the_harvest(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let (h, _) = tomatoes(&store, 10).await;
    let split =
        "insert into homestead.harvest_dispositions (harvest_id, kind, quantity, unit) values ($1, 'sale', 6, 'lb')";
    let mut a = pool.begin().await.expect("a");
    sqlx::query(split).bind(h.id).execute(&mut *a).await.expect("a fits");
    let b = tokio::spawn({
        let pool = pool.clone();
        async move {
            let mut b = pool.begin().await.expect("b");
            sqlx::query(split).bind(h.id).execute(&mut *b).await?;
            b.commit().await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!b.is_finished(), "B went ahead while A was still open");
    a.commit().await.expect("a commits");
    let refused = b.await.expect("join").expect_err("B would exceed the harvest");
    assert!(refused.to_string().contains("exceed"), "{refused}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_seed_lot_knows_what_it_produced(pool: PgPool) {
    let store = Store::from_pool(pool);
    let (h, lot) = tomatoes(&store, 5).await;
    let from_lot = store.harvests_from_lot(lot.expect("lot")).await.expect("harvests");
    assert_eq!(from_lot.iter().map(|x| (x.id, x.quantity)).collect::<Vec<_>>(), [(h.id, d(5))]);
}

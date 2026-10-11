//! Seed lots: which lot produced a planting, and what's left.

use homestead_domain::seeds::{NewSeedLot, SeedUnit};
use homestead_domain::{LocationKind, NewLocation, NewPlanting};
use homestead_store::{Store, StoreError};
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::macros::date;

fn tomato(quantity: Option<i64>) -> NewSeedLot {
    NewSeedLot {
        crop: "Tomato".into(),
        variety: "Sungold".into(),
        supplier: "Baker Creek".into(),
        supplier_lot: "A-2291".into(),
        vendor_id: None,
        acquired_on: Some(date!(2026 - 01 - 15)),
        packed_for_year: Some(2026),
        organic: Some(true),
        quantity: quantity.map(Decimal::from),
        unit: quantity.map(|_| SeedUnit::Seeds),
        notes: String::new(),
    }
}

fn sow(location_id: uuid::Uuid, lot: Option<uuid::Uuid>, used: Option<i64>) -> NewPlanting {
    NewPlanting {
        variety: "Sungold".into(),
        seed_lot_id: lot,
        seed_quantity_used: used.map(Decimal::from),
        ..NewPlanting::at(location_id, "Tomato", date!(2026 - 03 - 01))
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_lot_knows_its_plantings_and_what_is_left(pool: PgPool) {
    let store = Store::from_pool(pool);
    let bed = |n: &str| NewLocation::named(n, LocationKind::Bed);
    let (b1, b2) = (
        store.create_location(bed("Bed 1")).await.expect("b1").id,
        store.create_location(bed("Bed 2")).await.expect("b2").id,
    );
    let lot = store.create_seed_lot(tomato(Some(30))).await.expect("lot");
    // The same supplier lot can't be recorded twice.
    assert!(matches!(store.create_seed_lot(tomato(Some(5))).await, Err(StoreError::Conflict(_))));
    let p1 = store.create_planting(sow(b1, Some(lot.id), Some(12))).await.expect("p1");
    assert_eq!(p1.seed_lot_id, Some(lot.id), "the planting says which lot produced it");
    store.create_planting(sow(b2, Some(lot.id), Some(10))).await.expect("p2");

    let lot = store.get_seed_lot(lot.id).await.expect("lot");
    assert_eq!((lot.used, lot.remaining(), lot.plantings), (Decimal::from(22), Some(Decimal::from(8)), 2));
    let from = store.plantings_from_lot(lot.id).await.expect("plantings");
    let mut beds: Vec<_> = from.iter().map(|(_, loc, _)| loc.as_str()).collect();
    beds.sort();
    assert_eq!(beds, vec!["Bed 1", "Bed 2"]);

    // Using more than is left is refused; the lot is unchanged.
    let err = store.create_planting(sow(b1, Some(lot.id), Some(9))).await.expect_err("only 8 left");
    assert!(matches!(&err, StoreError::Conflict(m) if m.contains("8 left")), "{err:?}");
    assert_eq!(store.get_seed_lot(lot.id).await.expect("lot").plantings, 2);
    // An uncounted lot can be sown from freely; a planting may skip the amount.
    let saved = store
        .create_seed_lot(NewSeedLot {
            supplier: "Member's saved seed".into(),
            supplier_lot: String::new(),
            ..tomato(None)
        })
        .await
        .expect("saved seed");
    store.create_planting(sow(b1, Some(saved.id), None)).await.expect("uncounted");
    store.create_planting(sow(b1, Some(lot.id), None)).await.expect("amount not recorded");
    assert!(matches!(
        store.create_planting(sow(b1, Some(uuid::Uuid::new_v4()), None)).await,
        Err(StoreError::NotFound(_))
    ));
}

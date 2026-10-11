//! Farm stand and compost against a real Postgres.

use homestead_domain::stand::*;
use homestead_store::Store;
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::macros::date;

fn sale(item: &str, qty: i64, unit: SaleUnit, amount: i64, on: time::Date) -> NewStandSale {
    NewStandSale {
        sold_on: on,
        item: item.into(),
        quantity: Decimal::from(qty),
        unit,
        amount: Decimal::from(amount),
        payment: Payment::Cash,
        notes: String::new(),
        harvest_id: None,
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn every_unit_and_payment_is_accepted(pool: PgPool) {
    let store = Store::from_pool(pool);
    for u in SaleUnit::ALL {
        store.record_sale(sale("x", 1, *u, 1, date!(2026 - 09 - 26))).await.unwrap_or_else(|e| panic!("{u}: {e}"));
    }
    for p in Payment::ALL {
        let s = NewStandSale { payment: *p, ..sale("x", 1, SaleUnit::Each, 1, date!(2026 - 09 - 26)) };
        store.record_sale(s).await.unwrap_or_else(|e| panic!("{p}: {e}"));
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn totals_group_by_item_and_never_mix_units(pool: PgPool) {
    let store = Store::from_pool(pool);
    let d = date!(2026 - 09 - 26);
    store.record_sale(sale("Eggs", 3, SaleUnit::Dozen, 21, d)).await.expect("1");
    store.record_sale(sale(" eggs", 2, SaleUnit::Dozen, 14, d)).await.expect("2");
    store.record_sale(sale("Eggs", 6, SaleUnit::Each, 4, d)).await.expect("half dozen loose");
    store.record_sale(sale("Kale", 2, SaleUnit::Bunch, 8, d)).await.expect("3");
    store.record_sale(sale("Eggs", 9, SaleUnit::Dozen, 63, date!(2026 - 08 - 01))).await.expect("outside");
    let t = store.sales_totals(date!(2026 - 09 - 01), date!(2026 - 09 - 30)).await.expect("totals");
    assert_eq!(
        t,
        [
            ItemTotal {
                item: "Eggs".into(),
                unit: SaleUnit::Dozen,
                quantity: Decimal::from(5),
                amount: Decimal::from(35)
            },
            ItemTotal {
                item: "Kale".into(),
                unit: SaleUnit::Bunch,
                quantity: Decimal::from(2),
                amount: Decimal::from(8)
            },
            ItemTotal {
                item: "Eggs".into(),
                unit: SaleUnit::Each,
                quantity: Decimal::from(6),
                amount: Decimal::from(4)
            },
        ]
    );
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn compost_counts_every_dropoff_and_weighs_the_weighed(pool: PgPool) {
    let store = Store::from_pool(pool);
    let c = |w: Option<i64>| NewCompostDropoff {
        received_on: date!(2026 - 09 - 20),
        contributor: "Neighbour".into(),
        weight_lb: w.map(Decimal::from),
        notes: String::new(),
    };
    store.record_compost_dropoff(c(Some(12))).await.expect("1");
    store.record_compost_dropoff(c(None)).await.expect("2");
    store.record_compost_dropoff(c(Some(8))).await.expect("3");
    let (n, lb) = store.compost_totals(date!(2026 - 09 - 01), date!(2026 - 09 - 30)).await.expect("totals");
    assert_eq!((n, lb), (3, Decimal::from(20)));
}

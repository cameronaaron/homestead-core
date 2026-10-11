//! Gifts against a real Postgres.

use homestead_domain::gifts::*;
use homestead_store::{Store, StoreError};
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::macros::date;

fn new(kind: GiftKind, amount: Option<i64>, description: &str) -> NewGift {
    NewGift {
        donor_name: "Kellogg's".into(),
        donor_email: String::new(),
        kind,
        received_on: date!(2026 - 09 - 18),
        amount: amount.map(Decimal::from),
        description: description.into(),
        estimated_value: None,
        goods_or_services: String::new(),
        goods_value: None,
        fund: String::new(),
        notes: String::new(),
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn every_gift_kind_round_trips(pool: PgPool) {
    let store = Store::from_pool(pool);
    for k in GiftKind::ALL {
        let input = match k {
            GiftKind::InKind => new(*k, None, "40 vegetable starts"),
            _ => new(*k, Some(300), ""),
        };
        let g = store.record_gift(input).await.unwrap_or_else(|e| panic!("{k}: {e}"));
        assert_eq!(store.get_gift(g.id).await.expect("get"), g);
        assert!(g.needs_acknowledgment(), "{k}");
    }
    assert_eq!(store.list_gifts().await.expect("list").len(), GiftKind::ALL.len());
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn acknowledgment_date_is_set_once(pool: PgPool) {
    let store = Store::from_pool(pool);
    let g = store.record_gift(new(GiftKind::Cash, Some(500), "")).await.expect("gift");
    store.mark_gift_acknowledged(g.id, date!(2026 - 09 - 20)).await.expect("ack");
    store.mark_gift_acknowledged(g.id, date!(2026 - 10 - 01)).await.expect("again");
    let g = store.get_gift(g.id).await.expect("get");
    assert_eq!(g.acknowledged_on, Some(date!(2026 - 09 - 20)));
    assert!(!g.needs_acknowledgment());
    let missing = store.mark_gift_acknowledged(uuid::Uuid::new_v4(), date!(2026 - 09 - 20)).await;
    assert!(matches!(missing, Err(StoreError::NotFound("gift"))));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_database_refuses_a_valued_in_kind_gift(pool: PgPool) {
    let r = sqlx::query(
        "insert into homestead.gifts (donor_name, kind, received_on, amount, description)
         values ('X', 'in_kind', '2026-09-18', 100, 'plants')",
    )
    .execute(&pool)
    .await;
    assert!(r.is_err(), "in-kind gifts carry no amount");
}

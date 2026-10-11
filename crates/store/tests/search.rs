//! Search: finds by any word's start, ranks names first, and each query
//! is served by its GIN index.

use homestead_domain::search::{SearchKind, prefix_query};
use homestead_store::Store;
use homestead_store::search::search_sql;
use sqlx::PgPool;

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn finds_records_by_the_start_of_any_word(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    for sql in [
        "insert into homestead.animals (species, name, sex, status, origin, arrived_on, notes)
         values ('cat', 'Mochi', 'female', 'in_care', 'trapped', '2026-09-01', 'shy, likes the feed shed')",
        "insert into homestead.inventory_items (name, unit) values ('Layer feed', 'bag')",
        "insert into homestead.tasks (title, details) values ('Move Mochi indoors', 'before the rain')",
        "insert into homestead.contacts (name, email, source) values ('Ana Ruiz', 'ana@example.org', 'test')",
        "insert into homestead.gifts (donor_name, kind, received_on, description) values ('Green Thumb Nursery', 'in_kind', '2026-09-18', '40 vegetable starts')",
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    let find = |q: &str| {
        let store = store.clone();
        let q = prefix_query(q).expect("query");
        async move { store.search(SearchKind::ALL, &q, 10).await.expect("search") }
    };
    let hits = find("moch").await;
    assert_eq!(hits.len(), 2);
    assert_eq!((hits[0].kind, hits[0].title.as_str()), (SearchKind::Animal, "Mochi"), "a name outranks a mention");
    assert_eq!(hits[1].kind, SearchKind::Task);
    assert!(find("feed").await.iter().any(|h| h.kind == SearchKind::Item && h.url.starts_with("/inventory/")));
    assert_eq!(find("ana@exa").await.first().map(|h| h.kind), Some(SearchKind::Contact));
    assert_eq!(find("green nurs").await.first().map(|h| h.title.as_str()), Some("Green Thumb Nursery"));
    assert!(find("mochi rain").await.iter().all(|h| h.kind == SearchKind::Task), "every word must match");
    assert!(find("zebra").await.is_empty());
    // Only the kinds asked for are searched.
    let only_tasks = store.search(&[SearchKind::Task], &prefix_query("mochi").expect("q"), 10).await.expect("s");
    assert!(only_tasks.iter().all(|h| h.kind == SearchKind::Task));
}

/// The index is justified only if the query can use it. With sequential
/// scans off, every search plan must go through its GIN index.
#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn every_search_can_use_its_index(pool: PgPool) {
    let mut conn = pool.acquire().await.expect("conn");
    sqlx::query("set enable_seqscan = off").execute(&mut *conn).await.expect("set");
    for kind in SearchKind::ALL {
        let sql = format!("explain {}", search_sql(*kind).replace("$1", "'moch:*'").replace("$2", "10"));
        let plan: Vec<String> =
            sqlx::query_scalar(sqlx::AssertSqlSafe(sql)).fetch_all(&mut *conn).await.expect("explain");
        let plan = plan.join("\n");
        assert!(plan.contains("_search"), "{kind} doesn't use its index:\n{plan}");
    }
}

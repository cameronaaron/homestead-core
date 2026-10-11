//! The mailing list against a real Postgres.

use homestead_domain::contacts::*;
use homestead_store::Store;
use homestead_store::contacts::ImportReport;
use sqlx::PgPool;

fn c(email: &str, interests: &[Interest]) -> NewContact {
    NewContact { name: String::new(), email: email.into(), interests: interests.to_vec(), source: "website".into() }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn every_interest_is_accepted(pool: PgPool) {
    let store = Store::from_pool(pool);
    assert_eq!(store.add_contact(c("a@b.example", Interest::ALL)).await.expect("add"), Some(true));
    assert_eq!(store.subscribed_contacts(None).await.expect("list")[0].interests.len(), Interest::ALL.len());
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn signing_up_twice_merges_interests(pool: PgPool) {
    let store = Store::from_pool(pool);
    store.add_contact(c("Kiki@Example.org", &[Interest::Workshops])).await.expect("1");
    assert_eq!(store.add_contact(c("kiki@example.org", &[Interest::CatAdoption])).await.expect("2"), Some(false));
    let all = store.subscribed_contacts(None).await.expect("list");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].interests, [Interest::CatAdoption, Interest::Workshops]);
    assert_eq!(store.subscribed_contacts(Some(Interest::CatAdoption)).await.expect("cats").len(), 1);
    assert!(store.subscribed_contacts(Some(Interest::Donating)).await.expect("donors").is_empty());
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn an_unsubscribe_survives_a_later_import(pool: PgPool) {
    let store = Store::from_pool(pool);
    store.add_contact(c("gone@example.org", &[Interest::Newsletter])).await.expect("add");
    assert!(store.unsubscribe_contact(" GONE@example.org").await.expect("unsub"));
    let report = store
        .import_contacts(vec![c("gone@example.org", &[Interest::Newsletter]), c("new@example.org", &[])])
        .await
        .expect("import");
    assert_eq!(report, ImportReport { added: 1, updated: 0, kept_unsubscribed: 1 });
    let emails: Vec<String> =
        store.subscribed_contacts(None).await.expect("list").into_iter().map(|c| c.email).collect();
    assert_eq!(emails, ["new@example.org"]);
    assert_eq!(store.add_contact(c("gone@example.org", &[])).await.expect("again"), None);
    assert_eq!(store.unsubscribed_count().await.expect("n"), 1);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn one_bad_row_rejects_the_whole_import(pool: PgPool) {
    let store = Store::from_pool(pool);
    let r = store.import_contacts(vec![c("ok@example.org", &[]), c("not an email", &[])]).await;
    assert!(r.is_err());
    assert!(store.subscribed_contacts(None).await.expect("list").is_empty());
}

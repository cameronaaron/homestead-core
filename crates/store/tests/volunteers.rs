//! Volunteers as stable people: name resolution, merging, and the
//! migration that turned typed names into records.

use homestead_domain::community::{NewVolunteerShift, VolunteerEdit, WorkingGroup};
use homestead_store::{MIGRATOR, Store, StoreError};
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::macros::date;

fn shift(name: &str, hours: i64) -> NewVolunteerShift {
    NewVolunteerShift {
        person_name: name.into(),
        worked_on: date!(2026 - 09 - 26),
        hours: Decimal::from(hours),
        working_group: WorkingGroup::FarmLabor,
        event_id: None,
        notes: String::new(),
    }
}

/// (name, hours this year, other names), in the order the page lists them.
async fn roster(store: &Store) -> Vec<(String, Decimal, Vec<String>)> {
    let all = store.list_volunteers(date!(2026 - 07 - 01), date!(2027 - 06 - 30)).await.expect("list");
    all.into_iter().map(|(v, h, _)| (v.name, h, v.other_names)).collect()
}

fn people(store_hours: &[homestead_domain::community::HoursByGroup]) -> i64 {
    store_hours.iter().map(|h| h.people).sum()
}

#[sqlx::test(migrations = false)]
async fn existing_names_become_one_volunteer_each(pool: PgPool) {
    MIGRATOR.run_to(54, &pool).await.expect("up to 0054");
    sqlx::raw_sql(
        "insert into homestead.volunteer_shifts (person_name, worked_on, hours, working_group) values
           ('Sam', '2026-09-01', 2, 'farm_labor'), (' sam ', '2026-09-02', 3, 'farm_labor'),
           ('Sam R.', '2026-09-03', 1, 'farm_labor');
         insert into homestead.volunteer_visits (person_name, in_at) values ('Ava', '2026-09-04 10:00Z');",
    )
    .execute(&pool)
    .await
    .expect("old rows");
    MIGRATOR.run(&pool).await.expect("0055 onward");

    let store = Store::from_pool(pool.clone());
    let got = roster(&store).await;
    let names: Vec<(&str, Decimal)> = got.iter().map(|(n, h, _)| (n.as_str(), *h)).collect();
    assert_eq!(
        names,
        [("Ava", Decimal::ZERO), ("Sam", Decimal::from(5)), ("Sam R.", Decimal::from(1))],
        "case and spaces were already one person; a different spelling is still another"
    );
    let orphans: i64 = sqlx::query_scalar("select count(*) from homestead.volunteer_visits where volunteer_id is null")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(orphans, 0);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_typed_name_finds_the_same_volunteer_everywhere(pool: PgPool) {
    let store = Store::from_pool(pool);
    let first = store.log_volunteer_shift(shift("Ricardo", 2)).await.expect("first");
    let again = store.log_volunteer_shift(shift("  ricardo ", 3)).await.expect("again");
    assert_eq!(first.volunteer_id, again.volunteer_id);
    assert_eq!(again.person_name, "ricardo", "the shift keeps the name as written");
    let visit = store.sign_in_at_gate("RICARDO", WorkingGroup::FarmLabor).await.expect("gate");
    assert_eq!(visit.volunteer_id, first.volunteer_id);
    let (_, gate_shift) = store.sign_out_at_gate("Ricardo").await.expect("out");
    assert_eq!(gate_shift.expect("a short visit is a shift").volunteer_id, first.volunteer_id);
    assert_eq!(roster(&store).await.len(), 1, "one person, not three");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn merging_moves_hours_and_keeps_the_old_name_working(pool: PgPool) {
    let store = Store::from_pool(pool);
    let sam = store.log_volunteer_shift(shift("Sam", 2)).await.expect("sam").volunteer_id;
    let sam_r = store.log_volunteer_shift(shift("Sam R.", 3)).await.expect("sam r").volunteer_id;
    store
        .update_volunteer(
            sam,
            VolunteerEdit {
                name: "Sam".into(),
                email: Some("sam@example.org".into()),
                phone: String::new(),
                active: true,
            },
        )
        .await
        .expect("email");
    let (from, to) = (date!(2026 - 07 - 01), date!(2027 - 06 - 30));
    assert_eq!(people(&store.volunteer_hours(from, to).await.expect("hours")), 2, "two until someone says otherwise");

    store.merge_volunteers(sam, sam_r).await.expect("merge");
    assert_eq!(roster(&store).await, [("Sam R.".to_owned(), Decimal::from(5), vec!["sam".to_owned()])]);
    assert_eq!(people(&store.volunteer_hours(from, to).await.expect("hours")), 1, "counted once");
    let all = store.list_volunteers(from, to).await.expect("list");
    assert_eq!(all[0].0.email.as_deref(), Some("sam@example.org"), "contact details carry over");

    // The next "Sam" at the gate is Sam R.
    assert_eq!(store.sign_in_at_gate("sam", WorkingGroup::FarmLabor).await.expect("in").volunteer_id, sam_r);

    let err = store.merge_volunteers(sam_r, sam_r).await.expect_err("self");
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
    let err = store.merge_volunteers(sam, sam_r).await.expect_err("gone");
    assert!(matches!(err, StoreError::NotFound("volunteer")), "{err:?}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn two_people_on_site_cant_be_merged(pool: PgPool) {
    let store = Store::from_pool(pool);
    let a = store.sign_in_at_gate("Ava", WorkingGroup::FarmLabor).await.expect("ava").volunteer_id;
    let b = store.sign_in_at_gate("Ava M.", WorkingGroup::FarmLabor).await.expect("ava m").volunteer_id;
    let err = store.merge_volunteers(a, b).await.expect_err("both in");
    assert!(matches!(&err, StoreError::Conflict(m) if m.contains("signed in")), "{err:?}");
    store.sign_out_at_gate("Ava").await.expect("out");
    store.merge_volunteers(a, b).await.expect("now it's fine");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn renaming_keeps_the_old_name_and_refuses_someone_elses(pool: PgPool) {
    let store = Store::from_pool(pool);
    let edit = |name: &str, active| VolunteerEdit { name: name.into(), email: None, phone: String::new(), active };
    let sam = store.log_volunteer_shift(shift("Sam", 1)).await.expect("sam").volunteer_id;
    store.log_volunteer_shift(shift("Ricardo", 1)).await.expect("ricardo");

    store.update_volunteer(sam, edit("Sam Reyes", false)).await.expect("rename, and gone quiet");
    let got = roster(&store).await;
    assert_eq!((got[1].0.as_str(), got[1].2.as_slice()), ("Sam Reyes", ["sam".to_owned()].as_slice()), "inactive last");

    let err = store.update_volunteer(sam, edit("ricardo", true)).await.expect_err("taken");
    assert!(matches!(&err, StoreError::Conflict(m) if m.contains("merge")), "{err:?}");
    assert_eq!(roster(&store).await[1].0, "Sam Reyes", "the refused rename changed nothing");

    // Turning up again makes them active.
    store.sign_in_at_gate("Sam", WorkingGroup::FarmLabor).await.expect("back");
    let all = store.list_volunteers(date!(2026 - 07 - 01), date!(2027 - 06 - 30)).await.expect("list");
    assert!(all.iter().all(|(v, _, _)| v.active));
    let err = store.update_volunteer(uuid::Uuid::nil(), edit("x", true)).await.expect_err("missing");
    assert!(matches!(err, StoreError::NotFound("volunteer")), "{err:?}");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn two_first_sign_ins_at_once_make_one_volunteer(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    // Hold a transaction that has resolved "Kiki" (made her) but not
    // committed, then start a second resolution of the same name: it must
    // wait for the first, then find her, rather than making a second Kiki
    // or failing on the name's primary key.
    let mut tx = pool.begin().await.expect("tx");
    sqlx::query("select pg_advisory_xact_lock(hashtext('volunteer:' || lower(btrim($1))))")
        .bind("Kiki")
        .execute(&mut *tx)
        .await
        .expect("lock");
    let id: uuid::Uuid = sqlx::query_scalar("insert into homestead.volunteers (name) values ('Kiki') returning id")
        .fetch_one(&mut *tx)
        .await
        .expect("insert");
    sqlx::query("insert into homestead.volunteer_names (name, volunteer_id) values ('kiki', $1)")
        .bind(id)
        .execute(&mut *tx)
        .await
        .expect("name");
    let second = tokio::spawn({
        let store = store.clone();
        async move { store.log_volunteer_shift(shift("kiki", 2)).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!second.is_finished(), "the second resolution waits for the first");
    tx.commit().await.expect("commit");
    let shift = second.await.expect("join").expect("second resolution");
    assert_eq!(shift.volunteer_id, id);
    assert_eq!(roster(&store).await.len(), 1);
}

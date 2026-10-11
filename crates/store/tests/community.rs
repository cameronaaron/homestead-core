//! Community events and volunteer hours against a real Postgres.

use homestead_domain::community::*;
use homestead_store::{Store, StoreError};
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::macros::date;

fn ev(title: &str, kind: EventKind, on: time::Date) -> NewCommunityEvent {
    NewCommunityEvent {
        title: title.into(),
        kind,
        held_on: on,
        time_note: String::new(),
        place: String::new(),
        description: String::new(),
    }
}

fn shift(who: &str, on: time::Date, hours: i64, group: WorkingGroup) -> NewVolunteerShift {
    NewVolunteerShift {
        person_name: who.into(),
        worked_on: on,
        hours: Decimal::from(hours),
        working_group: group,
        event_id: None,
        notes: String::new(),
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn every_event_kind_and_group_is_accepted(pool: PgPool) {
    let store = Store::from_pool(pool);
    for k in EventKind::ALL {
        store.create_community_event(ev("x", *k, date!(2026 - 09 - 26))).await.unwrap_or_else(|e| panic!("{k}: {e}"));
    }
    for g in WorkingGroup::ALL {
        store
            .log_volunteer_shift(shift("A", date!(2026 - 09 - 26), 1, *g))
            .await
            .unwrap_or_else(|e| panic!("{g}: {e}"));
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn upcoming_events_come_first_soonest_first(pool: PgPool) {
    let store = Store::from_pool(pool);
    let today = date!(2026 - 09 - 28);
    store.create_community_event(ev("Potluck", EventKind::Potluck, date!(2026 - 09 - 26))).await.expect("a");
    store.create_community_event(ev("CRFG talk", EventKind::Talk, date!(2026 - 11 - 24))).await.expect("b");
    store.create_community_event(ev("Work day", EventKind::WorkDay, date!(2026 - 10 - 04))).await.expect("c");
    store.create_community_event(ev("Old", EventKind::Other, date!(2026 - 08 - 01))).await.expect("d");
    let titles: Vec<String> =
        store.list_community_events(today).await.expect("list").into_iter().map(|e| e.title).collect();
    assert_eq!(titles, ["Work day", "CRFG talk", "Potluck", "Old"]);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn hours_sum_per_group_and_count_people_once(pool: PgPool) {
    let store = Store::from_pool(pool);
    let d = date!(2026 - 09 - 26);
    store.log_volunteer_shift(shift("Ricardo", d, 3, WorkingGroup::FarmLabor)).await.expect("1");
    store.log_volunteer_shift(shift(" ricardo ", d, 2, WorkingGroup::FarmLabor)).await.expect("2");
    store.log_volunteer_shift(shift("Randy", d, 4, WorkingGroup::FarmLabor)).await.expect("3");
    store.log_volunteer_shift(shift("Hola", d, 1, WorkingGroup::SocialMedia)).await.expect("4");
    store
        .log_volunteer_shift(shift("Randy", date!(2026 - 06 - 01), 8, WorkingGroup::FarmLabor))
        .await
        .expect("out of range");
    let got = store.volunteer_hours(date!(2026 - 09 - 01), date!(2026 - 09 - 30)).await.expect("hours");
    assert_eq!(
        got,
        [
            HoursByGroup { working_group: WorkingGroup::FarmLabor, hours: Decimal::from(9), people: 2 },
            HoursByGroup { working_group: WorkingGroup::SocialMedia, hours: Decimal::from(1), people: 1 },
        ]
    );
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_shift_for_a_missing_event_or_attendance_is_refused(pool: PgPool) {
    let store = Store::from_pool(pool);
    let mut s = shift("A", date!(2026 - 09 - 26), 1, WorkingGroup::Other);
    s.event_id = Some(uuid::Uuid::new_v4());
    assert!(matches!(store.log_volunteer_shift(s).await, Err(StoreError::NotFound("event"))));
    let e = store.create_community_event(ev("Potluck", EventKind::Potluck, date!(2026 - 09 - 26))).await.expect("e");
    assert!(store.set_attendance(e.id, -1).await.is_err());
    store.set_attendance(e.id, 14).await.expect("attendance");
    let listed = store.list_community_events(date!(2026 - 09 - 28)).await.expect("list");
    assert_eq!(listed[0].attendance, Some(14));
}

/// Moves an open visit's sign-in time back, as if they'd arrived earlier.
async fn arrived_ago(pool: &PgPool, who: &str, minutes: i64) {
    sqlx::query(
        "update homestead.volunteer_visits set in_at = now() - make_interval(mins => $2)
         where lower(person_name) = lower($1) and out_at is null",
    )
    .bind(who)
    .bind(i32::try_from(minutes).expect("small"))
    .execute(pool)
    .await
    .expect("backdate");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn the_gate_turns_a_visit_into_a_shift(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let first = store.sign_in_at_gate("Sam", WorkingGroup::CatClub).await.expect("in");
    // A second scan (any capitalisation) keeps the first arrival time.
    let again = store.sign_in_at_gate(" sam ", WorkingGroup::FarmLabor).await.expect("again");
    assert_eq!((again.id, again.in_at, again.working_group), (first.id, first.in_at, WorkingGroup::CatClub));
    store.sign_in_at_gate("Ava", WorkingGroup::FarmLabor).await.expect("Ava");
    assert_eq!(store.on_site().await.expect("site").len(), 2);

    arrived_ago(&pool, "Sam", 3 * 60 + 10).await;
    let (visit, shift) = store.sign_out_at_gate("SAM").await.expect("out");
    let shift = shift.expect("a shift");
    assert!(visit.out_at.is_some() && visit.shift_id == Some(shift.id));
    assert_eq!((shift.person_name.as_str(), shift.hours.to_string().as_str()), ("Sam", "3.25"));
    assert_eq!(shift.working_group, WorkingGroup::CatClub);
    assert_eq!(store.recent_volunteer_shifts(5).await.expect("shifts").len(), 1);

    // Signing out twice, or someone who never signed in, is refused.
    assert!(matches!(store.sign_out_at_gate("Sam").await, Err(StoreError::Conflict(_))));
    assert!(matches!(store.sign_out_at_gate("Nobody").await, Err(StoreError::Conflict(_))));

    // A forgotten sign-out closes the visit but invents no hours.
    arrived_ago(&pool, "Ava", 20 * 60).await;
    let (visit, shift) = store.sign_out_at_gate("Ava").await.expect("out");
    assert!(shift.is_none() && visit.out_at.is_some() && visit.shift_id.is_none());
    assert!(store.on_site().await.expect("site").is_empty());
    assert_eq!(store.recent_volunteer_shifts(5).await.expect("shifts").len(), 1);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn who_is_on_site_shows_whether_their_waiver_is_on_file(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    sqlx::query(
        "insert into homestead.release_requests
           (id, release_key, title, signer_name, signer_email, envelope_id, status, completed_at)
         values (gen_random_uuid(), 'liability-waiver', 'Waiver', 'Sam Lee', 's@example.org', 'e1', 'signed', now()),
                (gen_random_uuid(), 'liability-waiver', 'Waiver', 'Ava', 'a@example.org', 'e2', 'sent', null),
                (gen_random_uuid(), 'photo-release', 'Photos', 'Kim', 'k@example.org', 'e3', 'signed', now())",
    )
    .execute(&pool)
    .await
    .expect("releases");
    for who in ["sam lee", "Ava", "Kim"] {
        store.sign_in_at_gate(who, WorkingGroup::FarmLabor).await.expect("in");
    }
    let site: Vec<(String, bool)> =
        store.on_site().await.expect("site").into_iter().map(|(v, w)| (v.person_name, w)).collect();
    // Signed waiver: yes. Sent but unsigned: no. A photo release isn't a waiver.
    assert_eq!(site, [("sam lee".into(), true), ("Ava".into(), false), ("Kim".into(), false)]);
}

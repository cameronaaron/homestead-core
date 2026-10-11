//! The fiscal-year report: derived from dated records, split by unit,
//! bounded by the farm's own calendar, and only the sections asked for.

use homestead_domain::reports::{Count, FiscalYear, Measure, ReportSection};
use homestead_store::Store;
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::macros::date;

async fn seed(pool: &PgPool) {
    for sql in [
        // Volunteers: two in the year, one the day before it began.
        // Shifts count people by volunteer, not by how the name was typed:
        // "Sam" and "Sam R." are one volunteer here (as after a merge).
        "insert into homestead.volunteers (id, name) values
           ('00000000-0000-0000-0000-0000000000e1', 'Sam R.'), ('00000000-0000-0000-0000-0000000000e2', 'Ana')",
        "insert into homestead.volunteer_shifts (volunteer_id, person_name, worked_on, hours, working_group) values
           ('00000000-0000-0000-0000-0000000000e1', 'Sam', '2026-07-01', 3, 'farm_labor'),
           ('00000000-0000-0000-0000-0000000000e1', 'Sam R.', '2026-08-01', 2.5, 'farm_labor'),
           ('00000000-0000-0000-0000-0000000000e2', 'Ana', '2026-09-01', 4, 'cat_club'),
           ('00000000-0000-0000-0000-0000000000e2', 'Ana', '2026-06-30', 8, 'cat_club')",
        "insert into homestead.community_events (title, kind, held_on, attendance) values
           ('Seed swap', 'workshop', '2026-08-10', 14), ('Work day', 'work_day', '2026-08-20', null),
           ('Old potluck', 'potluck', '2026-06-01', 40)",
        // Kale in two units, in the year; beans before it.
        "insert into homestead.locations (id, name, kind) values ('00000000-0000-0000-0000-00000000000a', 'Bed 1', 'bed')",
        "insert into homestead.plantings (id, location_id, crop, sown_on) values
           ('00000000-0000-0000-0000-0000000000b1', '00000000-0000-0000-0000-00000000000a', 'Kale', '2026-03-01'),
           ('00000000-0000-0000-0000-0000000000b2', '00000000-0000-0000-0000-00000000000a', 'Beans', '2026-03-01')",
        "insert into homestead.harvests (planting_id, harvested_on, quantity, unit) values
           ('00000000-0000-0000-0000-0000000000b1', '2026-07-05', 10, 'lb'),
           ('00000000-0000-0000-0000-0000000000b1', '2026-07-12', 2, 'lb'),
           ('00000000-0000-0000-0000-0000000000b1', '2026-07-12', 3, 'bunch'),
           ('00000000-0000-0000-0000-0000000000b2', '2026-06-20', 9, 'lb')",
        "insert into homestead.stand_sales (sold_on, item, quantity, unit, amount) values
           ('2026-07-06', 'Eggs', 2, 'dozen', 12), ('2026-07-07', 'Kale', 1, 'bunch', 4.50)",
        "insert into homestead.gifts (donor_name, kind, received_on, amount) values
           ('A', 'cash', '2026-07-02', 50), ('B', 'sponsorship', '2026-07-03', 500), ('C', 'cash', '2026-05-01', 999)",
        "insert into homestead.gifts (donor_name, kind, received_on, description, estimated_value) values
           ('D', 'in_kind', '2026-07-04', 'Lumber', 120), ('E', 'in_kind', '2026-07-04', 'Seedlings', null)",
        // A resident since 2024, a cat that arrived and was adopted this
        // year, and one adopted last year (not assisted this year).
        "insert into homestead.animals (id, species, name, head_count, status, arrived_on) values
           ('00000000-0000-0000-0000-0000000000c1', 'chicken', 'Flock A', 6, 'resident', '2024-04-01'),
           ('00000000-0000-0000-0000-0000000000c2', 'cat', 'Mochi', 1, 'adopted', '2026-07-10'),
           ('00000000-0000-0000-0000-0000000000c3', 'cat', 'Pip', 1, 'adopted', '2025-01-01')",
        "insert into homestead.animal_placements (animal_id, kind, person_name, started_on) values
           ('00000000-0000-0000-0000-0000000000c2', 'adoption', 'Lee', '2026-09-01'),
           ('00000000-0000-0000-0000-0000000000c3', 'adoption', 'Kim', '2025-03-01')",
        "insert into homestead.egg_days (animal_id, on_date, collected, damaged) values
           ('00000000-0000-0000-0000-0000000000c1', '2026-07-01', 5, 1)",
        "insert into homestead.inventory_items (id, name, unit) values ('00000000-0000-0000-0000-0000000000d1', 'Layer feed', 'bag')",
        // 23:30 on June 30 in Los Angeles is July 1 in UTC: last year's.
        "insert into homestead.stock_movements (item_id, delta, reason, kind, at) values
           ('00000000-0000-0000-0000-0000000000d1', 10, 'count', 'adjustment', '2026-06-01 12:00-07'),
           ('00000000-0000-0000-0000-0000000000d1', -2, 'fed', 'consumption', '2026-06-30 23:30-07'),
           ('00000000-0000-0000-0000-0000000000d1', -3, 'fed', 'consumption', '2026-07-01 00:30-07'),
           ('00000000-0000-0000-0000-0000000000d1', -1, 'wet', 'waste', '2026-08-01 12:00-07')",
        "insert into homestead.incidents (occurred_at, kind, description, reported_by_name) values
           ('2026-07-15 10:00-07', 'near_miss', 'Ladder slipped', 'Sam')",
    ] {
        sqlx::query(sql).execute(pool).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
}

fn m(key: &str, unit: &str, q: &str) -> Measure {
    Measure { key: key.into(), unit: unit.into(), quantity: q.parse().expect("decimal") }
}

fn c(key: &str, count: i64) -> Count {
    Count { key: key.into(), count }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_year_is_counted_from_its_own_records(pool: PgPool) {
    seed(&pool).await;
    let store = Store::from_pool(pool).with_timezone("America/Los_Angeles").await.expect("tz");
    let fy = FiscalYear::starting(2026).expect("fy");
    let r = store.fiscal_report(fy, date!(2026 - 09 - 28), ReportSection::ALL).await.expect("report");

    let v = r.volunteers.expect("volunteers");
    assert_eq!((v.hours, v.shifts, v.people), (Decimal::new(95, 1), 3, 2), "'Sam' and 'Sam R.' are one volunteer");
    let e = r.events.expect("events");
    assert_eq!((e.held, e.attendance, e.without_attendance), (2, 14, 1));

    let h = r.harvest.expect("harvest");
    assert_eq!(h.harvested, vec![m("Kale", "bunch", "3"), m("Kale", "lb", "12")], "never lb + bunch");

    let s = r.stand.expect("stand");
    assert_eq!((s.revenue, s.sales), ("16.50".parse().expect("d"), 2));
    let g = r.gifts.expect("gifts");
    assert_eq!((g.cash.amount, g.cash_count), (Decimal::from(50), 1), "May's gift is last year's");
    assert_eq!((g.sponsorship.amount, g.in_kind_count, g.in_kind_unvalued), (Decimal::from(500), 2, 1));
    assert_eq!(g.in_kind_estimated, Decimal::from(120));

    let a = r.animals.expect("animals");
    assert_eq!(a.assisted, vec![c("cat", 1), c("chicken", 6)], "Pip left last year");
    assert_eq!(a.intake, vec![c("cat", 1)]);
    assert_eq!(a.placements, vec![c("adoption", 1)]);
    assert_eq!((a.eggs_collected, a.eggs_damaged), (5, 1));

    let i = r.inventory.expect("inventory");
    assert_eq!(i.consumed, vec![m("Layer feed", "bag", "3")], "the farm's July 1, not UTC's; usage is positive");
    assert_eq!(i.wasted, vec![m("Layer feed", "bag", "1")]);
    assert!(i.adjusted.is_empty(), "June's count was last year");

    let inc = r.incidents.expect("incidents");
    assert_eq!((inc.reported, inc.still_open, inc.by_kind), (1, 1, vec![c("near_miss", 1)]));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn only_the_sections_asked_for_are_filled(pool: PgPool) {
    seed(&pool).await;
    let store = Store::from_pool(pool);
    let fy = FiscalYear::starting(2026).expect("fy");
    let r = store.fiscal_report(fy, fy.last_day(), &[ReportSection::Harvest]).await.expect("report");
    assert!(r.harvest.is_some());
    assert!(r.gifts.is_none() && r.incidents.is_none() && r.volunteers.is_none() && r.costs.is_none());
    assert!(r.rows().iter().all(|row| row[0] == "harvest"));
}

//! Weather and irrigation: manual records first-class, sensor retries safe.

use homestead_domain::water::{IrrigationMethod, MANUAL, NewIrrigation, NewWeather};
use homestead_domain::{LocationKind, NewLocation, NewPlanting};
use homestead_store::{Store, StoreError};
use rust_decimal::Decimal;
use sqlx::PgPool;
use time::macros::{date, datetime};

fn d(s: &str) -> Decimal {
    s.parse().expect("decimal")
}

fn rain(source: &str, mm: &str) -> NewWeather {
    NewWeather {
        observed_at: datetime!(2026-09-28 14:00 UTC),
        source: source.into(),
        location_id: None,
        temperature_c: None,
        precipitation_mm: Some(d(mm)),
        wind_kph: None,
        humidity_pct: None,
        conditions: String::new(),
        raw: None,
        soil_temperature_c: None,
        frost: false,
    }
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_station_retrying_never_overwrites_or_duplicates(pool: PgPool) {
    let store = Store::from_pool(pool.clone()).with_timezone("America/Los_Angeles").await.expect("tz");
    let mine = store.record_weather(rain(MANUAL, "12.5")).await.expect("manual gauge");
    let station = store.record_weather(rain("station:backyard", "11.9")).await.expect("station");
    let retry = store.record_weather(rain("station:backyard", "99")).await.expect("retried upload");
    assert_eq!((retry.id, retry.precipitation_mm), (station.id, Some(d("11.9"))), "first report stands");
    assert_ne!(mine.id, station.id, "a person and a station at the same instant are both kept");
    let n: i64 =
        sqlx::query_scalar("select count(*) from homestead.weather_records").fetch_one(&pool).await.expect("n");
    assert_eq!(n, 2);
    // Totals stay per source: the same rain isn't counted twice.
    assert_eq!(
        store.rain_since(date!(2026 - 09 - 01)).await.expect("rain"),
        vec![(MANUAL.to_owned(), d("12.50")), ("station:backyard".to_owned(), d("11.90"))]
    );
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn watering_a_planting_records_where_it_is(pool: PgPool) {
    let store = Store::from_pool(pool.clone()).with_timezone("America/Los_Angeles").await.expect("tz");
    let loc = |n: &str| NewLocation::named(n, LocationKind::Bed);
    let (bed, other) = (
        store.create_location(loc("Bed 4")).await.expect("bed"),
        store.create_location(loc("Bed 5")).await.expect("b5"),
    );
    let planting = store
        .create_planting(NewPlanting {
            location_id: bed.id,
            crop: "Squash".into(),
            variety: String::new(),
            sown_on: date!(2026 - 06 - 01),
            expected_harvest_on: None,
            notes: String::new(),
            seed_lot_id: None,
            seed_quantity_used: None,
            ..NewPlanting::at(bed.id, "", date!(2026 - 01 - 01))
        })
        .await
        .expect("planting");
    // 7 am on the farm in September is 14:00 UTC (PDT).
    let start = store.farm_time(datetime!(2026-09-28 07:00)).await.expect("farm time");
    assert_eq!(start, datetime!(2026-09-28 14:00 UTC));
    let run = |location: Option<uuid::Uuid>, source: &str, ext: Option<&str>| NewIrrigation {
        location_id: location,
        planting_id: Some(planting.id),
        started_at: start,
        ended_at: Some(start + time::Duration::minutes(40)),
        water_liters: Some(d("75.708")),
        method: IrrigationMethod::Drip,
        notes: String::new(),
        source: source.into(),
        external_id: ext.map(str::to_owned),
        zone_id: None,
        problem: None,
    };
    let r = store.record_irrigation(run(None, MANUAL, None)).await.expect("watered");
    assert_eq!(
        (r.location_name.as_str(), r.planting_crop.as_deref(), r.minutes()),
        ("Bed 4", Some("Squash"), Some(40))
    );
    assert!(matches!(store.record_irrigation(run(Some(other.id), MANUAL, None)).await, Err(StoreError::Invalid(_))));

    let a = store.record_irrigation(run(None, "controller:zone3", Some("run-881"))).await.expect("controller");
    let b = store.record_irrigation(run(None, "controller:zone3", Some("run-881"))).await.expect("retry");
    assert_eq!(a.id, b.id);
    let totals = store.water_by_location_since(date!(2026 - 09 - 01)).await.expect("totals");
    assert_eq!(totals, vec![("Bed 4".to_owned(), d("151.416"), 2, 0)]);
}

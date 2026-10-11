//! Rules about the server's own source that a type can't express.

/// A calendar date must be the farm's (`store.today()`), not UTC's: at 5 pm
/// in Los Angeles UTC is already tomorrow, which once put the wrong date on
/// gift letters and moved evening entries into the next fiscal year. UTC
/// *instants* (lockouts, signature timestamps) are fine.
#[test]
fn calendar_dates_come_from_the_farms_time_zone() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    for entry in std::fs::read_dir(dir).expect("src") {
        let path = entry.expect("entry").path();
        let src = std::fs::read_to_string(&path).expect("read");
        for bad in ["now_utc().date()", "now_utc().year()", "now_utc().month()"] {
            assert!(!src.contains(bad), "{} uses {bad}; use store.today()", path.display());
        }
    }
}

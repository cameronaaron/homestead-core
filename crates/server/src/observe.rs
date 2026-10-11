//! Request counts and latency for `/api/v1/metrics`.
//!
//! One process, so plain atomics are the whole series. The scrape itself is
//! counted after the handler writes the body, so a scrape does not include
//! its own request.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;

const BOUNDS_MS: [u128; 7] = [10, 50, 100, 250, 500, 1000, 2500];
const LE: [&str; 8] = ["0.01", "0.05", "0.1", "0.25", "0.5", "1", "2.5", "+Inf"];

static CLASSES: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
static BUCKETS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
static COUNT: AtomicU64 = AtomicU64::new(0);
static SUM_US: AtomicU64 = AtomicU64::new(0);

const CLASS_NAMES: [&str; 5] = ["2xx", "3xx", "4xx", "5xx", "other"];

fn class_index(status: StatusCode) -> usize {
    match status.as_u16() {
        200..=299 => 0,
        300..=399 => 1,
        400..=499 => 2,
        500..=599 => 3,
        _ => 4,
    }
}

pub(crate) fn record(status: StatusCode, elapsed: Duration) {
    CLASSES[class_index(status)].fetch_add(1, Ordering::Relaxed);
    let ms = elapsed.as_millis();
    let mut idx = BOUNDS_MS.len();
    for (i, bound) in BOUNDS_MS.iter().enumerate() {
        if ms <= *bound {
            idx = i;
            break;
        }
    }
    for bucket in BUCKETS.iter().skip(idx) {
        bucket.fetch_add(1, Ordering::Relaxed);
    }
    COUNT.fetch_add(1, Ordering::Relaxed);
    SUM_US.fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
}

pub(crate) fn render() -> String {
    let mut out = String::from("# TYPE homestead_http_requests_total counter\n");
    for (i, name) in CLASS_NAMES.iter().enumerate() {
        out.push_str(&format!(
            "homestead_http_requests_total{{class=\"{name}\"}} {}\n",
            CLASSES[i].load(Ordering::Relaxed)
        ));
    }
    out.push_str("# TYPE homestead_http_request_duration_seconds histogram\n");
    for (i, le) in LE.iter().enumerate() {
        out.push_str(&format!(
            "homestead_http_request_duration_seconds_bucket{{le=\"{le}\"}} {}\n",
            BUCKETS[i].load(Ordering::Relaxed)
        ));
    }
    let count = COUNT.load(Ordering::Relaxed);
    let sum = SUM_US.load(Ordering::Relaxed) as f64 / 1_000_000.0;
    out.push_str(&format!(
        "homestead_http_request_duration_seconds_count {count}\n\
         homestead_http_request_duration_seconds_sum {sum}\n"
    ));
    out
}

pub(crate) async fn http_metrics(req: Request, next: Next) -> Response {
    let started = std::time::Instant::now();
    let response = next.run(req).await;
    record(response.status(), started.elapsed());
    response
}

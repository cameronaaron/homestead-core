//! The integration bus.
//!
//! A [`Sink`] is one outside system (Discord, Linear, ...). The [`Dispatcher`]
//! drains the outbox: it routes new events to the sinks that accept them,
//! leases due deliveries, runs them with a timeout, and settles each one as
//! delivered, retry-later, or dead. Sinks never see each other's failures.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use homestead_domain::Event;
use homestead_store::{Store, StoreError, outbox::Claimed};
use tokio::sync::watch;
use tokio::task::JoinSet;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeliveryError {
    /// Worth trying again later (network error, 5xx, rate limit).
    #[error("retryable: {0}")]
    Retryable(String),
    /// Will never succeed as-is (bad credentials, rejected payload).
    #[error("permanent: {0}")]
    Permanent(String),
}

pub trait Sink: Send + Sync + 'static {
    /// Stable identifier, stored in `deliveries.sink`. Renaming a sink
    /// orphans its pending deliveries (they dead-letter as unconfigured).
    fn name(&self) -> &'static str;

    /// Whether this sink wants the event at all. Decided once, at fan-out.
    fn accepts(&self, event: &Event) -> bool;

    /// Delivers one event. May be called more than once for the same
    /// `event_id` (at-least-once), so side effects should be idempotent or
    /// at least harmless to repeat.
    fn deliver<'a>(&'a self, event_id: i64, event: &'a Event) -> BoxFuture<'a, Result<(), DeliveryError>>;
}

/// Limits a sink to the event kinds configured for it (`homestead.toml`),
/// on top of whatever the sink itself accepts.
pub struct Routed {
    inner: Arc<dyn Sink>,
    kinds: std::collections::BTreeSet<String>,
}

impl Routed {
    pub fn new(inner: Arc<dyn Sink>, kinds: impl IntoIterator<Item = String>) -> Self {
        Self { inner, kinds: kinds.into_iter().collect() }
    }
}

impl Sink for Routed {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn accepts(&self, event: &Event) -> bool {
        self.kinds.contains(event.kind()) && self.inner.accepts(event)
    }

    fn deliver<'a>(&'a self, event_id: i64, event: &'a Event) -> BoxFuture<'a, Result<(), DeliveryError>> {
        self.inner.deliver(event_id, event)
    }
}

/// Exponential backoff: `base * 2^(n-1)` after the n-th failure, capped,
/// dead-lettering once `max_attempts` attempts have failed.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: i32,
    pub base: Duration,
    pub cap: Duration,
}

impl Default for RetryPolicy {
    /// 12 attempts spread over ~2.4 hours: rides out a typical outage of a
    /// chat or tracker API without hammering it.
    fn default() -> Self {
        Self { max_attempts: 12, base: Duration::from_secs(5), cap: Duration::from_secs(3600) }
    }
}

impl RetryPolicy {
    /// Delay before the next attempt, given how many attempts have now
    /// failed (≥ 1). `None` means give up.
    pub fn next_delay(&self, failed_attempts: i32) -> Option<Duration> {
        if failed_attempts >= self.max_attempts {
            return None;
        }
        let exp = u32::try_from(failed_attempts.saturating_sub(1)).unwrap_or(0).min(30);
        Some(self.base.saturating_mul(1u32 << exp).min(self.cap))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DispatcherConfig {
    pub batch: i64,
    /// How long a claimed delivery is reserved for this worker.
    pub lease: Duration,
    /// Upper bound on one `deliver` call; must be shorter than `lease` or a
    /// slow delivery could be claimed and run twice concurrently.
    pub delivery_timeout: Duration,
    /// Fallback wake-up when no NOTIFY arrives (e.g. behind a pooler).
    pub poll_interval: Duration,
    pub retry: RetryPolicy,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            batch: 50,
            lease: Duration::from_secs(120),
            delivery_timeout: Duration::from_secs(30),
            poll_interval: Duration::from_secs(5),
            retry: RetryPolicy::default(),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tick {
    pub fanned_out: usize,
    pub delivered: usize,
    pub failed: usize,
}

#[derive(Clone)]
pub struct Dispatcher {
    store: Store,
    sinks: Arc<Vec<Arc<dyn Sink>>>,
    config: DispatcherConfig,
}

impl Dispatcher {
    pub fn new(store: Store, sinks: Vec<Arc<dyn Sink>>, config: DispatcherConfig) -> Self {
        assert!(config.delivery_timeout < config.lease, "delivery_timeout must be shorter than the lease");
        let mut names: Vec<_> = sinks.iter().map(|s| s.name()).collect();
        names.sort_unstable();
        let n = names.len();
        names.dedup();
        assert_eq!(n, names.len(), "sink names must be unique");
        Self { store, sinks: Arc::new(sinks), config }
    }

    pub fn sink_names(&self) -> Vec<&'static str> {
        self.sinks.iter().map(|s| s.name()).collect()
    }

    /// One pass: route new events, then attempt every due delivery.
    pub async fn tick(&self) -> Result<Tick, StoreError> {
        let sinks = Arc::clone(&self.sinks);
        let route = move |e: &Event| sinks.iter().filter(|s| s.accepts(e)).map(|s| s.name()).collect();
        let mut tick = Tick::default();
        loop {
            let n = self.store.fan_out(self.config.batch, &route).await?;
            tick.fanned_out += n;
            if n < usize::try_from(self.config.batch).unwrap_or(usize::MAX) {
                break;
            }
        }

        let claimed = self.store.claim_due(self.config.batch, self.config.lease).await?;
        let mut running = JoinSet::new();
        for c in claimed {
            let this = self.clone();
            running.spawn(async move { this.attempt(c).await });
        }
        while let Some(res) = running.join_next().await {
            match res {
                Ok(Ok(true)) => tick.delivered += 1,
                Ok(Ok(false)) => tick.failed += 1,
                Ok(Err(e)) => return Err(e),
                // A panicking sink is a bug; its lease lapses and it retries.
                Err(join) => {
                    tracing::error!(error = %join, "sink task panicked");
                    tick.failed += 1;
                }
            }
        }
        Ok(tick)
    }

    /// Runs one claimed delivery and records the outcome. `Ok(true)` if delivered.
    async fn attempt(&self, c: Claimed) -> Result<bool, StoreError> {
        let Some(sink) = self.sinks.iter().find(|s| s.name() == c.sink) else {
            let msg = format!("sink '{}' is not configured on this server", c.sink);
            self.store.mark_failed(c.event_id, &c.sink, &msg, None).await?;
            return Ok(false);
        };
        let event = match Event::from_parts(&c.kind, c.payload) {
            Ok(e) => e,
            Err(e) => {
                let msg = format!("cannot decode {}: {e}", c.kind);
                self.store.mark_failed(c.event_id, &c.sink, &msg, None).await?;
                return Ok(false);
            }
        };
        let outcome = match tokio::time::timeout(self.config.delivery_timeout, sink.deliver(c.event_id, &event)).await {
            Ok(r) => r,
            Err(_) => Err(DeliveryError::Retryable(format!("timed out after {:?}", self.config.delivery_timeout))),
        };
        match outcome {
            Ok(()) => {
                self.store.mark_delivered(c.event_id, &c.sink).await?;
                tracing::debug!(event_id = c.event_id, sink = c.sink, "delivered");
                Ok(true)
            }
            Err(err) => {
                let retry_in = match &err {
                    DeliveryError::Retryable(_) => self.config.retry.next_delay(c.attempts + 1),
                    DeliveryError::Permanent(_) => None,
                };
                tracing::warn!(event_id = c.event_id, sink = c.sink, attempt = c.attempts + 1, ?retry_in, %err, "delivery failed");
                self.store.mark_failed(c.event_id, &c.sink, &err.to_string(), retry_in).await?;
                Ok(false)
            }
        }
    }

    /// Runs until `shutdown` flips to true. Wakes on NOTIFY or every
    /// `poll_interval`, whichever is first; store errors are logged and
    /// retried rather than killing the loop.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        let mut listener = match sqlx::postgres::PgListener::connect_with(self.store.pool()).await {
            Ok(mut l) => match l.listen("homestead_events").await {
                Ok(()) => Some(l),
                Err(e) => {
                    tracing::warn!(error = %e, "LISTEN failed; falling back to polling only");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "no listener connection; falling back to polling only");
                None
            }
        };
        tracing::info!(sinks = ?self.sink_names(), "dispatcher started");
        loop {
            match self.tick().await {
                Ok(t) if t != Tick::default() => tracing::info!(?t, "dispatch tick"),
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, "dispatch tick failed"),
            }
            let notified = async {
                match listener.as_mut() {
                    Some(l) => {
                        if let Err(e) = l.recv().await {
                            tracing::warn!(error = %e, "listener error");
                            tokio::time::sleep(self.config.poll_interval).await;
                        }
                    }
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = notified => {}
                _ = tokio::time::sleep(self.config.poll_interval) => {}
                _ = shutdown.changed() => {}
            }
            if *shutdown.borrow() {
                tracing::info!("dispatcher stopping");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_caps_and_gives_up() {
        let p = RetryPolicy { max_attempts: 5, base: Duration::from_secs(5), cap: Duration::from_secs(30) };
        let delays: Vec<_> = (1..=5).map(|n| p.next_delay(n).map(|d| d.as_secs())).collect();
        assert_eq!(delays, [Some(5), Some(10), Some(20), Some(30), None]);
    }

    #[test]
    fn default_policy_never_overflows() {
        let p = RetryPolicy { max_attempts: i32::MAX, ..RetryPolicy::default() };
        assert_eq!(p.next_delay(1_000_000), Some(p.cap));
    }
}

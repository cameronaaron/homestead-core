//! What's happening on Discord and social media, read live for the
//! Community page.
//!
//! Nothing here is stored: these are views of the other systems, cached for
//! a couple of minutes so a busy page doesn't spend Discord's or Buffer's
//! rate limits, and bounded in time so a slow API can't hang the page.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use homestead_integrations::buffer::{self, Social};
use homestead_integrations::discord::feed::{Reader, Snapshot};

/// How long a good read is reused.
const FRESH: Duration = Duration::from_secs(120);
/// How long a failed read is reused before trying again.
const RETRY: Duration = Duration::from_secs(30);
/// How long the page waits for a read.
const WAIT: Duration = Duration::from_secs(5);

/// A read and when it was made.
type Read<T> = (Instant, Result<Arc<T>, String>);

/// The last read of one source.
pub struct Cache<T> {
    slot: tokio::sync::Mutex<Option<Read<T>>>,
}

impl<T> Default for Cache<T> {
    fn default() -> Self {
        Self { slot: tokio::sync::Mutex::new(None) }
    }
}

impl<T> Cache<T> {
    /// The cached read if still fresh, else a new one. Holding the lock
    /// while reading means concurrent pages share one request.
    pub async fn get<F, Fut>(&self, read: F) -> Result<Arc<T>, String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, String>>,
    {
        let mut slot = self.slot.lock().await;
        if let Some((at, got)) = &*slot {
            let keep = if got.is_ok() { FRESH } else { RETRY };
            if at.elapsed() < keep {
                return got.clone();
            }
        }
        let got = match tokio::time::timeout(WAIT, read()).await {
            Ok(r) => r.map(Arc::new),
            Err(_) => Err(format!("no answer within {} seconds", WAIT.as_secs())),
        };
        if let Err(e) = &got {
            tracing::warn!(error = %e, "feed read failed");
        }
        *slot = Some((Instant::now(), got.clone()));
        got
    }
}

pub struct DiscordFeed {
    pub reader: Reader,
    /// Checked at startup to be readable by the whole community.
    pub channels: Vec<String>,
    pub cache: Cache<Snapshot>,
    /// Who holds which role, for the Releases page's onboarding check.
    pub holders: Cache<Vec<homestead_integrations::discord::feed::RoleHolder>>,
}

impl DiscordFeed {
    pub async fn read(&self) -> Result<Arc<Snapshot>, String> {
        self.cache.get(|| self.reader.snapshot(&self.channels, 5)).await
    }

    pub async fn role_holders(&self) -> Result<Arc<Vec<homestead_integrations::discord::feed::RoleHolder>>, String> {
        self.holders.get(|| self.reader.role_holders()).await
    }
}

pub struct SocialFeed {
    pub client: buffer::Client,
    pub organization_id: String,
    pub cache: Cache<Social>,
}

impl SocialFeed {
    pub async fn read(&self) -> Result<Arc<Social>, String> {
        self.cache.get(|| self.client.social(&self.organization_id)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn a_fresh_read_is_reused_and_a_hung_one_gives_up() {
        let cache = Cache::<u32>::default();
        let calls = AtomicUsize::new(0);
        let read = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(7)
        };
        assert_eq!(*cache.get(read).await.expect("first"), 7);
        assert_eq!(*cache.get(read).await.expect("cached"), 7);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "the second page view reuses the read");

        tokio::time::pause();
        let hung = Cache::<u32>::default();
        let err = hung.get(std::future::pending::<Result<u32, String>>).await.expect_err("timeout");
        assert!(err.contains("5 seconds"), "{err}");
    }
}

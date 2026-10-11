//! Adapters between the homestead core and outside systems.
//!
//! Outbound adapters implement [`homestead_bus::Sink`]; inbound ones (Discord
//! slash commands) expose verification and wire types for the server.

pub mod blobs;
pub mod buffer;
pub mod discord;
pub mod documenso;
pub mod github;
pub mod grants;
pub mod linear;
pub mod settings;
pub mod zeffy;

use homestead_bus::DeliveryError;

/// Outcome of checking a system that can't (fully) be written through its
/// API against its spec. Shared by the verify-only systems.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// The live system contradicts the spec in a way that matters: fail CI.
    Violation(String),
    /// A manual step hasn't been done yet: warn.
    Pending(String),
    /// Worth knowing, nothing to fix.
    Note(String),
}

/// Shared HTTP status → retry classification for outbound calls.
/// 408/425/429 and 5xx are transient; any other 4xx will not fix itself.
pub(crate) fn classify(status: reqwest::StatusCode, body: &str) -> Result<(), DeliveryError> {
    if status.is_success() {
        return Ok(());
    }
    let snippet: String = body.chars().take(300).collect();
    let msg = format!("HTTP {status}: {snippet}");
    if status.is_server_error() || matches!(status.as_u16(), 408 | 425 | 429) {
        Err(DeliveryError::Retryable(msg))
    } else {
        Err(DeliveryError::Permanent(msg))
    }
}

/// Transport failures (DNS, connect, reset, timeout) are always transient.
pub(crate) fn transport(e: reqwest::Error) -> DeliveryError {
    DeliveryError::Retryable(format!("transport: {e}"))
}

/// Builds the shared outbound client with a bounded per-request timeout.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent(concat!("sunbrewed-homestead/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("static reqwest config is valid")
}

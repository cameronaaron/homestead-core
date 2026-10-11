//! Zeffy webhook receiver. Verification follows Zeffy's published contract:
//! HMAC-SHA256 over `{timestamp}.{raw body}`, with a five-minute replay limit.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::AppState;

const MAX_AGE_SECONDS: i64 = 300;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    id: Uuid,
    #[serde(rename = "type")]
    event_type: String,
    version: i32,
    dispatched_at: OffsetDateTime,
    data: serde_json::Value,
}

fn verify(secret: &[u8], header: &str, body: &[u8], now: i64) -> Result<(), &'static str> {
    let mut timestamp = None;
    let mut signature = None;
    for part in header.split(',') {
        let (name, value) = part.trim().split_once('=').ok_or("malformed signature")?;
        match name {
            "t" => timestamp = value.parse::<i64>().ok(),
            "v1" => signature = hex::decode(value).ok(),
            _ => {}
        }
    }
    let timestamp = timestamp.ok_or("missing timestamp")?;
    if now.abs_diff(timestamp) > MAX_AGE_SECONDS as u64 {
        return Err("stale signature");
    }
    let supplied = signature.ok_or("missing signature")?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).map_err(|_| "invalid secret")?;
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    let expected = mac.finalize().into_bytes();
    if !bool::from(expected[..].ct_eq(&supplied)) {
        return Err("invalid signature");
    }
    Ok(())
}

pub async fn webhook(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(secret) = state.zeffy_webhook_secret.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(signature) = headers.get("zeffy-signature").and_then(|v| v.to_str().ok()) else {
        return (StatusCode::UNAUTHORIZED, "missing Zeffy-Signature").into_response();
    };
    if let Err(reason) = verify(secret.as_bytes(), signature, &body, OffsetDateTime::now_utc().unix_timestamp()) {
        return (StatusCode::UNAUTHORIZED, reason).into_response();
    }
    let envelope: Envelope = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid webhook payload").into_response(),
    };
    if !matches!(
        envelope.event_type.as_str(),
        "payment.created" | "payment.completed" | "payment.updated" | "payment.deleted"
    ) {
        return (StatusCode::UNPROCESSABLE_ENTITY, "unsupported Zeffy event type").into_response();
    }
    match state
        .system_store
        .record_zeffy_event(envelope.id, &envelope.event_type, envelope.version, envelope.dispatched_at, envelope.data)
        .await
    {
        Ok(inserted) => (StatusCode::OK, if inserted { "recorded" } else { "duplicate" }).into_response(),
        Err(error) => {
            tracing::error!(%error, "recording Zeffy webhook failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_are_exact_and_expire() {
        let body = br#"{"id":"event"}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"secret").expect("key");
        mac.update(b"100.");
        mac.update(body);
        let header = format!("t=100,v1={}", hex::encode(mac.finalize().into_bytes()));
        assert_eq!(verify(b"secret", &header, body, 399), Ok(()));
        assert_eq!(verify(b"secret", &header, body, 401), Err("stale signature"));
        assert_eq!(verify(b"secret", &header, b"changed", 100), Err("invalid signature"));
    }
}

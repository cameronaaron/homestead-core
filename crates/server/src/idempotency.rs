//! Exactly-once HTTP responses for authenticated API writes.

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use homestead_store::IdempotencyClaim;
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};

use crate::AppState;

pub async fn enforce(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if matches!(*request.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(request).await;
    }
    let Some(key) = request.headers().get("idempotency-key").and_then(|v| v.to_str().ok()).map(str::to_owned) else {
        return (StatusCode::BAD_REQUEST, "Idempotency-Key is required for API writes").into_response();
    };
    if key.is_empty() || key.len() > 200 {
        return (StatusCode::BAD_REQUEST, "Idempotency-Key must contain 1 to 200 characters").into_response();
    }
    let (parts, body) = request.into_parts();
    let body = match body.collect().await {
        Ok(v) => v.to_bytes(),
        Err(_) => return (StatusCode::BAD_REQUEST, "could not read request body").into_response(),
    };
    // Keys are the caller's own namespace: two services picking the same key
    // must never replay each other's responses.
    let caller = parts.extensions.get::<crate::auth::Principal>().map(|p| p.actor.id.to_string()).unwrap_or_default();
    let scope = format!("{caller} {} {}", parts.method, parts.uri.path());
    let mut hasher = Sha256::new();
    hasher.update(parts.method.as_str());
    hasher.update(b"\0");
    hasher.update(parts.uri.to_string());
    hasher.update(b"\0");
    hasher.update(&body);
    let hash = hex::encode(hasher.finalize());
    match state.system_store.claim_idempotency(&scope, &key, &hash).await {
        Ok(IdempotencyClaim::Replay { status, body }) => {
            let status = StatusCode::from_u16(status as u16).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, [("idempotency-replayed", "true")], Bytes::from(body)).into_response()
        }
        Ok(IdempotencyClaim::InProgress) => {
            (StatusCode::CONFLICT, "request with this key is still in progress").into_response()
        }
        Err(error) => crate::status_of(&error).into_response(),
        Ok(IdempotencyClaim::Proceed) => {
            let request = Request::from_parts(parts, Body::from(body));
            let response = next.run(request).await;
            let (parts, body) = response.into_parts();
            let bytes = match body.collect().await {
                Ok(v) => v.to_bytes(),
                Err(error) => {
                    tracing::error!(%error, "could not retain idempotent response");
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            };
            if let Err(error) =
                state.system_store.finish_idempotency(&scope, &key, parts.status.as_u16() as i16, &bytes).await
            {
                tracing::error!(%error, "could not finish idempotency record");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            Response::from_parts(parts, Body::from(bytes))
        }
    }
}

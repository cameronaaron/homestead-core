//! Signed-in test callers. Sessions are seeded straight into the store
//! with fixed tokens, so request builders can stay synchronous.
#![allow(dead_code)] // each test binary uses a different subset

use homestead_domain::auth::{ActorKind, ActorRecord, NewActor, Role};
use homestead_server::auth::hash_secret;
use homestead_store::{Actor, Store};
use sqlx::PgPool;
use time::Duration;

/// The steward's session cookie value in every test.
pub const STEWARD: &str = "5e55105e55105e55105e55105e55105e55105e55105e55105e55105e55105e55";

/// Adds a person with `roles` and a live session whose cookie is `token`.
pub async fn person(pool: &PgPool, name: &str, roles: &[Role], token: &str) -> ActorRecord {
    let store = Store::from_pool(pool.clone());
    let actor = store
        .create_actor(NewActor {
            display_name: name.into(),
            email: Some(format!("{}@example.org", name.to_lowercase().replace(' ', "."))),
            kind: ActorKind::Human,
            roles: roles.to_vec(),
        })
        .await
        .expect("person");
    store
        .acting_as(Actor { id: Some(actor.id), label: name.into() })
        .start_session(actor.id, &hash_secret(token), Duration::days(1))
        .await
        .expect("session");
    actor
}

pub async fn steward(pool: &PgPool) -> ActorRecord {
    person(pool, "Kiki", &[Role::Steward], STEWARD).await
}

/// A service account with an API token `token`.
pub async fn service(pool: &PgPool, name: &str, token: &str) -> ActorRecord {
    let store = Store::from_pool(pool.clone());
    let actor = store
        .create_actor(NewActor {
            display_name: name.into(),
            email: None,
            kind: ActorKind::Service,
            roles: vec![Role::Service],
        })
        .await
        .expect("service");
    store.create_api_token(actor.id, name, &hash_secret(token)).await.expect("token");
    actor
}

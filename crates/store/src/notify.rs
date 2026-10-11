//! Per-person notification choices.
//!
//! `today` is on unless they turned it off. `discord` is off unless they
//! turned it on, and a ping still requires a linked Discord user id plus
//! the capability to see that kind of record.

use std::collections::HashMap;

use homestead_domain::auth::{CapabilitySet, Role};
use homestead_domain::{Event, ValidationError};
use uuid::Uuid;

use crate::{Result, Store, parse};

pub const CHANNELS: &[&str] = &["today", "discord"];

fn check(channel: &str, kind: &str) -> Result<()> {
    if !CHANNELS.contains(&channel) || !Event::KINDS.contains(&kind) {
        return Err(ValidationError::Malformed("notification").into());
    }
    Ok(())
}

impl Store {
    pub async fn notification_prefs(&self, actor_id: Uuid) -> Result<HashMap<(String, String), bool>> {
        let rows: Vec<(String, String, bool)> =
            sqlx::query_as("select channel, kind, enabled from homestead.notification_preferences where actor_id = $1")
                .bind(actor_id)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(|(c, k, on)| ((c, k), on)).collect())
    }

    pub async fn set_notification(&self, actor_id: Uuid, channel: &str, kind: &str, enabled: bool) -> Result<()> {
        check(channel, kind)?;
        let mut tx = self.begin().await?;
        sqlx::query(
            "insert into homestead.notification_preferences (actor_id, channel, kind, enabled)
             values ($1, $2, $3, $4)
             on conflict (actor_id, channel, kind) do update set enabled = excluded.enabled",
        )
        .bind(actor_id)
        .bind(channel)
        .bind(kind)
        .bind(enabled)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Recent event lines for Today. Kinds they cannot see are left out,
    /// and a `today` preference of false hides that kind. No row means on.
    pub async fn notices_for(&self, actor_id: Uuid, caps: CapabilitySet, limit: i64) -> Result<Vec<String>> {
        let prefs = self.notification_prefs(actor_id).await?;
        let events = self.recent_events(80).await?;
        let mut out = Vec::new();
        for event in events {
            let Some(cap) = Event::audience(&event.kind) else { continue };
            if !caps.has(cap) {
                continue;
            }
            if prefs.get(&("today".into(), event.kind.clone())) == Some(&false) {
                continue;
            }
            let Ok(decoded) = Event::from_parts(&event.kind, event.payload) else { continue };
            out.push(decoded.summary());
            if out.len() as i64 >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Discord snowflakes to ping for `kind`: people who opted in, are
    /// linked, and can see that kind of record.
    pub async fn discord_mentions(&self, kind: &str) -> Result<Vec<String>> {
        let Some(need) = Event::audience(kind) else { return Ok(Vec::new()) };
        let rows: Vec<(String, Vec<String>)> = sqlx::query_as(
            "select a.external_subject,
                    coalesce((select array_agg(r.role) from homestead.actor_roles r where r.actor_id = a.id), '{}')
             from homestead.actors a
             join homestead.notification_preferences p on p.actor_id = a.id
             where p.channel = 'discord' and p.kind = $1 and p.enabled
               and a.disabled_at is null
               and a.external_subject like 'discord:%'",
        )
        .bind(kind)
        .fetch_all(&self.pool)
        .await?;
        let mut ids = Vec::new();
        for (subject, roles) in rows {
            let roles = roles.iter().map(|r| parse::<Role>(r)).collect::<Result<Vec<_>>>()?;
            if !CapabilitySet::for_roles(&roles).has(need) {
                continue;
            }
            if let Some(id) = subject.strip_prefix("discord:")
                && !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_digit())
            {
                ids.push(id.to_owned());
            }
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }
}

//! People, service accounts, and their credentials.
//!
//! Secrets never reach this crate in the clear: sessions, setup links and
//! API tokens are looked up by the SHA-256 of the presented value, and
//! passwords arrive already hashed (argon2id, in the server).

use homestead_domain::auth::{ActorKind, ActorRecord, NewActor, Role};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{Result, Store, StoreError, Tx, parse};

/// Failed sign-ins in a row before an account is locked for a while.
pub const LOCKOUT_AFTER: i32 = 10;
pub const LOCKOUT_FOR: Duration = Duration::minutes(15);

macro_rules! actor_cols {
    () => {
        "a.id, a.display_name, a.email, a.kind, a.external_subject, a.disabled_at, a.welcomed_at,
         (c.actor_id is not null) as has_password,
         coalesce((select array_agg(r.role order by r.role) from homestead.actor_roles r where r.actor_id = a.id),
                  '{}') as roles"
    };
}

#[derive(sqlx::FromRow)]
struct ActorRow {
    id: Uuid,
    display_name: String,
    email: Option<String>,
    kind: String,
    external_subject: Option<String>,
    disabled_at: Option<OffsetDateTime>,
    welcomed_at: Option<OffsetDateTime>,
    has_password: bool,
    roles: Vec<String>,
}

impl TryFrom<ActorRow> for ActorRecord {
    type Error = StoreError;
    fn try_from(r: ActorRow) -> Result<Self> {
        let mut roles = r.roles.iter().map(|x| parse::<Role>(x)).collect::<Result<Vec<_>>>()?;
        roles.sort_by_key(|x| Role::ALL.iter().position(|y| y == x));
        Ok(ActorRecord {
            id: r.id,
            display_name: r.display_name,
            email: r.email,
            kind: parse(&r.kind)?,
            roles,
            external_subject: r.external_subject,
            has_password: r.has_password,
            disabled_at: r.disabled_at,
            welcomed_at: r.welcomed_at,
        })
    }
}

/// What a sign-in attempt needs to know about the account.
pub struct LoginCandidate {
    pub actor: ActorRecord,
    pub password_hash: String,
    pub locked_until: Option<OffsetDateTime>,
}

/// Appends an explicit audit entry (for actions that aren't a row change,
/// like signing in). Attribution comes from the transaction's settings.
pub(crate) async fn audit(
    tx: &mut Tx<'_>,
    action: &str,
    resource_type: &str,
    resource_id: Option<String>,
    metadata: serde_json::Value,
) -> Result<()> {
    sqlx::query(
        "insert into homestead.audit_entries (actor_label, actor_id, action, resource_type, resource_id, metadata)
         values (coalesce(nullif(current_setting('homestead.actor', true), ''), 'system'),
                 nullif(current_setting('homestead.actor_id', true), '')::uuid, $1, $2, $3, $4)",
    )
    .bind(action)
    .bind(resource_type)
    .bind(resource_id)
    .bind(metadata)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

impl Store {
    pub async fn create_actor(&self, input: NewActor) -> Result<ActorRecord> {
        let a = input.validated()?;
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.actors (display_name, email, kind) values ($1, $2, $3) returning id",
        )
        .bind(&a.display_name)
        .bind(&a.email)
        .bind(a.kind.as_str())
        .fetch_one(&mut *tx)
        .await?;
        grant_roles(&mut tx, id, &a.roles, self.actor().and_then(|x| x.id)).await?;
        tx.commit().await?;
        self.get_actor(id).await
    }

    pub async fn get_actor(&self, id: Uuid) -> Result<ActorRecord> {
        let row: Option<ActorRow> = sqlx::query_as(concat!(
            "select ",
            actor_cols!(),
            " from homestead.actors a left join homestead.actor_credentials c on c.actor_id = a.id where a.id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.ok_or(StoreError::NotFound("person"))?.try_into()
    }

    pub async fn list_actors(&self) -> Result<Vec<ActorRecord>> {
        let rows: Vec<ActorRow> = sqlx::query_as(concat!(
            "select ",
            actor_cols!(),
            " from homestead.actors a left join homestead.actor_credentials c on c.actor_id = a.id
             where a.external_subject is distinct from 'admin-token'
             order by a.disabled_at is not null, a.kind, a.display_name"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Replaces someone's roles. A steward can't remove their own steward
    /// role, so the organization can't lock itself out by accident.
    pub async fn set_roles(&self, id: Uuid, roles: &[Role]) -> Result<ActorRecord> {
        let current = self.get_actor(id).await?;
        let candidate = NewActor {
            display_name: current.display_name.clone(),
            email: current.email.clone(),
            kind: current.kind,
            roles: roles.to_vec(),
        }
        .validated()?;
        if self.actor().is_some_and(|me| me.id == Some(id))
            && current.roles.contains(&Role::Steward)
            && !candidate.roles.contains(&Role::Steward)
        {
            return Err(StoreError::Conflict("you can't remove your own steward role".into()));
        }
        let mut tx = self.begin().await?;
        sqlx::query("delete from homestead.actor_roles where actor_id = $1").bind(id).execute(&mut *tx).await?;
        grant_roles(&mut tx, id, &candidate.roles, self.actor().and_then(|x| x.id)).await?;
        tx.commit().await?;
        self.get_actor(id).await
    }

    /// Disabling signs the person out everywhere, revokes their tokens and
    /// puts their open tasks back up for grabs. Closed work keeps its
    /// historical assignee.
    pub async fn set_actor_disabled(&self, id: Uuid, disabled: bool) -> Result<()> {
        if disabled && self.actor().is_some_and(|me| me.id == Some(id)) {
            return Err(StoreError::Conflict("you can't disable yourself".into()));
        }
        let mut tx = self.begin().await?;
        let n = sqlx::query(
            "update homestead.actors set disabled_at = case when $2 then coalesce(disabled_at, now()) end
             where id = $1",
        )
        .bind(id)
        .bind(disabled)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if n == 0 {
            return Err(StoreError::NotFound("person"));
        }
        if disabled {
            sqlx::query("delete from homestead.sessions where actor_id = $1").bind(id).execute(&mut *tx).await?;
            sqlx::query("update homestead.api_tokens set revoked_at = coalesce(revoked_at, now()) where actor_id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("update homestead.tasks set assigned_to = null where assigned_to = $1 and status = 'open'")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Links (or, with `None`, unlinks) an outside identity such as
    /// `discord:<user id>`, so actions taken there are attributed.
    pub async fn set_external_subject(&self, id: Uuid, subject: Option<&str>) -> Result<()> {
        let subject = subject.map(str::trim).filter(|s| !s.is_empty());
        if subject.is_some_and(|s| s == "admin-token" || !s.contains(':')) {
            return Err(homestead_domain::ValidationError::Malformed("linked identity (like discord:1234)").into());
        }
        let mut tx = self.begin().await?;
        let n = sqlx::query("update homestead.actors set external_subject = $2 where id = $1")
            .bind(id)
            .bind(subject)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        if n == 0 { Err(StoreError::NotFound("person")) } else { Ok(()) }
    }

    pub async fn actor_by_external_subject(&self, subject: &str) -> Result<Option<ActorRecord>> {
        let row: Option<ActorRow> = sqlx::query_as(concat!(
            "select ",
            actor_cols!(),
            " from homestead.actors a left join homestead.actor_credentials c on c.actor_id = a.id
             where a.external_subject = $1"
        ))
        .bind(subject)
        .fetch_optional(&self.pool)
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    /// The service actor the legacy `ADMIN_TOKEN` acts as, created once.
    pub async fn admin_token_actor(&self) -> Result<ActorRecord> {
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.actors (display_name, kind, external_subject)
             values ('ADMIN_TOKEN (legacy API credential)', 'service', 'admin-token')
             on conflict (external_subject) do update set external_subject = excluded.external_subject
             returning id",
        )
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("insert into homestead.actor_roles (actor_id, role) values ($1, 'service') on conflict do nothing")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.get_actor(id).await
    }

    // ── Setup links and passwords ──────────────────────────────────────────

    /// Stores a one-time link for `actor_id` to set a password. Issuing one
    /// retires any earlier unused link for the same person.
    pub async fn issue_setup_link(&self, actor_id: Uuid, token_hash: &str, valid_for: Duration) -> Result<()> {
        let mut tx = self.begin().await?;
        let kind: String =
            sqlx::query_scalar("select kind from homestead.actors where id = $1 and disabled_at is null")
                .bind(actor_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::NotFound("active person"))?;
        if kind != ActorKind::Human.as_str() {
            return Err(StoreError::Conflict("services sign in with API tokens, not passwords".into()));
        }
        sqlx::query("update homestead.setup_links set used_at = now() where actor_id = $1 and used_at is null")
            .bind(actor_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "insert into homestead.setup_links (token_hash, actor_id, created_by, expires_at)
             values ($1, $2, $3, now() + $4)",
        )
        .bind(token_hash)
        .bind(actor_id)
        .bind(self.actor().and_then(|a| a.id))
        .bind(valid_for)
        .execute(&mut *tx)
        .await?;
        audit(&mut tx, "setup_link_issued", "actors", Some(actor_id.to_string()), serde_json::json!({})).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Who a setup link is for, if it's still usable.
    pub async fn setup_link_actor(&self, token_hash: &str) -> Result<Option<ActorRecord>> {
        let id: Option<Uuid> = sqlx::query_scalar(
            "select l.actor_id from homestead.setup_links l join homestead.actors a on a.id = l.actor_id
             where l.token_hash = $1 and l.used_at is null and l.expires_at > now() and a.disabled_at is null",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?;
        match id {
            Some(id) => Ok(Some(self.get_actor(id).await?)),
            None => Ok(None),
        }
    }

    /// Spends a setup link on a new password. Every existing session for the
    /// person ends: whoever held the old password is signed out.
    pub async fn redeem_setup_link(&self, token_hash: &str, password_hash: &str) -> Result<ActorRecord> {
        let mut tx = self.begin().await?;
        let actor_id: Uuid = sqlx::query_scalar(
            "update homestead.setup_links l set used_at = now()
             from homestead.actors a
             where l.token_hash = $1 and a.id = l.actor_id and l.used_at is null and l.expires_at > now()
               and a.disabled_at is null
             returning l.actor_id",
        )
        .bind(token_hash)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::NotFound("valid setup link"))?;
        sqlx::query(
            "insert into homestead.actor_credentials (actor_id, password_hash) values ($1, $2)
             on conflict (actor_id) do update
               set password_hash = excluded.password_hash, set_at = now(), failed_logins = 0, locked_until = null",
        )
        .bind(actor_id)
        .bind(password_hash)
        .execute(&mut *tx)
        .await?;
        sqlx::query("delete from homestead.sessions where actor_id = $1").bind(actor_id).execute(&mut *tx).await?;
        audit(&mut tx, "password_set", "actors", Some(actor_id.to_string()), serde_json::json!({})).await?;
        tx.commit().await?;
        self.get_actor(actor_id).await
    }

    pub async fn login_candidate(&self, email: &str) -> Result<Option<LoginCandidate>> {
        let row: Option<(Uuid, String, Option<OffsetDateTime>)> = sqlx::query_as(
            "select a.id, c.password_hash, c.locked_until
             from homestead.actors a join homestead.actor_credentials c on c.actor_id = a.id
             where a.email = lower(btrim($1)) and a.kind = 'human'",
        )
        .bind(email)
        .fetch_optional(&self.pool)
        .await?;
        let Some((id, password_hash, locked_until)) = row else { return Ok(None) };
        Ok(Some(LoginCandidate { actor: self.get_actor(id).await?, password_hash, locked_until }))
    }

    /// Counts a wrong password; the tenth in a row locks the account.
    pub async fn record_login_failure(&self, actor_id: Uuid) -> Result<()> {
        let mut tx = self.begin().await?;
        let locked: bool = sqlx::query_scalar(
            "update homestead.actor_credentials
             set failed_logins = failed_logins + 1,
                 locked_until = case when failed_logins + 1 >= $2 then now() + $3 else locked_until end
             where actor_id = $1 returning locked_until is not null and locked_until > now()",
        )
        .bind(actor_id)
        .bind(LOCKOUT_AFTER)
        .bind(LOCKOUT_FOR)
        .fetch_one(&mut *tx)
        .await?;
        let meta = serde_json::json!({ "locked": locked });
        audit(&mut tx, "login_failed", "actors", Some(actor_id.to_string()), meta).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Starts a session (by the hash of its cookie) for an actor who just
    /// proved their password; this store should be acting as them.
    pub async fn start_session(&self, actor_id: Uuid, token_hash: &str, valid_for: Duration) -> Result<()> {
        let mut tx = self.begin().await?;
        sqlx::query(
            "update homestead.actor_credentials set failed_logins = 0, locked_until = null where actor_id = $1",
        )
        .bind(actor_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("delete from homestead.sessions where actor_id = $1 and expires_at <= now()")
            .bind(actor_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("insert into homestead.sessions (token_hash, actor_id, expires_at) values ($1, $2, now() + $3)")
            .bind(token_hash)
            .bind(actor_id)
            .bind(valid_for)
            .execute(&mut *tx)
            .await?;
        audit(&mut tx, "login", "actors", Some(actor_id.to_string()), serde_json::json!({})).await?;
        tx.commit().await?;
        Ok(())
    }

    /// The signed-in person behind a session cookie, if it's still valid.
    pub async fn session_actor(&self, token_hash: &str) -> Result<Option<ActorRecord>> {
        let mut tx = self.begin().await?;
        // Touch at most every five minutes: a write per page view buys nothing.
        let id: Option<Uuid> = sqlx::query_scalar(
            "with s as (
               select s.actor_id from homestead.sessions s join homestead.actors a on a.id = s.actor_id
               where s.token_hash = $1 and s.expires_at > now() and a.disabled_at is null
             ), touch as (
               update homestead.sessions set last_seen_at = now()
               where token_hash = $1 and last_seen_at < now() - interval '5 minutes'
             )
             select actor_id from s",
        )
        .bind(token_hash)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        match id {
            Some(id) => Ok(Some(self.get_actor(id).await?)),
            None => Ok(None),
        }
    }

    pub async fn end_session(&self, token_hash: &str) -> Result<()> {
        let mut tx = self.begin().await?;
        let actor: Option<Uuid> =
            sqlx::query_scalar("delete from homestead.sessions where token_hash = $1 returning actor_id")
                .bind(token_hash)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(id) = actor {
            audit(&mut tx, "logout", "actors", Some(id.to_string()), serde_json::json!({})).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    // ── API tokens ─────────────────────────────────────────────────────────

    /// Stores a new API token (by hash) for a service account.
    pub async fn create_api_token(&self, actor_id: Uuid, name: &str, token_hash: &str) -> Result<Uuid> {
        let owner = self.get_actor(actor_id).await?;
        if owner.kind != ActorKind::Service || owner.disabled_at.is_some() {
            return Err(StoreError::Conflict("API tokens belong to active service accounts".into()));
        }
        let name = name.trim();
        if name.is_empty() {
            return Err(homestead_domain::ValidationError::Blank("token name").into());
        }
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.api_tokens (actor_id, name, token_hash, created_by) values ($1, $2, $3, $4)
             returning id",
        )
        .bind(actor_id)
        .bind(name)
        .bind(token_hash)
        .bind(self.actor().and_then(|a| a.id))
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// The service behind a bearer token, if the token is live.
    pub async fn api_token_actor(&self, token_hash: &str) -> Result<Option<ActorRecord>> {
        let mut tx = self.begin().await?;
        let id: Option<Uuid> = sqlx::query_scalar(
            "update homestead.api_tokens t
             set last_used_at = now()
             from homestead.actors a
             where t.token_hash = $1 and t.revoked_at is null and a.id = t.actor_id and a.disabled_at is null
             returning t.actor_id",
        )
        .bind(token_hash)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        match id {
            Some(id) => Ok(Some(self.get_actor(id).await?)),
            None => Ok(None),
        }
    }

    pub async fn list_api_tokens(&self) -> Result<Vec<ApiTokenInfo>> {
        Ok(sqlx::query_as(
            "select t.id, t.name, a.display_name as owner, t.created_at, t.last_used_at, t.revoked_at
             from homestead.api_tokens t join homestead.actors a on a.id = t.actor_id
             order by t.revoked_at is not null, t.created_at desc",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn revoke_api_token(&self, id: Uuid) -> Result<()> {
        let mut tx = self.begin().await?;
        let n = sqlx::query("update homestead.api_tokens set revoked_at = coalesce(revoked_at, now()) where id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        if n == 0 { Err(StoreError::NotFound("API token")) } else { Ok(()) }
    }

    /// One actor's own actions, newest first, limited to records `caps`
    /// may still read.
    pub async fn audit_by_actor(
        &self,
        actor_id: Uuid,
        caps: homestead_domain::auth::CapabilitySet,
        limit: i64,
    ) -> Result<Vec<crate::AuditEntry>> {
        let (tables, owners) = crate::audit_scope(caps);
        Ok(sqlx::query_as(concat!(
            "select id, occurred_at, actor_label, request_id, action, resource_type, resource_id,
                    before, after, metadata
             from homestead.audit_entries where actor_id = $4 and ",
            crate::audit_visible!(),
            " order by id desc limit $3"
        ))
        .bind(tables)
        .bind(owners)
        .bind(limit)
        .bind(actor_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn mark_welcomed(&self, id: Uuid) -> Result<()> {
        let mut tx = self.begin().await?;
        let n = sqlx::query("update homestead.actors set welcomed_at = now() where id = $1 and welcomed_at is null")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        if n == 0 && self.get_actor(id).await?.welcomed_at.is_none() {
            return Err(StoreError::NotFound("person"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct ApiTokenInfo {
    pub id: Uuid,
    pub name: String,
    pub owner: String,
    pub created_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
    pub revoked_at: Option<OffsetDateTime>,
}

async fn grant_roles(tx: &mut Tx<'_>, actor_id: Uuid, roles: &[Role], granted_by: Option<Uuid>) -> Result<()> {
    for r in roles {
        sqlx::query("insert into homestead.actor_roles (actor_id, role, granted_by) values ($1, $2, $3)")
            .bind(actor_id)
            .bind(r.as_str())
            .bind(granted_by)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

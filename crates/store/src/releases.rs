//! Legal release requests sent through Documenso.

use homestead_domain::Event;
use homestead_domain::onboarding::OnboardingRole;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Result, Store, StoreError, outbox};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseRequest {
    pub id: Uuid,
    pub release_key: String,
    pub title: String,
    pub signer_name: String,
    pub signer_email: String,
    pub envelope_id: String,
    pub status: String,
    pub sent_at: OffsetDateTime,
    pub completed_at: Option<OffsetDateTime>,
    /// Where the signer signs, while the request is pending.
    pub signing_url: Option<String>,
}

/// A release Documenso has accepted, to record.
#[derive(Debug, Clone, Copy)]
pub struct SentRelease<'a> {
    /// The `externalId` the envelope was created with.
    pub id: Uuid,
    pub release_key: &'a str,
    pub title: &'a str,
    pub signer_name: &'a str,
    pub signer_email: &'a str,
    pub envelope_id: &'a str,
    pub signing_url: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Signed,
    Declined,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Signed => "signed",
            Outcome::Declined => "declined",
        }
    }
}

type Row =
    (Uuid, String, String, String, String, String, String, OffsetDateTime, Option<OffsetDateTime>, Option<String>);

fn from_row(r: Row) -> ReleaseRequest {
    let (id, release_key, title, signer_name, signer_email, envelope_id, status, sent_at, completed_at, signing_url) =
        r;
    ReleaseRequest {
        id,
        release_key,
        title,
        signer_name,
        signer_email,
        envelope_id,
        status,
        sent_at,
        completed_at,
        signing_url,
    }
}

/// A Discord member joining through `/onboard`.
#[derive(Debug, Clone, Copy)]
pub struct Joiner<'a> {
    pub discord_user_id: &'a str,
    pub discord_username: &'a str,
    pub role: OnboardingRole,
}

/// Someone who joined through Discord, and their latest welcome agreement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Onboarding {
    pub discord_user_id: String,
    pub discord_username: String,
    pub role: OnboardingRole,
    pub started_at: OffsetDateTime,
    /// When a agreement came back signed.
    pub completed_at: Option<OffsetDateTime>,
    /// The most recent agreement sent for them.
    pub latest: Option<ReleaseRequest>,
}

async fn insert_request(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    r: &SentRelease<'_>,
    discord_user_id: Option<&str>,
) -> Result<ReleaseRequest> {
    let row: Row = sqlx::query_as(
        "insert into homestead.release_requests
           (id, release_key, title, signer_name, signer_email, envelope_id, signing_url, discord_user_id)
         values ($1, $2, $3, btrim($4), lower(btrim($5)), $6, $7, $8)
         returning id, release_key, title, signer_name, signer_email, envelope_id, status, sent_at, completed_at, signing_url",
    )
    .bind(r.id)
    .bind(r.release_key)
    .bind(r.title)
    .bind(r.signer_name)
    .bind(r.signer_email)
    .bind(r.envelope_id)
    .bind(r.signing_url)
    .bind(discord_user_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| match StoreError::from(e) {
        StoreError::Db(sqlx::Error::Database(d)) if d.code().as_deref() == Some("23514") => {
            StoreError::Invalid(homestead_domain::ValidationError::Malformed("signer name or email"))
        }
        other => other,
    })?;
    Ok(from_row(row))
}

impl Store {
    /// Records a release that Documenso has accepted for signing.
    pub async fn record_release_sent(&self, r: &SentRelease<'_>) -> Result<ReleaseRequest> {
        let mut tx = self.begin().await?;
        let request = insert_request(&mut tx, r, None).await?;
        tx.commit().await?;
        Ok(request)
    }

    /// Records a welcome agreement sent for a Discord member, and the role
    /// they asked for. Asking again (a new agreement, or another role) updates
    /// their onboarding; one they already finished stays finished.
    pub async fn record_onboarding_sent(&self, r: &SentRelease<'_>, who: Joiner<'_>) -> Result<ReleaseRequest> {
        let mut tx = self.begin().await?;
        let request = insert_request(&mut tx, r, Some(who.discord_user_id)).await?;
        sqlx::query(
            "insert into homestead.onboardings (discord_user_id, discord_username, role) values ($1, btrim($2), $3)
             on conflict (discord_user_id) do update
               set discord_username = excluded.discord_username, role = excluded.role",
        )
        .bind(who.discord_user_id)
        .bind(who.discord_username)
        .bind(who.role.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|e| match StoreError::from(e) {
            StoreError::Db(sqlx::Error::Database(d)) if d.code().as_deref() == Some("23514") => {
                StoreError::Invalid(homestead_domain::ValidationError::Malformed("Discord member"))
            }
            other => other,
        })?;
        tx.commit().await?;
        Ok(request)
    }

    /// Someone who already signed asks for a role (another one, or theirs
    /// back): record it and grant it. `None` if they haven't signed a
    /// agreement yet.
    pub async fn choose_onboarded_role(&self, who: Joiner<'_>) -> Result<Option<Onboarding>> {
        let mut tx = self.begin().await?;
        let name: Option<String> = sqlx::query_scalar(
            "update homestead.onboardings o set role = $2, discord_username = btrim($3)
             where o.discord_user_id = $1 and o.completed_at is not null
             returning (select r.signer_name from homestead.release_requests r
                        where r.discord_user_id = o.discord_user_id and r.status = 'signed'
                        order by r.completed_at desc limit 1)",
        )
        .bind(who.discord_user_id)
        .bind(who.role.as_str())
        .bind(who.discord_username)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(name) = name else { return Ok(None) };
        let event = Event::MemberOnboarded {
            discord_user_id: who.discord_user_id.to_owned(),
            name: name.clone(),
            role: who.role,
        };
        outbox::emit(&mut tx, &event).await?;
        tx.commit().await?;
        self.onboarding(who.discord_user_id).await
    }

    /// Settles a sent request and announces it. Returns `None` if the id is
    /// unknown or the request was already settled: webhooks are retried, and
    /// a repeat must not announce twice.
    pub async fn complete_release(&self, id: Uuid, outcome: Outcome) -> Result<Option<ReleaseRequest>> {
        let mut tx = self.begin().await?;
        let row: Option<Row> = sqlx::query_as(
            "update homestead.release_requests set status = $2, completed_at = now(), signing_url = null
             where id = $1 and status = 'sent'
             returning id, release_key, title, signer_name, signer_email, envelope_id, status, sent_at, completed_at, signing_url",
        )
        .bind(id)
        .bind(outcome.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let r = from_row(row);
        let (release_key, title, signer_name) = (r.release_key.clone(), r.title.clone(), r.signer_name.clone());
        let event = match outcome {
            Outcome::Signed => Event::ReleaseSigned { release_key, title, signer_name },
            Outcome::Declined => Event::ReleaseDeclined { release_key, title, signer_name },
        };
        outbox::emit(&mut tx, &event).await?;
        if outcome == Outcome::Signed {
            // A agreement sent for a Discord member finishes their onboarding
            // (the first one they sign), and grants the role they asked for.
            let joined: Option<(String, String)> = sqlx::query_as(
                "update homestead.onboardings o set completed_at = now()
                 from homestead.release_requests r
                 where r.id = $1 and o.discord_user_id = r.discord_user_id and o.completed_at is null
                 returning o.discord_user_id, o.role",
            )
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some((discord_user_id, role)) = joined {
                let role = role.parse().map_err(StoreError::Invalid)?;
                outbox::emit(&mut tx, &Event::MemberOnboarded { discord_user_id, name: r.signer_name.clone(), role })
                    .await?;
            }
        }
        tx.commit().await?;
        Ok(Some(r))
    }

    /// Where one Discord member stands.
    pub async fn onboarding(&self, discord_user_id: &str) -> Result<Option<Onboarding>> {
        Ok(self.onboardings_where(Some(discord_user_id)).await?.pop())
    }

    /// Everyone who joined through Discord, newest first.
    pub async fn onboardings(&self) -> Result<Vec<Onboarding>> {
        self.onboardings_where(None).await
    }

    async fn onboardings_where(&self, discord_user_id: Option<&str>) -> Result<Vec<Onboarding>> {
        #[derive(sqlx::FromRow)]
        struct Joined {
            discord_user_id: String,
            discord_username: String,
            role: String,
            started_at: OffsetDateTime,
            onboarded_at: Option<OffsetDateTime>,
            id: Option<Uuid>,
            release_key: Option<String>,
            title: Option<String>,
            signer_name: Option<String>,
            signer_email: Option<String>,
            envelope_id: Option<String>,
            status: Option<String>,
            sent_at: Option<OffsetDateTime>,
            completed_at: Option<OffsetDateTime>,
            signing_url: Option<String>,
        }
        let rows: Vec<Joined> = sqlx::query_as(
            "select o.discord_user_id, o.discord_username, o.role, o.started_at, o.completed_at as onboarded_at,
                    r.id, r.release_key, r.title, r.signer_name, r.signer_email, r.envelope_id, r.status,
                    r.sent_at, r.completed_at, r.signing_url
             from homestead.onboardings o
             left join lateral (
               select * from homestead.release_requests r
               where r.discord_user_id = o.discord_user_id order by r.sent_at desc limit 1
             ) r on true
             where $1::text is null or o.discord_user_id = $1
             order by o.started_at desc",
        )
        .bind(discord_user_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|j| {
                let latest = match (j.id, j.release_key, j.title, j.signer_name, j.signer_email, j.envelope_id) {
                    (
                        Some(id),
                        Some(release_key),
                        Some(title),
                        Some(signer_name),
                        Some(signer_email),
                        Some(envelope_id),
                    ) => Some(ReleaseRequest {
                        id,
                        release_key,
                        title,
                        signer_name,
                        signer_email,
                        envelope_id,
                        status: j.status.unwrap_or_default(),
                        sent_at: j.sent_at.unwrap_or(j.started_at),
                        completed_at: j.completed_at,
                        signing_url: j.signing_url,
                    }),
                    _ => None,
                };
                Ok(Onboarding {
                    discord_user_id: j.discord_user_id,
                    discord_username: j.discord_username,
                    role: j.role.parse().map_err(StoreError::Invalid)?,
                    started_at: j.started_at,
                    completed_at: j.onboarded_at,
                    latest,
                })
            })
            .collect()
    }

    pub async fn recent_releases(&self, limit: i64) -> Result<Vec<ReleaseRequest>> {
        let rows: Vec<Row> = sqlx::query_as(
            "select id, release_key, title, signer_name, signer_email, envelope_id, status, sent_at, completed_at, signing_url
             from homestead.release_requests order by sent_at desc limit $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(from_row).collect())
    }
}

/// Someone joining from the `/join` link, and how far they've got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Join {
    pub id: Uuid,
    pub name: String,
    pub email: String,
    pub role: OnboardingRole,
    pub created_at: OffsetDateTime,
    /// The agreement, once Documenso has it.
    pub request: Option<ReleaseRequest>,
    /// Why it couldn't be sent.
    pub send_error: Option<String>,
    /// The send neither finished nor failed in time: the core restarted
    /// under it. It may be retried, like a failure.
    pub stalled: bool,
    /// The exact Discord account connected after signing.
    pub discord: Option<(String, String)>,
}

impl Join {
    pub fn signed(&self) -> bool {
        self.request.as_ref().is_some_and(|r| r.status == "signed")
    }
}

#[derive(sqlx::FromRow)]
struct JoinRow {
    join_id: Uuid,
    name: String,
    email: String,
    role: String,
    created_at: OffsetDateTime,
    send_error: Option<String>,
    stalled: bool,
    discord_user_id: Option<String>,
    discord_username: Option<String>,
    id: Option<Uuid>,
    release_key: Option<String>,
    title: Option<String>,
    signer_name: Option<String>,
    signer_email: Option<String>,
    envelope_id: Option<String>,
    status: Option<String>,
    sent_at: Option<OffsetDateTime>,
    completed_at: Option<OffsetDateTime>,
    signing_url: Option<String>,
}

/// How long a join's send may run before it counts as cut off. It covers
/// waking Documenso (90 s), the create, and the distribute with its retries.
macro_rules! join_stall {
    () => {
        "interval '5 minutes'"
    };
}

macro_rules! join_select {
    ($where:literal) => {
        concat!(
            "select j.id as join_id, j.name, j.email, j.role, j.created_at, j.send_error,
                    (j.release_request_id is null and j.send_error is null
                     and j.send_started_at < now() - ",
            join_stall!(),
            ") as stalled,
                    j.discord_user_id, j.discord_username,
                    r.id, r.release_key, r.title, r.signer_name, r.signer_email,
                    r.envelope_id, r.status, r.sent_at, r.completed_at, r.signing_url
             from homestead.joins j
             left join homestead.release_requests r on r.id = j.release_request_id ",
            $where
        )
    };
}

fn join_from(j: JoinRow) -> Result<Join> {
    let request = match (j.id, j.release_key, j.title, j.signer_name, j.signer_email, j.envelope_id, j.sent_at) {
        (
            Some(id),
            Some(release_key),
            Some(title),
            Some(signer_name),
            Some(signer_email),
            Some(envelope_id),
            Some(sent_at),
        ) => Some(ReleaseRequest {
            id,
            release_key,
            title,
            signer_name,
            signer_email,
            envelope_id,
            status: j.status.unwrap_or_default(),
            sent_at,
            completed_at: j.completed_at,
            signing_url: j.signing_url,
        }),
        _ => None,
    };
    Ok(Join {
        id: j.join_id,
        name: j.name,
        email: j.email,
        role: j.role.parse().map_err(StoreError::Invalid)?,
        created_at: j.created_at,
        request,
        send_error: j.send_error,
        stalled: j.stalled,
        discord: j.discord_user_id.zip(j.discord_username),
    })
}

/// A join's agreement, ready to send (or to send again).
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct JoinSend {
    pub id: Uuid,
    /// The release request id, which Documenso's webhook carries back.
    pub release_id: Uuid,
    pub name: String,
    pub email: String,
    /// Already created in Documenso: distribute it, don't create another.
    pub envelope_id: Option<String>,
}

impl Store {
    /// Starts a join from the link: who they are, before the agreement is
    /// sent. `token_hash` is the sha256 of their page's token. `None` when
    /// that token already started one (the form sent twice): send nothing
    /// more.
    pub async fn start_join(
        &self,
        token_hash: &str,
        name: &str,
        email: &str,
        role: OnboardingRole,
    ) -> Result<Option<JoinSend>> {
        let mut tx = self.begin().await?;
        let started: Option<JoinSend> = sqlx::query_as(
            "insert into homestead.joins (id, token_hash, name, email, role)
             values (gen_random_uuid(), $1, btrim($2), lower(btrim($3)), $4)
             on conflict (token_hash) do nothing
             returning id, release_id, name, email, envelope_id",
        )
        .bind(token_hash)
        .bind(name)
        .bind(email)
        .bind(role.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| match StoreError::from(e) {
            StoreError::Db(sqlx::Error::Database(d)) if d.code().as_deref() == Some("23514") => {
                StoreError::Invalid(homestead_domain::ValidationError::Malformed("name or email"))
            }
            other => other,
        })?;
        tx.commit().await?;
        Ok(started)
    }

    /// Records the envelope Documenso created for a join, before it is
    /// distributed, so a retry distributes this one instead of creating
    /// another.
    pub async fn record_join_envelope(&self, join: Uuid, envelope: &str) -> Result<()> {
        let mut tx = self.begin().await?;
        sqlx::query("update homestead.joins set envelope_id = $2 where id = $1")
            .bind(join)
            .bind(envelope)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Claims a failed or cut-off send to try again. `None` unless it failed
    /// or stalled: a send still running, or one already sent, isn't retried,
    /// and of two taps on "Try again" only one wins.
    pub async fn retry_join(&self, join: Uuid) -> Result<Option<JoinSend>> {
        let mut tx = self.begin().await?;
        let claimed: Option<JoinSend> = sqlx::query_as(concat!(
            "update homestead.joins set send_error = null, send_started_at = now()
             where id = $1 and release_request_id is null
               and (send_error is not null or send_started_at < now() - ",
            join_stall!(),
            ")
             returning id, release_id, name, email, envelope_id"
        ))
        .bind(join)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(claimed)
    }

    /// Records the agreement Documenso accepted for a join.
    pub async fn record_join_sent(&self, join: Uuid, r: &SentRelease<'_>) -> Result<ReleaseRequest> {
        let mut tx = self.begin().await?;
        let request = insert_request(&mut tx, r, None).await?;
        sqlx::query("update homestead.joins set release_request_id = $2, send_error = null where id = $1")
            .bind(join)
            .bind(request.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(request)
    }

    /// Records that the agreement couldn't be sent, for their page to say.
    pub async fn record_join_failed(&self, join: Uuid, error: &str) -> Result<()> {
        let mut tx = self.begin().await?;
        sqlx::query("update homestead.joins set send_error = left($2, 300) where id = $1")
            .bind(join)
            .bind(error)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Claims a Discord account for this signed join, before the bot adds
    /// it. One signed agreement admits one account: the same account may
    /// claim again (a retry), a different one is refused. Also refused
    /// unless the welcome agreement is signed.
    pub async fn claim_join_discord(&self, join: Uuid, user_id: &str, username: &str) -> Result<()> {
        let mut tx = self.begin().await?;
        let updated = sqlx::query(
            "update homestead.joins j set discord_user_id = $2, discord_username = $3
             from homestead.release_requests r
             where j.id = $1 and r.id = j.release_request_id and r.status = 'signed'
               and (j.discord_user_id is null or j.discord_user_id = $2)",
        )
        .bind(join)
        .bind(user_id)
        .bind(username)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(StoreError::Conflict(
                "this link needs a signed agreement and admits one Discord account".into(),
            ));
        }
        tx.commit().await?;
        Ok(())
    }

    /// Gives back a claim whose Discord add failed, so the person can try
    /// again (with any account).
    pub async fn release_join_discord(&self, join: Uuid, user_id: &str) -> Result<()> {
        let mut tx = self.begin().await?;
        sqlx::query(
            "update homestead.joins set discord_user_id = null, discord_username = null
             where id = $1 and discord_user_id = $2",
        )
        .bind(join)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The join a page token belongs to.
    pub async fn join_by_token(&self, token_hash: &str) -> Result<Option<Join>> {
        let row: Option<JoinRow> =
            sqlx::query_as(join_select!("where j.token_hash = $1")).bind(token_hash).fetch_optional(&self.pool).await?;
        row.map(join_from).transpose()
    }

    /// Everyone who joined from the link, newest first.
    pub async fn joins(&self, limit: i64) -> Result<Vec<Join>> {
        let rows: Vec<JoinRow> = sqlx::query_as(join_select!("order by j.created_at desc limit $1"))
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(join_from).collect()
    }
}

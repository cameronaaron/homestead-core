//! Persistent grant templates, recipients, campaigns and delivery state.

use std::collections::BTreeMap;

use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Result, Store, StoreError};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct GrantTemplate {
    pub id: Uuid,
    pub name: String,
    pub subject_template: String,
    pub body_template: String,
    pub design: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct RecipientRow {
    id: Uuid,
    organization: String,
    contact_name: String,
    email: String,
    program_name: String,
    source: String,
    custom_fields: serde_json::Value,
    suppressed_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
pub struct GrantRecipient {
    pub id: Uuid,
    pub organization: String,
    pub contact_name: String,
    pub email: String,
    pub program_name: String,
    pub source: String,
    pub custom_fields: BTreeMap<String, String>,
    pub suppressed_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
pub struct NewGrantRecipient {
    pub organization: String,
    pub contact_name: String,
    pub email: String,
    pub program_name: String,
    pub source: String,
    pub custom_fields: BTreeMap<String, String>,
}

impl TryFrom<RecipientRow> for GrantRecipient {
    type Error = StoreError;

    fn try_from(row: RecipientRow) -> Result<Self> {
        let custom_fields = serde_json::from_value(row.custom_fields)
            .map_err(|e| StoreError::Corrupt(format!("grant recipient custom_fields: {e}")))?;
        Ok(Self {
            id: row.id,
            organization: row.organization,
            contact_name: row.contact_name,
            email: row.email,
            program_name: row.program_name,
            source: row.source,
            custom_fields,
            suppressed_at: row.suppressed_at,
        })
    }
}

#[derive(Debug, Clone)]
pub struct NewGrantMessage {
    pub id: Uuid,
    pub recipient_id: Uuid,
    pub recipient_email: String,
    pub recipient_name: String,
    pub organization: String,
    pub subject: String,
    pub body_text: String,
    pub body_html: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct GrantMessage {
    pub id: Uuid,
    pub campaign_id: Uuid,
    pub recipient_email: String,
    pub recipient_name: String,
    pub organization: String,
    pub subject: String,
    pub body_text: String,
    pub body_html: String,
    pub status: String,
    pub provider_id: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct GrantCampaign {
    pub id: Uuid,
    pub name: String,
    pub template_name: String,
    pub status: String,
    pub created_at: OffsetDateTime,
    pub total: i64,
    pub sent: i64,
    pub failed: i64,
    pub suppressed: i64,
}

impl Store {
    pub async fn create_grant_template(&self, name: &str, subject: &str, body: &str) -> Result<GrantTemplate> {
        self.create_grant_template_with_design(name, subject, body, "sunrise").await
    }

    pub async fn create_grant_template_with_design(
        &self,
        name: &str,
        subject: &str,
        body: &str,
        design: &str,
    ) -> Result<GrantTemplate> {
        let mut tx = self.begin().await?;
        let template: GrantTemplate = sqlx::query_as(
            "insert into homestead.grant_templates (name, subject_template, body_template, design)
             values (btrim($1), btrim($2), btrim($3), $4)
             returning id, name, subject_template, body_template, design",
        )
        .bind(name)
        .bind(subject)
        .bind(body)
        .bind(design)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(template)
    }

    pub async fn update_grant_template(
        &self,
        id: Uuid,
        name: &str,
        subject: &str,
        body: &str,
        design: &str,
    ) -> Result<GrantTemplate> {
        let mut tx = self.begin().await?;
        let template: GrantTemplate = sqlx::query_as(
            "update homestead.grant_templates
             set name = btrim($2), subject_template = btrim($3), body_template = btrim($4), design = $5,
                 updated_at = now()
             where id = $1
             returning id, name, subject_template, body_template, design",
        )
        .bind(id)
        .bind(name)
        .bind(subject)
        .bind(body)
        .bind(design)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::NotFound("grant template"))?;
        tx.commit().await?;
        Ok(template)
    }

    pub async fn list_grant_templates(&self) -> Result<Vec<GrantTemplate>> {
        Ok(sqlx::query_as(
            "select id, name, subject_template, body_template, design from homestead.grant_templates order by name",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn get_grant_template(&self, id: Uuid) -> Result<GrantTemplate> {
        sqlx::query_as(
            "select id, name, subject_template, body_template, design from homestead.grant_templates where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound("grant template"))
    }

    pub async fn create_grant_recipient(
        &self,
        organization: &str,
        contact_name: &str,
        email: &str,
        program_name: &str,
        source: &str,
        custom_fields: &BTreeMap<String, String>,
    ) -> Result<GrantRecipient> {
        let custom = serde_json::to_value(custom_fields).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        let mut tx = self.begin().await?;
        let row: RecipientRow = sqlx::query_as(
            "insert into homestead.grant_recipients
               (organization, contact_name, email, program_name, source, custom_fields)
             values (btrim($1), btrim($2), lower(btrim($3)), btrim($4), btrim($5), $6)
             returning id, organization, contact_name, email, program_name, source, custom_fields, suppressed_at",
        )
        .bind(organization)
        .bind(contact_name)
        .bind(email)
        .bind(program_name)
        .bind(source)
        .bind(custom)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        row.try_into()
    }

    pub async fn list_grant_recipients(&self) -> Result<Vec<GrantRecipient>> {
        let rows: Vec<RecipientRow> = sqlx::query_as(
            "select id, organization, contact_name, email, program_name, source, custom_fields, suppressed_at
             from homestead.grant_recipients order by suppressed_at nulls first, organization, email",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Imports a pre-validated group atomically: one duplicate or malformed
    /// row rolls the whole CSV back instead of leaving a partial directory.
    pub async fn import_grant_recipients(&self, recipients: &[NewGrantRecipient]) -> Result<u64> {
        if recipients.is_empty() {
            return Ok(0);
        }
        let mut tx = self.begin().await?;
        for recipient in recipients {
            let custom = serde_json::to_value(&recipient.custom_fields)
                .map_err(|e| StoreError::Corrupt(format!("grant recipient custom_fields: {e}")))?;
            sqlx::query(
                "insert into homestead.grant_recipients
                   (organization, contact_name, email, program_name, source, custom_fields)
                 values (btrim($1), btrim($2), lower(btrim($3)), btrim($4), btrim($5), $6)",
            )
            .bind(&recipient.organization)
            .bind(&recipient.contact_name)
            .bind(&recipient.email)
            .bind(&recipient.program_name)
            .bind(&recipient.source)
            .bind(custom)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(recipients.len() as u64)
    }

    pub async fn suppress_grant_recipient(&self, id: Uuid) -> Result<()> {
        let mut tx = self.begin().await?;
        let changed = sqlx::query(
            "update homestead.grant_recipients set suppressed_at = coalesce(suppressed_at, now()), updated_at = now()
             where id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed == 0 {
            return Err(StoreError::NotFound("grant recipient"));
        }
        sqlx::query(
            "update homestead.grant_messages set status = 'suppressed'
             where recipient_id = $1 and status in ('ready', 'failed')",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "update homestead.grant_campaigns c set status = 'completed', completed_at = now()
             where c.status = 'approved'
               and exists (select 1 from homestead.grant_messages m where m.campaign_id = c.id and m.recipient_id = $1)
               and not exists (
                 select 1 from homestead.grant_messages m
                 where m.campaign_id = c.id and m.status not in ('sent', 'suppressed')
               )",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn create_grant_campaign(
        &self,
        name: &str,
        template_id: Uuid,
        messages: &[NewGrantMessage],
    ) -> Result<Uuid> {
        if messages.is_empty() {
            return Err(StoreError::Conflict("a campaign needs at least one active recipient".into()));
        }
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.grant_campaigns (name, template_id) values (btrim($1), $2) returning id",
        )
        .bind(name)
        .bind(template_id)
        .fetch_one(&mut *tx)
        .await?;
        for message in messages {
            sqlx::query(
                "insert into homestead.grant_messages
                   (id, campaign_id, recipient_id, recipient_email, recipient_name, organization, subject, body_text, body_html)
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(message.id)
            .bind(id)
            .bind(message.recipient_id)
            .bind(&message.recipient_email)
            .bind(&message.recipient_name)
            .bind(&message.organization)
            .bind(&message.subject)
            .bind(&message.body_text)
            .bind(&message.body_html)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(id)
    }

    pub async fn list_grant_campaigns(&self) -> Result<Vec<GrantCampaign>> {
        Ok(sqlx::query_as(
            "select c.id, c.name, t.name as template_name, c.status, c.created_at,
                    count(m.id) as total,
                    count(m.id) filter (where m.status = 'sent') as sent,
                    count(m.id) filter (where m.status = 'failed') as failed,
                    count(m.id) filter (where m.status = 'suppressed') as suppressed
             from homestead.grant_campaigns c
             join homestead.grant_templates t on t.id = c.template_id
             left join homestead.grant_messages m on m.campaign_id = c.id
             group by c.id, t.name order by c.created_at desc",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn get_grant_campaign(&self, id: Uuid) -> Result<GrantCampaign> {
        sqlx::query_as(
            "select c.id, c.name, t.name as template_name, c.status, c.created_at,
                    count(m.id) as total,
                    count(m.id) filter (where m.status = 'sent') as sent,
                    count(m.id) filter (where m.status = 'failed') as failed,
                    count(m.id) filter (where m.status = 'suppressed') as suppressed
             from homestead.grant_campaigns c
             join homestead.grant_templates t on t.id = c.template_id
             left join homestead.grant_messages m on m.campaign_id = c.id
             where c.id = $1 group by c.id, t.name",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound("grant campaign"))
    }

    pub async fn grant_campaign_messages(&self, campaign_id: Uuid) -> Result<Vec<GrantMessage>> {
        Ok(sqlx::query_as(
            "select id, campaign_id, recipient_email, recipient_name, organization, subject, body_text, body_html,
                    status, provider_id, last_error
             from homestead.grant_messages where campaign_id = $1 order by organization, recipient_email",
        )
        .bind(campaign_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn approve_grant_campaign(&self, id: Uuid) -> Result<()> {
        let mut tx = self.begin().await?;
        let changed = sqlx::query(
            "update homestead.grant_campaigns set status = 'approved', approved_at = now()
             where id = $1 and status = 'draft'
               and exists (select 1 from homestead.grant_messages where campaign_id = $1)",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        if changed == 0 { Err(StoreError::Conflict("campaign is not an approvable draft".into())) } else { Ok(()) }
    }

    pub async fn retry_grant_failures(&self, campaign_id: Uuid) -> Result<u64> {
        let mut tx = self.begin().await?;
        let n = sqlx::query(
            "update homestead.grant_messages m set status = 'ready', last_error = null, claimed_at = null
             from homestead.grant_campaigns c
             where m.campaign_id = $1 and c.id = m.campaign_id and c.status = 'approved' and m.status = 'failed'",
        )
        .bind(campaign_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(n)
    }

    /// Atomically leases a bounded batch. Stale leases are reclaimed; the
    /// provider idempotency key makes a post-send crash safe to retry.
    pub async fn claim_grant_messages(&self, campaign_id: Uuid, limit: i64) -> Result<Vec<GrantMessage>> {
        let mut tx = self.begin().await?;
        let claimed: Vec<GrantMessage> = sqlx::query_as(
            "with due as (
               select m.id from homestead.grant_messages m
               join homestead.grant_campaigns c on c.id = m.campaign_id
               join homestead.grant_recipients r on r.id = m.recipient_id
               where m.campaign_id = $1 and c.status = 'approved' and r.suppressed_at is null
                 and (m.status = 'ready' or (m.status = 'sending' and m.claimed_at < now() - interval '10 minutes'))
               order by m.organization, m.recipient_email limit $2 for update of m skip locked
             )
             update homestead.grant_messages m set status = 'sending', claimed_at = now()
             from due where m.id = due.id
             returning m.id, m.campaign_id, m.recipient_email, m.recipient_name, m.organization, m.subject,
                       m.body_text, m.body_html, m.status, m.provider_id, m.last_error",
        )
        .bind(campaign_id)
        .bind(limit.clamp(1, 100))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(claimed)
    }

    pub async fn settle_grant_message(&self, id: Uuid, result: std::result::Result<&str, &str>) -> Result<()> {
        let mut tx = self.begin().await?;
        match result {
            Ok(provider_id) => {
                sqlx::query(
                    "update homestead.grant_messages
                     set status = 'sent', provider_id = $2, last_error = null, sent_at = now()
                     where id = $1 and status = 'sending'",
                )
                .bind(id)
                .bind(provider_id)
                .execute(&mut *tx)
                .await?;
            }
            Err(error) => {
                sqlx::query(
                    "update homestead.grant_messages set status = 'failed', last_error = $2
                     where id = $1 and status = 'sending'",
                )
                .bind(id)
                .bind(error)
                .execute(&mut *tx)
                .await?;
            }
        }
        sqlx::query(
            "update homestead.grant_campaigns c set status = 'completed', completed_at = now()
             where c.id = (select campaign_id from homestead.grant_messages where id = $1)
               and c.status = 'approved'
               and not exists (
                 select 1 from homestead.grant_messages m
                 where m.campaign_id = c.id and m.status not in ('sent', 'suppressed')
               )",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

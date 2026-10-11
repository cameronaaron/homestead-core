//! Attachment metadata. The bytes live in object storage; this is the
//! record of what they are, whose they are, and their SHA-256.

use homestead_domain::attachments::{Attachment, AttachmentOwner, NewAttachment};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Result, Store, StoreError, Tx, parse};

macro_rules! attachment_cols {
    () => {
        "id, resource_type, resource_id, original_name, media_type, byte_size, sha256, storage_key, created_by, created_at"
    };
}

type Row = (Uuid, String, Uuid, String, String, i64, String, String, Option<Uuid>, OffsetDateTime);

fn attachment(r: Row) -> Result<Attachment> {
    let (id, owner, owner_id, original_name, media_type, byte_size, sha256, storage_key, created_by, created_at) = r;
    Ok(Attachment {
        id,
        owner: parse(&owner)?,
        owner_id,
        original_name,
        media_type,
        byte_size,
        sha256,
        storage_key,
        created_by,
        created_at,
    })
}

/// Whether the record a file is being attached to exists (one static query
/// per owner kind; table names can't be bound as parameters).
async fn owner_exists(tx: &mut Tx<'_>, owner: AttachmentOwner, id: Uuid) -> Result<bool> {
    let sql = match owner {
        AttachmentOwner::Animal => "select exists (select 1 from homestead.animals where id = $1)",
        AttachmentOwner::AnimalCare => "select exists (select 1 from homestead.animal_care where id = $1)",
        AttachmentOwner::Purchase => "select exists (select 1 from homestead.purchases where id = $1)",
        AttachmentOwner::Gift => "select exists (select 1 from homestead.gifts where id = $1)",
        AttachmentOwner::GrantCampaign => "select exists (select 1 from homestead.grant_campaigns where id = $1)",
        AttachmentOwner::SeedLot => "select exists (select 1 from homestead.seed_lots where id = $1)",
        AttachmentOwner::Equipment => "select exists (select 1 from homestead.equipment where id = $1)",
        AttachmentOwner::Incident => "select exists (select 1 from homestead.incidents where id = $1)",
        AttachmentOwner::SoilTest => "select exists (select 1 from homestead.soil_tests where id = $1)",
        AttachmentOwner::CropObservation => "select exists (select 1 from homestead.crop_observations where id = $1)",
        AttachmentOwner::Obligation => "select exists (select 1 from homestead.obligations where id = $1)",
        AttachmentOwner::EmergencyPlan => "select exists (select 1 from homestead.emergency_plans where id = $1)",
        AttachmentOwner::EquipmentMaintenance => {
            "select exists (select 1 from homestead.equipment_maintenance where id = $1)"
        }
    };
    Ok(sqlx::query_scalar(sql).bind(id).fetch_one(&mut **tx).await?)
}

impl Store {
    /// Records a stored file against its owner. The owner is locked-in by
    /// existence check in the same transaction as the insert.
    pub async fn record_attachment(&self, input: NewAttachment) -> Result<Attachment> {
        let a = input.validated()?;
        let mut tx = self.begin().await?;
        if !owner_exists(&mut tx, a.owner, a.owner_id).await? {
            return Err(StoreError::NotFound("record to attach to"));
        }
        let row: Row = sqlx::query_as(concat!(
            "insert into homestead.attachments
               (resource_type, resource_id, storage_key, original_name, media_type, byte_size, sha256, created_by)
             values ($1, $2, $3, $4, $5, $6, $7, $8) returning ",
            attachment_cols!()
        ))
        .bind(a.owner.as_str())
        .bind(a.owner_id)
        .bind(&a.storage_key)
        .bind(&a.original_name)
        .bind(&a.media_type)
        .bind(a.byte_size)
        .bind(&a.sha256)
        .bind(self.actor().and_then(|x| x.id))
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        attachment(row)
    }

    pub async fn get_attachment(&self, id: Uuid) -> Result<Attachment> {
        let row: Option<Row> =
            sqlx::query_as(concat!("select ", attachment_cols!(), " from homestead.attachments where id = $1"))
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        attachment(row.ok_or(StoreError::NotFound("attachment"))?)
    }

    /// Files on one record, newest first.
    pub async fn list_attachments(&self, owner: AttachmentOwner, owner_id: Uuid) -> Result<Vec<Attachment>> {
        let rows: Vec<Row> = sqlx::query_as(concat!(
            "select ",
            attachment_cols!(),
            " from homestead.attachments where resource_type = $1 and resource_id = $2 order by created_at desc, id"
        ))
        .bind(owner.as_str())
        .bind(owner_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(attachment).collect()
    }

    /// Removes the record of a file and returns it, queueing its bytes for
    /// purging after `retain_days` (see [`Store::due_blob_purges`]). Until
    /// then [`Store::restore_attachment`] can bring it back. The audit log
    /// keeps its name, size and SHA-256 either way.
    pub async fn delete_attachment(&self, id: Uuid, retain_days: i32) -> Result<Attachment> {
        let mut tx = self.begin().await?;
        sqlx::query(
            "insert into homestead.attachment_purges (storage_key, attachment, purge_after)
             select storage_key, to_jsonb(a), now() + make_interval(days => $2)
             from homestead.attachments a where id = $1",
        )
        .bind(id)
        .bind(retain_days)
        .execute(&mut *tx)
        .await?;
        let row: Option<Row> =
            sqlx::query_as(concat!("delete from homestead.attachments where id = $1 returning ", attachment_cols!()))
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        tx.commit().await?;
        attachment(row.ok_or(StoreError::NotFound("attachment"))?)
    }

    /// Queues bytes that were stored but never recorded (the record's
    /// insert failed) for purging after `retain_days`.
    pub async fn queue_unrecorded_purge(&self, storage_key: &str, retain_days: i32) -> Result<()> {
        let mut tx = self.begin().await?;
        sqlx::query(
            "insert into homestead.attachment_purges (storage_key, purge_after)
             values ($1, now() + make_interval(days => $2)) on conflict (storage_key) do nothing",
        )
        .bind(storage_key)
        .bind(retain_days)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Storage keys whose retention has run out, oldest first.
    pub async fn due_blob_purges(&self, limit: i64) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "select storage_key from homestead.attachment_purges where purge_after <= now()
             order by purge_after limit $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Every storage key the database still accounts for: live attachments
    /// and those waiting to be purged. Anything else in the bucket is an
    /// orphan; anything here missing from it is lost.
    pub async fn accounted_blobs(&self) -> Result<Vec<(String, Option<String>)>> {
        Ok(sqlx::query_as(
            "select storage_key, sha256 from homestead.attachments
             union all
             select storage_key, attachment->>'sha256' from homestead.attachment_purges
             order by 1",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Forgets a purge once its bytes are gone from storage.
    pub async fn blob_purged(&self, storage_key: &str) -> Result<()> {
        let mut tx = self.begin().await?;
        sqlx::query("delete from homestead.attachment_purges where storage_key = $1 and purge_after <= now()")
            .bind(storage_key)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Undoes a delete whose bytes haven't been purged yet. Fails if the
    /// record it was attached to is gone too.
    pub async fn restore_attachment(&self, id: Uuid) -> Result<Attachment> {
        let mut tx = self.begin().await?;
        let snapshot: Option<(String, Uuid)> = sqlx::query_as(
            "select attachment->>'resource_type', (attachment->>'resource_id')::uuid
             from homestead.attachment_purges where attachment->>'id' = $1::text for update",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let (owner, owner_id) = snapshot.ok_or(StoreError::NotFound("deleted attachment awaiting purge"))?;
        if !owner_exists(&mut tx, parse(&owner)?, owner_id).await? {
            return Err(StoreError::NotFound("record the attachment belonged to"));
        }
        let row: Row = sqlx::query_as(concat!(
            "with gone as (
               delete from homestead.attachment_purges where attachment->>'id' = $1::text returning attachment
             )
             insert into homestead.attachments
             select (jsonb_populate_record(null::homestead.attachments, attachment)).* from gone
             returning ",
            attachment_cols!()
        ))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        attachment(row)
    }
}

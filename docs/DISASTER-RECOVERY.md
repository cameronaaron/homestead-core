# Disaster recovery

## Objectives and ownership

- Target recovery point (RPO): 24 hours until continuous/PITR backups are enabled.
- Target recovery time (RTO): 4 hours.
- The Steward owns recovery; the Farm Lead is the alternate. The person
  restoring must not be the only person validating the recovered service.
- Database dumps contain personal, donor, financial and operational data.
  Encrypt them at rest, restrict access, and never commit `backups/`.
- Attachment objects are a separate backup set. R2 has no object versions:
  the recovery copy is `homestead-attachments-backup`, filled nightly by
  `.github/workflows/backup.yml` and locked for 90 days. Verify every object
  against `homestead.attachments.sha256` after recovery.

## Backup and proof

1. Run `just backup` against the direct or session-pooler `DATABASE_URL`.
   It creates a private custom-format dump plus a SHA-256 sidecar. The dump
   holds the `homestead` schema and `public` (sqlx's migration history)
   only: Supabase's own schemas need extensions that exist only on
   Supabase (`supabase_vault`), and a whole-database dump restores nowhere
   else. `scripts/pg.sh` runs PostgreSQL 17 client tools from `PATH`, or
   in the `postgres:17-alpine` image (Docker, or Apple `container`; a
   localhost URL on this Mac runs them inside `sunbrewed-pg`).
2. **Nightly, automatically** (`.github/workflows/backup.yml`, 03:41 Pacific):
   the workflow dumps production, encrypts the dump with `age` to the
   public key in `scripts/backup-age-recipient.txt`, and keeps it as a
   workflow artifact for **90 days** (GitHub's maximum). Decrypt with the
   private key in `~/.local/share/sunbrewed/homestead/backup-age-key.txt`
   (back that file up somewhere else too: without it the artifacts are
   unreadable). This is the 24-hour RPO. It does **not** yet give the 5
   weekly and 12 monthly points older than 90 days: keep those by
   downloading an artifact (`gh run download`) into encrypted storage.
3. **Weekly** (Sundays, and on any manual run), the workflow runs
   `scripts/restore-test.sh` on the night's dump: it checks the checksum,
   restores into an isolated PostgreSQL 17, requires the same tables and
   migrations the source has, and records the drill in
   `homestead.restore_tests`. By hand: `just restore-test backups/<file>.dump`.
4. **Alerts.** The same job then reads production's `/api/v1/metrics` with
   `scripts/check-metrics.sh` and fails, which emails the repository owner,
   on dead-lettered deliveries, any 5xx (the core's or the Worker's), no
   drill ever, or a drill older than 8 days. `deploy/prometheus/alerts.yml`
   holds the same limits for a Prometheus, if one is ever pointed at the
   metrics. A backup that never ran shows up as no email from a *successful*
   run, so also look at the Actions page after any outage of GitHub.

## Incident procedure

1. Declare the incident, name the incident lead and recorder, and stop writes.
2. Preserve logs, the affected database, and object-storage versions. Record
   timestamps and hashes; do not experiment on the only copy.
3. Choose the newest recovery point before the failure or compromise. Verify
   its SHA-256 sidecar.
4. Restore into a new database/project. Never restore over the affected
   database. Apply no migrations until the restored schema version is known.
5. Validate table counts, foreign keys, latest operational records, event and
   delivery queues, audit continuity, Zeffy event IDs, and attachment hashes.
6. Point a staging instance at the restored database. Exercise login, reads,
   one idempotent write replay, and outbound delivery with external sinks
   disabled or redirected to test endpoints.
7. Have the second operator approve cutover. Rotate database, admin, webhook,
   API and object-storage credentials if compromise is possible.
8. Cut over, re-enable writes, then integrations one at a time. Watch health,
   error rate, pending/dead deliveries and database saturation.
9. Record the actual RPO/RTO, lost or replayed records, cause, and corrective
   actions in the incident log and append any recovery evidence to the audit.

## Attachment recovery

- **One file deleted by mistake** (within `retain_deleted_days`, 30):
  `homestead attachment-restore <attachment id>`. The id is in the audit log
  (`resource_type = 'attachments'`, `action = 'delete'`).
- **Objects missing from the primary bucket** (a bad delete, a bug): run the
  Backup workflow with `--restore`, or `just attachments-backup --restore`.
  It copies each missing key back from the backup after checking its SHA-256.
- **After restoring the database from an older dump**: the dump knows fewer
  files than the buckets hold. Run `just attachments-backup --verify`
  *without* `--prune` first. The newer files show as orphans in primary;
  decide per file before anything is pruned. The nightly prune refuses on
  its own if more than a tenth of the backup would go, but a smaller gap is
  within that limit, so pause the Backup workflow until you have decided.
- **Lost Cloudflare account**: both buckets go with it. Until an off-account
  copy exists, this loses attachment bytes (the database still has names,
  owners and hashes).

## Zeffy reconciliation

Webhook delivery is at-least-once. `homestead.zeffy_webhook_events.id` is the
deduplication boundary. After an outage, compare payments created during the
gap using Zeffy's read API with stored webhook event payloads. Insert or replay
only missing stable event IDs; do not key reconciliation by timestamps alone.

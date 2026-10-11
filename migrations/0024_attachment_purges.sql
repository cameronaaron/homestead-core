-- Deleting an attachment removes its record at once but its bytes only
-- after a retention period. The production bucket's lock refuses deletes
-- of young objects (R2 has no object versioning; the lock is its
-- retention), so an immediate delete would fail anyway, and until the
-- purge a mistaken delete can be undone from the snapshot below.

create table homestead.attachment_purges (
  storage_key text primary key check (storage_key ~ '^att/[0-9a-f]{64}$'),
  -- The deleted record, for `attachment-restore`. Null for bytes that were
  -- stored but never recorded (the upload's insert failed).
  attachment jsonb,
  queued_at timestamptz not null default now(),
  purge_after timestamptz not null
);
create index attachment_purges_due on homestead.attachment_purges (purge_after);

create trigger audit_attachment_purges after insert or update or delete on homestead.attachment_purges
  for each row execute function homestead.audit_row_change();

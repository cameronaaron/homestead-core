-- Finishes the attachments table from 0003: one table for every kind of
-- file, owned by exactly one record, private to whoever may read that record.

-- No code wrote to it before this migration. Should a hand-made row
-- violate the constraints below, the migration fails rather than guess.
alter table homestead.attachments
  alter column resource_id type uuid using resource_id::uuid,
  add column created_by uuid references homestead.actors (id),
  add constraint attachments_resource_type_check check (resource_type in
    ('animal', 'animal_care', 'purchase', 'gift', 'grant_campaign')),
  add constraint attachments_media_type_check check (media_type in
    ('image/jpeg', 'image/png', 'image/gif', 'image/webp', 'image/heic', 'application/pdf')),
  -- Unpredictable keys: 256 random bits, never derived from names or ids.
  add constraint attachments_storage_key_check check (storage_key ~ '^att/[0-9a-f]{64}$'),
  add constraint attachments_byte_size_positive check (byte_size > 0),
  add constraint attachments_original_name_check check (length(original_name) between 1 and 120);

-- Legal releases (liability waiver, NDA, photo release) sent for e-signature
-- through Documenso. The PDF and signing fields live in the repo
-- (documenso/releases.toml); this table records who was asked to sign what
-- and how it ended. Email addresses stay here, never in outbox events.

create table homestead.release_requests (
  id            uuid primary key,
  release_key   text not null check (length(btrim(release_key)) > 0),
  title         text not null check (length(btrim(title)) > 0),
  signer_name   text not null check (length(btrim(signer_name)) > 0),
  signer_email  text not null check (signer_email ~ '^[^@\s]+@[^@\s]+\.[^@\s]+$'),
  envelope_id   text not null unique,
  status        text not null default 'sent' check (status in ('sent', 'signed', 'declined')),
  sent_at       timestamptz not null default now(),
  completed_at  timestamptz,
  -- completed_at is set exactly when the request has an outcome.
  check ((status = 'sent') = (completed_at is null))
);
create index on homestead.release_requests (lower(signer_email));
create index on homestead.release_requests (status, sent_at desc);

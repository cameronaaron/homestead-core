-- Joining from a link (`/join`, shared on Instagram): no Discord command,
-- no account. The person gives a name and email, signs the welcome
-- agreement, and only then gets a single-use Discord invite that grants the
-- role as they join. Discord doesn't tell a bot which invite someone used,
-- so a join is never tied to a Discord account here; `systems-sync` writes
-- their username into discord/server.toml once they hold the role.
create table homestead.joins (
  id                 uuid primary key,
  -- sha256 of the token in their /join/<token> link, which is theirs alone.
  token_hash         text not null unique check (token_hash ~ '^[0-9a-f]{64}$'),
  name               text not null check (length(btrim(name)) between 1 and 80),
  email              text not null check (length(email) between 3 and 254 and email like '%_@_%'),
  role               text not null check (role in ('Community', 'Volunteer')),
  created_at         timestamptz not null default now(),
  -- Set once Documenso has the agreement, or why it couldn't be sent.
  release_request_id uuid unique references homestead.release_requests (id),
  send_error         text check (length(send_error) <= 300),
  -- The invite made after they signed, and when (they expire).
  invite_code        text check (invite_code ~ '^[A-Za-z0-9-]{2,32}$'),
  invite_created_at  timestamptz,
  check ((invite_code is null) = (invite_created_at is null))
);

create trigger audit_joins after insert or update or delete on homestead.joins
  for each row execute function homestead.audit_row_change();

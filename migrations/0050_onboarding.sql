-- Joining through Discord: `/onboard` sends someone the welcome agreement and
-- records which Discord member asked, and for which role. When a agreement
-- sent for them comes back signed, the role is granted (the
-- `member.onboarded` event; the `discord-roles` sink grants it, with the
-- outbox's retries).

-- Who a request was sent for, when it came from Discord. Any signed one
-- completes their onboarding, so running /onboard twice can't strand them
-- on the envelope they didn't sign.
alter table homestead.release_requests
  add column discord_user_id text check (discord_user_id ~ '^[0-9]{5,20}$');
create index on homestead.release_requests (discord_user_id) where discord_user_id is not null;

create table homestead.onboardings (
  discord_user_id  text primary key check (discord_user_id ~ '^[0-9]{5,20}$'),
  discord_username text not null check (length(btrim(discord_username)) between 1 and 40),
  role             text not null check (role in ('Community', 'Volunteer')),
  started_at       timestamptz not null default now(),
  -- When a agreement came back signed; the role follows by event.
  completed_at     timestamptz
);

create trigger audit_onboardings after insert or update or delete on homestead.onboardings
  for each row execute function homestead.audit_row_change();

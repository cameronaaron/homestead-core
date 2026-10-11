-- Per-person sign-in, machine credentials, and who-did-what in the audit log.
--
-- `actors` (0003) becomes the one identity table: people sign in with an
-- email and password, services with an API token. Roles map to capabilities
-- in code (domain::auth), so the database stores only role names.

alter table homestead.actors
  add column kind text not null default 'human' check (kind in ('human', 'service')),
  add column email text;
create unique index actors_email on homestead.actors (email) where email is not null;
alter table homestead.actors
  add constraint actors_email_lowercase check (email = lower(btrim(email))),
  add constraint actors_people_have_email check ((kind = 'human') = (email is not null));

-- Remap the placeholder roles from 0003 to the organization's own names.
alter table homestead.actor_roles drop constraint actor_roles_role_check;
update homestead.actor_roles set role = case role
  when 'admin' then 'steward' when 'treasurer' then 'board' when 'worker' then 'crew'
  when 'viewer' then 'volunteer' else role end;
alter table homestead.actor_roles add constraint actor_roles_role_check
  check (role in ('steward', 'board', 'farm_lead', 'crew', 'volunteer', 'service'));

-- Secrets and lockout counters live apart from `actors`, which is audited
-- row by row: a password hash must never be copied into audit history.
create table homestead.actor_credentials (
  actor_id uuid primary key references homestead.actors (id) on delete cascade,
  password_hash text not null check (password_hash like '$argon2id$%'),
  set_at timestamptz not null default now(),
  failed_logins integer not null default 0 check (failed_logins >= 0),
  locked_until timestamptz
);

-- Cookie sessions. Only a SHA-256 of the cookie value is stored, so a
-- database read (or backup) can't be replayed as a sign-in.
create table homestead.sessions (
  token_hash text primary key check (token_hash ~ '^[0-9a-f]{64}$'),
  actor_id uuid not null references homestead.actors (id) on delete cascade,
  created_at timestamptz not null default now(),
  expires_at timestamptz not null,
  last_seen_at timestamptz not null default now(),
  check (expires_at > created_at)
);
create index sessions_actor on homestead.sessions (actor_id);

-- One-time links a steward hands someone to set (or reset) their password.
create table homestead.setup_links (
  token_hash text primary key check (token_hash ~ '^[0-9a-f]{64}$'),
  actor_id uuid not null references homestead.actors (id) on delete cascade,
  created_by uuid references homestead.actors (id),
  created_at timestamptz not null default now(),
  expires_at timestamptz not null,
  used_at timestamptz,
  check (expires_at > created_at)
);
create index setup_links_actor on homestead.setup_links (actor_id);

-- API bearer tokens, each owned by a service actor.
create table homestead.api_tokens (
  id uuid primary key default gen_random_uuid(),
  actor_id uuid not null references homestead.actors (id),
  name text not null check (length(btrim(name)) > 0),
  token_hash text not null unique check (token_hash ~ '^[0-9a-f]{64}$'),
  created_by uuid references homestead.actors (id),
  created_at timestamptz not null default now(),
  last_used_at timestamptz,
  revoked_at timestamptz
);

-- The audit log names the actor, not just a label, and the label now comes
-- from the same transaction-local settings the application sets.
alter table homestead.audit_entries add column actor_id uuid references homestead.actors (id);
create index audit_entries_actor on homestead.audit_entries (actor_id, occurred_at desc) where actor_id is not null;

create or replace function homestead.audit_row_change() returns trigger language plpgsql as $$
declare old_row jsonb; new_row jsonb; row_id text;
begin
  old_row := case when tg_op = 'INSERT' then null else to_jsonb(old) end;
  new_row := case when tg_op = 'DELETE' then null else to_jsonb(new) end;
  row_id := coalesce(new_row ->> 'id', old_row ->> 'id');
  insert into homestead.audit_entries
    (actor_label, actor_id, request_id, action, resource_type, resource_id, before, after)
  values
    (coalesce(nullif(current_setting('homestead.actor', true), ''), 'system'),
     nullif(current_setting('homestead.actor_id', true), '')::uuid,
     nullif(current_setting('homestead.request_id', true), ''), lower(tg_op),
     tg_table_name, row_id, old_row, new_row);
  return coalesce(new, old);
end $$;

-- Who holds which role, and who granted it, is itself audited.
do $$
declare t text;
begin
  foreach t in array array['actors', 'actor_roles', 'api_tokens']
  loop
    execute format('create trigger audit_%I after insert or update or delete on homestead.%I for each row execute function homestead.audit_row_change()', t, t);
  end loop;
end $$;

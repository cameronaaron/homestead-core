-- Accountability, recovery evidence, and farm-operations records.
create table homestead.actors (
  id uuid primary key default gen_random_uuid(),
  external_subject text unique,
  display_name text not null check (length(btrim(display_name)) > 0),
  disabled_at timestamptz,
  created_at timestamptz not null default now()
);
create table homestead.actor_roles (
  actor_id uuid not null references homestead.actors (id) on delete cascade,
  role text not null check (role in ('viewer', 'worker', 'farm_lead', 'treasurer', 'admin')),
  granted_by uuid references homestead.actors (id),
  granted_at timestamptz not null default now(),
  primary key (actor_id, role)
);

create table homestead.audit_entries (
  id bigint generated always as identity primary key,
  occurred_at timestamptz not null default now(),
  actor_label text not null default 'system',
  request_id text,
  action text not null,
  resource_type text not null,
  resource_id text,
  before jsonb,
  after jsonb,
  metadata jsonb not null default '{}'::jsonb
);
create index audit_entries_lookup on homestead.audit_entries (resource_type, resource_id, occurred_at desc);
create index audit_entries_time on homestead.audit_entries (occurred_at desc);
create function homestead.reject_audit_mutation() returns trigger language plpgsql as $$
begin raise exception 'audit entries are append-only'; end $$;
create trigger audit_entries_immutable before update or delete on homestead.audit_entries
  for each row execute function homestead.reject_audit_mutation();

create function homestead.audit_row_change() returns trigger language plpgsql as $$
declare old_row jsonb; new_row jsonb; row_id text;
begin
  old_row := case when tg_op = 'INSERT' then null else to_jsonb(old) end;
  new_row := case when tg_op = 'DELETE' then null else to_jsonb(new) end;
  row_id := coalesce(new_row ->> 'id', old_row ->> 'id');
  insert into homestead.audit_entries
    (actor_label, request_id, action, resource_type, resource_id, before, after)
  values
    (coalesce(nullif(current_setting('homestead.actor', true), ''), 'system'),
     nullif(current_setting('homestead.request_id', true), ''), lower(tg_op),
     tg_table_name, row_id, old_row, new_row);
  return coalesce(new, old);
end $$;

create table homestead.idempotency_records (
  scope text not null,
  key text not null check (length(key) between 1 and 200),
  request_hash text not null,
  response_status smallint,
  response_headers jsonb,
  response_body bytea,
  created_at timestamptz not null default now(),
  completed_at timestamptz,
  primary key (scope, key),
  check ((response_status is null) = (completed_at is null))
);
create index idempotency_records_expiry on homestead.idempotency_records (created_at);

create table homestead.seed_lots (
  id uuid primary key default gen_random_uuid(),
  crop text not null check (length(btrim(crop)) > 0),
  variety text not null default '',
  supplier text not null default '',
  supplier_lot text not null default '',
  acquired_on date,
  packed_for_year integer check (packed_for_year between 1900 and 3000),
  organic boolean,
  notes text not null default '',
  created_at timestamptz not null default now()
);
create unique index seed_lots_supplier_lot on homestead.seed_lots (supplier, supplier_lot)
  where supplier_lot <> '';
alter table homestead.plantings add column seed_lot_id uuid references homestead.seed_lots (id);
create index on homestead.plantings (seed_lot_id);

create table homestead.vendors (
  id uuid primary key default gen_random_uuid(),
  name text not null unique check (length(btrim(name)) > 0),
  contact jsonb not null default '{}'::jsonb,
  notes text not null default '',
  created_at timestamptz not null default now()
);
create table homestead.purchases (
  id uuid primary key default gen_random_uuid(),
  vendor_id uuid references homestead.vendors (id),
  purchased_on date not null,
  amount numeric(12,2) not null check (amount >= 0),
  currency text not null default 'usd' check (currency in ('usd', 'cad')),
  category text not null check (length(btrim(category)) > 0),
  description text not null default '',
  external_reference text,
  created_at timestamptz not null default now()
);
create index purchases_date on homestead.purchases (purchased_on desc);

create table homestead.recurring_task_templates (
  id uuid primary key default gen_random_uuid(),
  title text not null check (length(btrim(title)) > 0),
  details text not null default '',
  location_id uuid references homestead.locations (id),
  cadence_days integer not null check (cadence_days > 0),
  next_due_on date not null,
  priority text not null default 'normal' check (priority in ('low', 'normal', 'high', 'urgent')),
  enabled boolean not null default true,
  created_at timestamptz not null default now()
);
create table homestead.weather_records (
  id bigint generated always as identity primary key,
  observed_at timestamptz not null,
  source text not null check (length(btrim(source)) > 0),
  temperature_c numeric(6,2),
  precipitation_mm numeric(10,2) check (precipitation_mm is null or precipitation_mm >= 0),
  wind_kph numeric(8,2) check (wind_kph is null or wind_kph >= 0),
  conditions text not null default '',
  raw jsonb not null default '{}'::jsonb,
  unique (source, observed_at)
);
create table homestead.irrigation_records (
  id uuid primary key default gen_random_uuid(),
  location_id uuid not null references homestead.locations (id),
  started_at timestamptz not null,
  ended_at timestamptz,
  water_liters numeric(12,3) check (water_liters is null or water_liters >= 0),
  method text not null default '',
  notes text not null default '',
  created_at timestamptz not null default now(),
  check (ended_at is null or ended_at >= started_at)
);
-- Object bytes live outside Postgres; provenance and integrity live here.
create table homestead.attachments (
  id uuid primary key default gen_random_uuid(),
  resource_type text not null,
  resource_id text not null,
  storage_key text not null unique,
  original_name text not null,
  media_type text not null,
  byte_size bigint not null check (byte_size >= 0),
  sha256 text not null check (sha256 ~ '^[0-9a-f]{64}$'),
  captured_at timestamptz,
  created_at timestamptz not null default now()
);
create index attachments_resource on homestead.attachments (resource_type, resource_id);

create table homestead.zeffy_webhook_events (
  id uuid primary key,
  event_type text not null,
  schema_version integer not null,
  dispatched_at timestamptz not null,
  payload jsonb not null,
  received_at timestamptz not null default now()
);
create table homestead.restore_tests (
  id bigint generated always as identity primary key,
  backup_name text not null,
  backup_sha256 text not null check (backup_sha256 ~ '^[0-9a-f]{64}$'),
  postgres_version text not null,
  tested_at timestamptz not null default now(),
  integrity_ok boolean not null,
  notes text not null default ''
);

do $$
declare t text;
begin
  foreach t in array array['locations','plantings','harvests','tasks','inventory_items','stock_movements',
    'seed_lots','vendors','purchases','recurring_task_templates','weather_records','irrigation_records',
    'attachments','zeffy_webhook_events','restore_tests']
  loop
    execute format('create trigger audit_%I after insert or update or delete on homestead.%I for each row execute function homestead.audit_row_change()', t, t);
  end loop;
end $$;

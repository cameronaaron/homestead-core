-- Core farm-operations schema.
--
-- Everything lives in the `homestead` schema, NOT `public`: Supabase exposes
-- `public` through its Data API (PostgREST) to anyone holding the publishable
-- key, and new tables there are granted to `anon` by default. A separate
-- schema is reachable only through this service's own connection.

create schema if not exists homestead;

-- The migration ledger sqlx creates in the default schema must not be
-- writable through the Data API either (a tampered checksum bricks startup).
do $$
begin
  if exists (select 1 from pg_roles where rolname = 'anon')
     and to_regclass('public._sqlx_migrations') is not null then
    execute 'revoke all on table public._sqlx_migrations from anon, authenticated';
  end if;
end $$;

create table homestead.locations (
  id          uuid primary key default gen_random_uuid(),
  name        text not null unique check (length(btrim(name)) > 0),
  kind        text not null check (kind in
                ('bed', 'field', 'greenhouse', 'orchard', 'pen', 'coop', 'storage', 'other')),
  notes       text not null default '',
  created_at  timestamptz not null default now()
);

create table homestead.plantings (
  id                   uuid primary key default gen_random_uuid(),
  location_id          uuid not null references homestead.locations (id),
  crop                 text not null check (length(btrim(crop)) > 0),
  variety              text not null default '',
  sown_on              date not null,
  expected_harvest_on  date,
  status               text not null default 'growing' check (status in
                         ('planned', 'growing', 'harvesting', 'finished', 'failed')),
  notes                text not null default '',
  created_at           timestamptz not null default now(),
  check (expected_harvest_on is null or expected_harvest_on >= sown_on)
);
create index on homestead.plantings (location_id);

create table homestead.harvests (
  id            uuid primary key default gen_random_uuid(),
  planting_id   uuid not null references homestead.plantings (id),
  harvested_on  date not null,
  quantity      numeric(12, 3) not null check (quantity > 0),
  unit          text not null check (unit in ('lb', 'oz', 'kg', 'g', 'each', 'bunch', 'dozen')),
  notes         text not null default '',
  created_at    timestamptz not null default now()
);
create index on homestead.harvests (planting_id);

create table homestead.tasks (
  id               uuid primary key default gen_random_uuid(),
  title            text not null check (length(btrim(title)) > 0),
  details          text not null default '',
  location_id      uuid references homestead.locations (id),
  due_on           date,
  priority         text not null default 'normal' check (priority in ('low', 'normal', 'high', 'urgent')),
  status           text not null default 'open' check (status in ('open', 'done', 'cancelled')),
  mirror_to_linear boolean not null default false,
  linear_issue_id  text,
  completed_at     timestamptz,
  created_at       timestamptz not null default now(),
  -- completed_at is set exactly when the task is done.
  check ((status = 'done') = (completed_at is not null))
);
create index on homestead.tasks (status, due_on);

create table homestead.inventory_items (
  id             uuid primary key default gen_random_uuid(),
  name           text not null unique check (length(btrim(name)) > 0),
  unit           text not null check (length(btrim(unit)) > 0),
  reorder_level  numeric(12, 3) not null default 0 check (reorder_level >= 0),
  -- Denormalised running total of stock_movements, maintained in the same
  -- transaction as each movement; the check makes negative stock unrepresentable.
  on_hand        numeric(12, 3) not null default 0 check (on_hand >= 0),
  created_at     timestamptz not null default now()
);

create table homestead.stock_movements (
  id        bigint generated always as identity primary key,
  item_id   uuid not null references homestead.inventory_items (id),
  delta     numeric(12, 3) not null check (delta <> 0),
  reason    text not null check (length(btrim(reason)) > 0),
  at        timestamptz not null default now()
);
create index on homestead.stock_movements (item_id, at);

-- ── Integration bus: transactional outbox ──────────────────────────────────
--
-- Domain writes append to `events` inside their own transaction, so an event
-- exists iff its change committed. The dispatcher later fans each event out
-- into one `deliveries` row per interested sink; each delivery retries and
-- dead-letters independently, so a broken integration never blocks others.

create table homestead.events (
  id             bigint generated always as identity primary key,
  kind           text not null,
  payload        jsonb not null,
  occurred_at    timestamptz not null default now(),
  fanned_out_at  timestamptz
);
create index events_unfanned on homestead.events (id) where fanned_out_at is null;

create table homestead.deliveries (
  event_id         bigint not null references homestead.events (id) on delete cascade,
  sink             text not null,
  status           text not null default 'pending' check (status in ('pending', 'delivered', 'dead')),
  attempts         integer not null default 0 check (attempts >= 0),
  next_attempt_at  timestamptz not null default now(),
  last_error       text,
  delivered_at     timestamptz,
  primary key (event_id, sink)
);
create index deliveries_due on homestead.deliveries (next_attempt_at) where status = 'pending';

-- Wake the dispatcher immediately instead of waiting for its poll interval.
-- Polling remains the correctness path; NOTIFY is only a latency optimisation
-- (it does not survive Supabase's transaction pooler).
create function homestead.notify_event() returns trigger language plpgsql as $$
begin
  perform pg_notify('homestead_events', new.id::text);
  return new;
end $$;

create trigger events_notify after insert on homestead.events
  for each row execute function homestead.notify_event();

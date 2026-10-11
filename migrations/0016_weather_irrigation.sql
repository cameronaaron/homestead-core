-- Weather and irrigation (from 0003), recordable by people now and by
-- sensors later without one overwriting the other.

alter table homestead.irrigation_records
  add column planting_id uuid references homestead.plantings (id),
  add column source text not null default 'manual' check (length(btrim(source)) > 0),
  add column external_id text,
  add constraint irrigation_method_check check (method in
    ('drip', 'sprinkler', 'hand', 'soaker', 'flood', 'other')),
  -- A machine's retried report is recorded once; people have no ids.
  add constraint irrigation_external_id_is_machine_only check (external_id is null or source <> 'manual');
alter table homestead.irrigation_records alter column method drop default;
create unique index irrigation_records_external on homestead.irrigation_records (source, external_id)
  where external_id is not null;
create index irrigation_records_started on homestead.irrigation_records (started_at desc);
create index irrigation_records_location on homestead.irrigation_records (location_id, started_at desc);
create index irrigation_records_planting on homestead.irrigation_records (planting_id) where planting_id is not null;

alter table homestead.weather_records
  add column location_id uuid references homestead.locations (id),
  add column humidity_pct numeric(5, 2) check (humidity_pct is null or humidity_pct between 0 and 100),
  add column created_at timestamptz not null default now();
create index weather_records_observed on homestead.weather_records (observed_at desc);

-- Readings: numbers taken off one thing on the farm (a compost pile's
-- temperature, a bed's soil moisture, what the flock ate) without a table
-- per measurement. `kind` is free text the domain lower-cases so readings
-- group; each value keeps the unit it was taken in and is never converted.
-- The typed records (harvests, soil tests, egg days, irrigation) stay the
-- way to record what they cover.

create table homestead.readings (
  id uuid primary key default gen_random_uuid(),
  observed_at timestamptz not null,
  kind text not null check (length(btrim(kind)) between 1 and 60 and kind = lower(kind)),
  location_id uuid references homestead.locations (id),
  planting_id uuid references homestead.plantings (id),
  animal_id uuid references homestead.animals (id),
  equipment_id uuid references homestead.equipment (id),
  notes text not null default '' check (length(notes) <= 500),
  created_at timestamptz not null default now(),
  -- One thing per reading: who may see it follows from what it measured.
  check (num_nonnulls(location_id, planting_id, animal_id, equipment_id) = 1)
);
create index readings_kind on homestead.readings (kind, observed_at desc);
create index readings_location on homestead.readings (location_id, observed_at desc) where location_id is not null;
create index readings_planting on homestead.readings (planting_id, observed_at desc) where planting_id is not null;
create index readings_animal on homestead.readings (animal_id, observed_at desc) where animal_id is not null;
create index readings_equipment on homestead.readings (equipment_id, observed_at desc) where equipment_id is not null;

create table homestead.reading_values (
  reading_id uuid not null references homestead.readings (id) on delete cascade,
  position smallint not null check (position between 0 and 19),
  name text not null check (length(btrim(name)) between 1 and 60),
  value numeric(14, 4) not null,
  unit text not null default '' check (length(unit) <= 20),
  primary key (reading_id, position)
);
create unique index reading_values_name on homestead.reading_values (reading_id, lower(name));

create trigger audit_readings after insert or update or delete on homestead.readings
  for each row execute function homestead.audit_row_change();
create trigger audit_reading_values after insert or update or delete on homestead.reading_values
  for each row execute function homestead.audit_row_change();

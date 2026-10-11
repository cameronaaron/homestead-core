-- Crop plans, soil, crop health, irrigation zones, harvest disposition,
-- produce donations, flock records, compost batches, checklists, losses,
-- compliance obligations, emergency procedures, and cost links.
-- Operational records only. Accounting, payments, and forecasting stay outside.

alter table homestead.plantings
  add column crop_plan_id uuid,
  add column family text not null default '',
  add column method text not null default 'direct' check (method in ('direct', 'transplant')),
  add column expected_yield numeric(12, 3) check (expected_yield is null or expected_yield > 0),
  add column expected_yield_unit text check (expected_yield_unit is null or expected_yield_unit in
    ('lb', 'oz', 'kg', 'g', 'each', 'bunch', 'dozen')),
  add constraint plantings_yield_has_unit check ((expected_yield is null) = (expected_yield_unit is null));

create table homestead.crop_plans (
  id uuid primary key default gen_random_uuid(),
  crop text not null check (length(btrim(crop)) > 0),
  variety text not null default '',
  family text not null default '',
  notes text not null default '',
  created_at timestamptz not null default now()
);
alter table homestead.plantings
  add constraint plantings_crop_plan_fk foreign key (crop_plan_id) references homestead.crop_plans (id);
create index plantings_plan on homestead.plantings (crop_plan_id) where crop_plan_id is not null;
create index plantings_family on homestead.plantings (location_id, family) where family <> '';

create table homestead.soil_tests (
  id uuid primary key default gen_random_uuid(),
  location_id uuid not null references homestead.locations (id),
  tested_on date not null,
  ph numeric(4, 2),
  nitrogen text not null default '',
  phosphorus text not null default '',
  potassium text not null default '',
  organic_matter text not null default '',
  lab text not null default '',
  measurements jsonb not null default '{}'::jsonb,
  notes text not null default '',
  created_at timestamptz not null default now()
);
create index soil_tests_location on homestead.soil_tests (location_id, tested_on desc);

create table homestead.soil_amendments (
  id uuid primary key default gen_random_uuid(),
  location_id uuid not null references homestead.locations (id),
  applied_on date not null,
  material text not null check (length(btrim(material)) > 0),
  amount numeric(12, 3) check (amount is null or amount > 0),
  unit text not null default '',
  reason text not null default '',
  cost numeric(12, 2) check (cost is null or cost >= 0),
  notes text not null default '',
  created_at timestamptz not null default now(),
  check ((amount is null) = (unit = ''))
);
create index soil_amendments_location on homestead.soil_amendments (location_id, applied_on desc);

create table homestead.crop_observations (
  id uuid primary key default gen_random_uuid(),
  observed_on date not null,
  location_id uuid references homestead.locations (id),
  planting_id uuid references homestead.plantings (id),
  category text not null check (category in ('pest', 'disease', 'nutrient', 'water', 'weather', 'other')),
  observation text not null check (length(btrim(observation)) > 0),
  severity text not null default 'low' check (severity in ('low', 'moderate', 'high')),
  affected text not null default '',
  action_taken text not null default '',
  follow_up_on date,
  outcome text not null default '',
  created_at timestamptz not null default now(),
  check (location_id is not null or planting_id is not null)
);
create index crop_observations_place on homestead.crop_observations (location_id, observed_on desc);
create index crop_observations_planting on homestead.crop_observations (planting_id, observed_on desc);

create table homestead.irrigation_zones (
  id uuid primary key default gen_random_uuid(),
  name text not null unique check (length(btrim(name)) > 0),
  notes text not null default '',
  created_at timestamptz not null default now()
);
create table homestead.irrigation_zone_locations (
  zone_id uuid not null references homestead.irrigation_zones (id) on delete cascade,
  location_id uuid not null references homestead.locations (id),
  primary key (zone_id, location_id)
);
alter table homestead.irrigation_records
  add column zone_id uuid references homestead.irrigation_zones (id),
  add column problem text check (problem is null or problem in
    ('leak', 'blocked', 'broken_line', 'low_pressure', 'controller', 'other'));
alter table homestead.weather_records
  add column soil_temperature_c numeric(6, 2),
  add column frost boolean not null default false;

create table homestead.harvest_dispositions (
  id uuid primary key default gen_random_uuid(),
  harvest_id uuid not null references homestead.harvests (id),
  kind text not null check (kind in ('sale', 'donation', 'community', 'feed', 'seed', 'loss', 'compost')),
  quantity numeric(12, 3) not null check (quantity > 0),
  unit text not null,
  notes text not null default '',
  created_at timestamptz not null default now()
);
create index harvest_dispositions_harvest on homestead.harvest_dispositions (harvest_id);

create function homestead.disposition_fits_harvest() returns trigger language plpgsql as $$
declare h_qty numeric; h_unit text; used numeric;
begin
  select quantity, unit into h_qty, h_unit from homestead.harvests where id = new.harvest_id;
  if h_unit is null then
    raise exception 'harvest not found';
  end if;
  if new.unit is distinct from h_unit then
    raise exception 'disposition unit must match the harvest unit';
  end if;
  select coalesce(sum(quantity), 0) into used from homestead.harvest_dispositions
    where harvest_id = new.harvest_id and id is distinct from new.id;
  if used + new.quantity > h_qty then
    raise exception 'dispositions exceed the harvest';
  end if;
  return new;
end $$;
create trigger harvest_dispositions_fit before insert or update on homestead.harvest_dispositions
  for each row execute function homestead.disposition_fits_harvest();

create table homestead.produce_donations (
  id uuid primary key default gen_random_uuid(),
  donated_on date not null,
  recipient text not null check (length(btrim(recipient)) > 0),
  product text not null check (length(btrim(product)) > 0),
  quantity numeric(12, 3) not null check (quantity > 0),
  unit text not null check (unit in ('lb', 'oz', 'kg', 'g', 'each', 'bunch', 'dozen')),
  disposition_id uuid references homestead.harvest_dispositions (id),
  operational_value numeric(12, 2) check (operational_value is null or operational_value >= 0),
  notes text not null default '',
  created_at timestamptz not null default now()
);
create index produce_donations_date on homestead.produce_donations (donated_on desc);

alter table homestead.animal_care drop constraint animal_care_kind_check;
alter table homestead.animal_care add constraint animal_care_kind_check check (kind in (
  'vaccine', 'spay_neuter', 'deworm', 'flea', 'vet_visit', 'medication', 'microchip', 'weight',
  'feeding', 'health_note', 'mortality', 'flock_count', 'hoof', 'enclosure', 'other'
));

alter table homestead.stock_movements
  add column animal_id uuid references homestead.animals (id);
create index stock_movements_animal on homestead.stock_movements (animal_id, at desc) where animal_id is not null;

create table homestead.egg_days (
  id uuid primary key default gen_random_uuid(),
  animal_id uuid not null references homestead.animals (id),
  on_date date not null,
  collected integer not null check (collected >= 0),
  damaged integer not null check (damaged >= 0),
  donated integer not null default 0 check (donated >= 0),
  internal_use integer not null default 0 check (internal_use >= 0),
  notes text not null default '',
  created_at timestamptz not null default now(),
  unique (animal_id, on_date),
  check (damaged + donated + internal_use <= collected)
);

create table homestead.compost_batches (
  id uuid primary key default gen_random_uuid(),
  name text not null check (length(btrim(name)) > 0),
  location_id uuid references homestead.locations (id),
  started_on date not null,
  inputs text not null default '',
  source text not null default '',
  status text not null default 'building' check (status in ('building', 'active', 'curing', 'finished')),
  finished_on date,
  result_quantity text not null default '',
  notes text not null default '',
  created_at timestamptz not null default now(),
  check (finished_on is null or finished_on >= started_on)
);
create table homestead.compost_turns (
  id uuid primary key default gen_random_uuid(),
  batch_id uuid not null references homestead.compost_batches (id),
  on_date date not null,
  temperature_c numeric(6, 2),
  moisture text not null default '',
  notes text not null default '',
  created_at timestamptz not null default now()
);
create index compost_turns_batch on homestead.compost_turns (batch_id, on_date desc);

create table homestead.checklist_templates (
  id uuid primary key default gen_random_uuid(),
  name text not null unique check (length(btrim(name)) > 0),
  notes text not null default '',
  created_at timestamptz not null default now()
);
create table homestead.checklist_template_items (
  id uuid primary key default gen_random_uuid(),
  template_id uuid not null references homestead.checklist_templates (id) on delete cascade,
  position integer not null check (position >= 0),
  label text not null check (length(btrim(label)) > 0),
  unique (template_id, position)
);
create table homestead.checklist_runs (
  id uuid primary key default gen_random_uuid(),
  template_id uuid not null references homestead.checklist_templates (id),
  on_date date not null,
  completed_by text not null default '',
  notes text not null default '',
  created_at timestamptz not null default now()
);
create table homestead.checklist_run_items (
  id uuid primary key default gen_random_uuid(),
  run_id uuid not null references homestead.checklist_runs (id) on delete cascade,
  label text not null,
  done boolean not null default false,
  note text not null default '',
  position integer not null
);
alter table homestead.recurring_task_templates
  add column checklist_template_id uuid references homestead.checklist_templates (id);

create table homestead.losses (
  id uuid primary key default gen_random_uuid(),
  lost_on date not null,
  reason text not null check (length(btrim(reason)) > 0),
  quantity numeric(12, 3) check (quantity is null or quantity > 0),
  unit text not null default '',
  harvest_id uuid references homestead.harvests (id),
  planting_id uuid references homestead.plantings (id),
  egg_day_id uuid references homestead.egg_days (id),
  notes text not null default '',
  created_at timestamptz not null default now(),
  check ((quantity is null) = (unit = ''))
);
create index losses_date on homestead.losses (lost_on desc);

create table homestead.obligations (
  id uuid primary key default gen_random_uuid(),
  requirement text not null check (length(btrim(requirement)) > 0),
  authority text not null default '',
  due_on date,
  owner_label text not null default '',
  status text not null default 'open' check (status in ('open', 'done', 'not_applicable')),
  completed_on date,
  external_reference text not null default '',
  notes text not null default '',
  created_at timestamptz not null default now()
);
create index obligations_due on homestead.obligations (status, due_on);

create table homestead.emergency_plans (
  id uuid primary key default gen_random_uuid(),
  scenario text not null unique check (length(btrim(scenario)) > 0),
  procedure text not null default '',
  roles text not null default '',
  contacts text not null default '',
  animals text not null default '',
  destination text not null default '',
  equipment text not null default '',
  reviewed_on date,
  created_at timestamptz not null default now()
);

create table homestead.cost_links (
  purchase_id uuid primary key references homestead.purchases (id),
  program text not null default '',
  location_id uuid references homestead.locations (id),
  planting_id uuid references homestead.plantings (id),
  animal_id uuid references homestead.animals (id),
  equipment_id uuid references homestead.equipment (id),
  event_id uuid references homestead.community_events (id)
);

alter table homestead.attachments drop constraint attachments_resource_type_check,
  add constraint attachments_resource_type_check check (resource_type in
    ('animal', 'animal_care', 'purchase', 'gift', 'grant_campaign', 'seed_lot', 'equipment', 'equipment_maintenance',
     'incident', 'soil_test', 'crop_observation', 'obligation', 'emergency_plan'));

do $$
declare t text;
begin
  foreach t in array array[
    'crop_plans', 'soil_tests', 'soil_amendments', 'crop_observations', 'irrigation_zones',
    'harvest_dispositions', 'produce_donations', 'egg_days', 'compost_batches', 'compost_turns',
    'checklist_templates', 'checklist_runs', 'losses', 'obligations', 'emergency_plans', 'cost_links'
  ]
  loop
    execute format('create trigger audit_%I after insert or update or delete on homestead.%I for each row execute function homestead.audit_row_change()', t, t);
  end loop;
end $$;

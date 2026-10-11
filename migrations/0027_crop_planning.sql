-- A crop plan's timing (so each succession gets its transplant date and
-- harvest window), its seed lot and expected yield, and tasks that know which
-- planting and step they are for, so ticking "Sow lettuce" marks it sown.

alter table homestead.crop_plans
  add column method text not null default 'direct' check (method in ('direct', 'transplant')),
  add column days_to_transplant integer check (days_to_transplant between 1 and 180),
  add column days_to_harvest integer check (days_to_harvest between 1 and 730),
  add column harvest_window_days integer check (harvest_window_days between 1 and 365),
  add column seed_lot_id uuid references homestead.seed_lots (id),
  add column expected_yield numeric(12, 3) check (expected_yield is null or expected_yield > 0),
  add column expected_yield_unit text check (expected_yield_unit is null or expected_yield_unit in
    ('lb', 'oz', 'kg', 'g', 'each', 'bunch', 'dozen')),
  add constraint crop_plans_yield_has_unit check ((expected_yield is null) = (expected_yield_unit is null)),
  add constraint crop_plans_transplant_timing check (days_to_transplant is null or method = 'transplant'),
  add constraint crop_plans_window_needs_start check (harvest_window_days is null or days_to_harvest is not null);
create index crop_plans_seed_lot on homestead.crop_plans (seed_lot_id) where seed_lot_id is not null;

alter table homestead.plantings
  add column transplant_on date,
  add column expected_harvest_until date,
  add constraint plantings_transplant_after_sowing check (transplant_on is null or transplant_on >= sown_on),
  add constraint plantings_window_is_ordered check (
    expected_harvest_until is null or (expected_harvest_on is not null and expected_harvest_until >= expected_harvest_on));

-- What needs doing: planned sowings by date, and harvests coming due.
create index plantings_agenda on homestead.plantings (status, sown_on);
create index plantings_harvest_due on homestead.plantings (status, expected_harvest_on) where expected_harvest_on is not null;

alter table homestead.tasks
  add column planting_id uuid references homestead.plantings (id),
  add column planting_step text check (planting_step in ('sow', 'transplant')),
  add constraint tasks_step_names_its_planting check ((planting_id is null) = (planting_step is null));
-- One sow task and one transplant task per planting, however often a plan is retried.
create unique index tasks_one_per_planting_step on homestead.tasks (planting_id, planting_step)
  where planting_id is not null;

-- A watering can be a zone's run (the zone serves several places), and a
-- problem found while watering opens a task to fix it. A zone can carry the
-- id a controller knows it by, so a future controller feed maps onto it.

alter table homestead.irrigation_records alter column location_id drop not null;
alter table homestead.irrigation_records
  add constraint irrigation_records_somewhere check (location_id is not null or zone_id is not null),
  add column problem_task_id uuid references homestead.tasks (id),
  add constraint irrigation_problem_task_needs_problem check (problem_task_id is null or problem is not null);
create index irrigation_records_zone on homestead.irrigation_records (zone_id, started_at desc) where zone_id is not null;
create index irrigation_records_problems on homestead.irrigation_records (started_at desc) where problem is not null;

alter table homestead.irrigation_zones
  add column controller_ref text not null default '',
  add column active boolean not null default true;
create unique index irrigation_zones_controller on homestead.irrigation_zones (controller_ref) where controller_ref <> '';
create index irrigation_zone_locations_location on homestead.irrigation_zone_locations (location_id);

-- Which places a zone serves is operational history too.
create trigger audit_irrigation_zone_locations after insert or update or delete on homestead.irrigation_zone_locations
  for each row execute function homestead.audit_row_change();

-- When a planting left its bed, so rotation history can say how long a place
-- has been free of a crop. Only an over planting has an end.

alter table homestead.plantings
  add column ended_on date,
  add constraint plantings_ends_after_sowing check (ended_on is null or ended_on >= sown_on),
  add constraint plantings_end_only_when_over check (ended_on is null or status in ('finished', 'failed'));

-- Existing finished plantings end on their last harvest when one was logged;
-- otherwise the end is unknown and stays null rather than being invented.
update homestead.plantings p set ended_on = h.last
  from (select planting_id, max(harvested_on) as last from homestead.harvests group by planting_id) h
  where h.planting_id = p.id and p.status in ('finished', 'failed') and h.last >= p.sown_on;

-- The location page lists a place's tasks.
create index tasks_location on homestead.tasks (location_id, created_at desc) where location_id is not null;

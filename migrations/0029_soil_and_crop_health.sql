-- Soil history by kind of amendment, soil tests whose extra measurements
-- stay whatever the lab reported, and crop-health observations that close
-- with an outcome (ticking off their follow-up task).

alter table homestead.soil_amendments
  add column kind text not null default 'other'
    check (kind in ('compost', 'fertilizer', 'lime', 'sulfur', 'mulch', 'other'));
update homestead.soil_amendments set kind = case
  when material ilike '%compost%' then 'compost'
  when material ilike '%lime%' then 'lime'
  when material ilike '%sulfur%' or material ilike '%sulphur%' then 'sulfur'
  when material ilike '%mulch%' then 'mulch'
  when material ilike '%fertili%' then 'fertilizer'
  else 'other' end;

alter table homestead.soil_tests
  add constraint soil_tests_ph_range check (ph is null or ph between 0 and 14),
  add constraint soil_tests_measurements_object check (jsonb_typeof(measurements) = 'object');

alter table homestead.crop_observations
  add column resolved_on date,
  add column follow_up_task_id uuid references homestead.tasks (id),
  add constraint crop_observations_resolved_after check (resolved_on is null or resolved_on >= observed_on),
  add constraint crop_observations_outcome_when_resolved check (resolved_on is null or length(btrim(outcome)) > 0);
create index crop_observations_open on homestead.crop_observations (observed_on desc) where resolved_on is null;
create index crop_observations_category on homestead.crop_observations (category, observed_on desc);

-- Where animals live and how many a place holds, so an overcrowded coop
-- shows up as a number instead of a feeling (the flock outgrew the coop in
-- 2026). Capacity is head of animals; it's for coops, pens and animal areas.

alter table homestead.locations add column capacity integer check (capacity is null or capacity > 0);
alter table homestead.animals add column home_location_id uuid references homestead.locations (id);
create index animals_home on homestead.animals (home_location_id) where home_location_id is not null;

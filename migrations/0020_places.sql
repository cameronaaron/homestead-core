-- A place can sit inside another (a bed in a garden on the site) without
-- every place carrying every field. Coordinates stay out until something maps them.

alter table homestead.locations drop constraint locations_kind_check;
alter table homestead.locations add constraint locations_kind_check check (kind in (
  'site', 'garden', 'field', 'bed', 'row', 'orchard', 'greenhouse',
  'animal_area', 'pen', 'coop', 'compost', 'stand', 'storage', 'other'
));

alter table homestead.locations
  add column parent_id uuid references homestead.locations (id),
  add column active boolean not null default true,
  add column area_sqft numeric(12, 2) check (area_sqft is null or area_sqft > 0),
  add column soil_notes text not null default '';

alter table homestead.locations add constraint locations_not_own_parent check (parent_id is distinct from id);
create index locations_parent on homestead.locations (parent_id) where parent_id is not null;

-- A parent chain that loops is unrepresentable. Depth is tiny (site → garden → bed → row).
create function homestead.location_parent_is_acyclic() returns trigger language plpgsql as $$
declare seen uuid := new.id; hop uuid := new.parent_id; n int := 0;
begin
  while hop is not null loop
    if hop = seen then
      raise exception 'location parent cycle';
    end if;
    n := n + 1;
    if n > 8 then
      raise exception 'location nesting is too deep';
    end if;
    select parent_id into hop from homestead.locations where id = hop;
  end loop;
  return new;
end $$;

create trigger locations_parent_acyclic before insert or update of parent_id on homestead.locations
  for each row execute function homestead.location_parent_is_acyclic();

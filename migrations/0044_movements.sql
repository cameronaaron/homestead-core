-- Where animals and equipment have been: the goat from Pasture A to
-- Pasture B to the barn, the tiller from the shed to the garden to the
-- repair shop. `animals.home_location_id` and `equipment.location_id` stay
-- the answer to "where is it now"; every change to them (from any form,
-- API or import) writes a movement here in the same statement, so the
-- history can't miss one. A move entered after the fact passes its time
-- and note through transaction-local settings, as the audit actor does.

create table homestead.movements (
  id uuid primary key default gen_random_uuid(),
  moved_at timestamptz not null,
  animal_id uuid references homestead.animals (id) on delete cascade,
  equipment_id uuid references homestead.equipment (id) on delete cascade,
  from_location_id uuid references homestead.locations (id),
  to_location_id uuid references homestead.locations (id),
  note text not null default '' check (length(note) <= 200),
  recorded_at timestamptz not null default now(),
  check (num_nonnulls(animal_id, equipment_id) = 1),
  check (from_location_id is distinct from to_location_id)
);
create index movements_animal on homestead.movements (animal_id, moved_at desc) where animal_id is not null;
create index movements_equipment on homestead.movements (equipment_id, moved_at desc) where equipment_id is not null;
create index movements_to on homestead.movements (to_location_id, moved_at desc) where to_location_id is not null;
create index movements_from on homestead.movements (from_location_id, moved_at desc) where from_location_id is not null;

create function homestead.record_movement() returns trigger language plpgsql as $$
declare
  old_place uuid;
  new_place uuid;
  at timestamptz := coalesce(nullif(current_setting('homestead.moved_at', true), '')::timestamptz, now());
  note text := coalesce(current_setting('homestead.move_note', true), '');
  latest timestamptz;
begin
  if tg_table_name = 'animals' then
    new_place := new.home_location_id;
    old_place := case when tg_op = 'UPDATE' then old.home_location_id end;
  else
    new_place := new.location_id;
    old_place := case when tg_op = 'UPDATE' then old.location_id end;
  end if;
  if new_place is not distinct from old_place then
    return new;
  end if;
  if at > now() + interval '5 minutes' then
    raise exception 'a move can''t be in the future';
  end if;
  select max(moved_at) into latest from homestead.movements
    where (tg_table_name = 'animals' and animal_id = new.id) or (tg_table_name = 'equipment' and equipment_id = new.id);
  -- Otherwise "where it is now" would disagree with the latest move.
  if latest is not null and at < latest then
    raise exception 'that move is before the last one recorded; record moves in order';
  end if;
  insert into homestead.movements (moved_at, animal_id, equipment_id, from_location_id, to_location_id, note)
  values (at,
          case when tg_table_name = 'animals' then new.id end,
          case when tg_table_name = 'equipment' then new.id end,
          old_place, new_place, note);
  return new;
end $$;

create trigger animals_movement after insert or update of home_location_id on homestead.animals
  for each row execute function homestead.record_movement();
create trigger equipment_movement after insert or update of location_id on homestead.equipment
  for each row execute function homestead.record_movement();

create trigger audit_movements after insert or update or delete on homestead.movements
  for each row execute function homestead.audit_row_change();

-- History starts now: where each thing is today, without pretending to know when it got there.
insert into homestead.movements (moved_at, animal_id, to_location_id, note)
  select now(), id, home_location_id, 'where it was when movement history began'
  from homestead.animals where home_location_id is not null;
insert into homestead.movements (moved_at, equipment_id, to_location_id, note)
  select now(), id, location_id, 'where it was when movement history began'
  from homestead.equipment where location_id is not null;

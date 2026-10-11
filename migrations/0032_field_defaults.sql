-- The last place, planting, amount, and flock someone logged from Today,
-- so the next entry can start there. Personal, and wiped with the account.

create table homestead.field_defaults (
  actor_id uuid primary key references homestead.actors (id) on delete cascade,
  location_id uuid references homestead.locations (id) on delete set null,
  planting_id uuid references homestead.plantings (id) on delete set null,
  harvest_quantity numeric(12, 3),
  harvest_unit text,
  animal_id uuid references homestead.animals (id) on delete set null,
  check (harvest_quantity is null or harvest_quantity > 0),
  check (harvest_unit is null or harvest_unit in ('lb', 'oz', 'kg', 'g', 'each', 'bunch', 'dozen'))
);

create trigger audit_field_defaults
  after insert or update or delete on homestead.field_defaults
  for each row execute function homestead.audit_row_change();

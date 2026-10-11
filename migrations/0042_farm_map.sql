-- Where each place is on the farm's plan: a point, a line or an outline,
-- in feet east (x) and north (y) of a corner the farm picks. Phone GPS
-- can't tell one bed from the next; a tape measure from a fixed corner can.
-- `points` is a JSON array of [x, y] pairs, checked by the domain before it
-- gets here; the database keeps only the bounds it can check cheaply.

create table homestead.location_shapes (
  location_id uuid primary key references homestead.locations (id) on delete cascade,
  kind text not null check (kind in ('point', 'line', 'polygon')),
  points jsonb not null check (
    jsonb_typeof(points) = 'array' and jsonb_array_length(points) between 1 and 200
  ),
  updated_at timestamptz not null default now()
);

create trigger audit_location_shapes after insert or update or delete on homestead.location_shapes
  for each row execute function homestead.audit_row_change();

-- Care that repeats: a foster cat's antibiotics twice a day for ten days
-- would be every_days = 1 with the doses in the details; a booster in
-- three weeks; the flock's dewormer every six months. Logging care of the
-- same kind moves the next due date on, and a schedule past its end stops.

create table homestead.care_schedules (
  id uuid primary key default gen_random_uuid(),
  animal_id uuid not null references homestead.animals (id) on delete cascade,
  kind text not null check (kind in (
    'vaccine', 'spay_neuter', 'deworm', 'flea', 'vet_visit', 'medication', 'microchip', 'weight',
    'feeding', 'health_note', 'mortality', 'flock_count', 'hoof', 'enclosure', 'other'
  )),
  details text not null default '' check (length(details) <= 200),
  every_days integer not null check (every_days between 1 and 366),
  next_due_on date not null,
  ends_on date,
  stopped_at timestamptz,
  created_at timestamptz not null default now(),
  check (ends_on is null or ends_on >= next_due_on)
);
create index care_schedules_due on homestead.care_schedules (next_due_on) where stopped_at is null;

create trigger audit_care_schedules after insert or update or delete on homestead.care_schedules
  for each row execute function homestead.audit_row_change();

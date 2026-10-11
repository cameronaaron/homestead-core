-- Community cat colonies for Kiki's Cat Club's TNR (trap, neuter, return).
-- A colony is where a group of outdoor cats lives and who feeds them; the
-- goal is every cat in it fixed. `area` is deliberately coarse (a block,
-- a landmark): colonies are often on someone's property, and the exact
-- spot and the caretaker's contact details are theirs to share.

create table homestead.colonies (
  id uuid primary key default gen_random_uuid(),
  name text not null unique check (length(btrim(name)) between 1 and 80),
  area text not null default '' check (length(area) <= 200),
  caretaker_name text not null default '' check (length(caretaker_name) <= 80),
  -- Phone or email; shown only to people who can change animals.
  caretaker_contact text not null default '' check (length(caretaker_contact) <= 120),
  -- The caretaker's best count, fixed or not.
  estimated_size integer check (estimated_size is null or estimated_size >= 0),
  notes text not null default '' check (length(notes) <= 2000),
  created_at timestamptz not null default now()
);

alter table homestead.animals
  add column colony_id uuid references homestead.colonies (id),
  -- The universal sign a community cat is already fixed.
  add column ear_tipped boolean not null default false,
  add check (colony_id is null or species = 'cat');

create trigger audit_colonies after insert or update or delete on homestead.colonies
  for each row execute function homestead.audit_row_change();

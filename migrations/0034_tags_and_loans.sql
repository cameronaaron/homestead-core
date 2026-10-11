-- QR tags: a printed code on a tool, a bin, a coop, a bed or the gate.
-- Scanning it opens /t/{code}, where anyone holding the tag (no account
-- needed) can do the few things that make sense for that thing: check a
-- tool out, say they used some feed, log eggs. The code is the credential,
-- like a setup link, so it's long enough not to guess, and retiring a tag
-- (a lost or photographed-and-abused one) revokes it.

create table homestead.tags (
  -- 10 characters from an alphabet without 0/O, 1/I/L: about 49 bits.
  code text primary key check (code ~ '^[2-9A-HJKMNP-Z]{10}$'),
  kind text not null check (kind in ('equipment', 'inventory_item', 'location', 'animal', 'gate')),
  equipment_id uuid references homestead.equipment (id),
  inventory_item_id uuid references homestead.inventory_items (id),
  location_id uuid references homestead.locations (id),
  animal_id uuid references homestead.animals (id),
  -- Printed under the code; defaults to the thing's name.
  label text not null default '',
  created_at timestamptz not null default now(),
  retired_at timestamptz,
  -- Exactly the target the kind names; the gate has none.
  check (num_nonnulls(equipment_id, inventory_item_id, location_id, animal_id) = case when kind = 'gate' then 0 else 1 end),
  check (kind <> 'equipment' or equipment_id is not null),
  check (kind <> 'inventory_item' or inventory_item_id is not null),
  check (kind <> 'location' or location_id is not null),
  check (kind <> 'animal' or animal_id is not null)
);
-- One live tag per thing, so reprinting shows the same code.
create unique index tags_one_live_per_target on homestead.tags
  (kind, coalesce(equipment_id, inventory_item_id, location_id, animal_id, '00000000-0000-0000-0000-000000000000'))
  where retired_at is null;

-- Who has a tool. Open while returned_at is null; at most one open loan per
-- tool. The borrower is whoever took it: a signed-in person (actor_id) or a
-- name typed at the tag.
create table homestead.equipment_loans (
  id uuid primary key default gen_random_uuid(),
  equipment_id uuid not null references homestead.equipment (id),
  borrower text not null check (length(btrim(borrower)) between 1 and 80),
  actor_id uuid references homestead.actors (id),
  out_at timestamptz not null default now(),
  due_on date,
  returned_at timestamptz,
  return_note text not null default '' check (length(return_note) <= 500),
  check (returned_at is null or returned_at >= out_at)
);
create unique index equipment_loans_one_open on homestead.equipment_loans (equipment_id) where returned_at is null;
create index equipment_loans_history on homestead.equipment_loans (equipment_id, out_at desc);

do $$
declare t text;
begin
  foreach t in array array['tags', 'equipment_loans']
  loop
    execute format('create trigger audit_%I after insert or update or delete on homestead.%I for each row execute function homestead.audit_row_change()', t, t);
  end loop;
end $$;

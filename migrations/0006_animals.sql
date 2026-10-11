-- Animals: the flock, any goats, and Kiki's Cat Club's cats.
--
-- A record is one animal, or a group counted together (a flock of hens).
-- Cats and goats are always one per record, because care, placement and
-- adoption happen to an individual.

create table homestead.animals (
  id uuid primary key default gen_random_uuid(),
  species text not null check (species in ('chicken', 'duck', 'goat', 'cat', 'other')),
  name text not null check (length(btrim(name)) > 0),
  head_count integer not null default 1 check (head_count >= 1),
  sex text not null default 'unknown' check (sex in ('female', 'male', 'unknown')),
  status text not null default 'resident'
    check (status in ('resident', 'in_care', 'fostered', 'adoptable', 'adopted', 'returned_to_colony', 'transferred', 'deceased')),
  origin text not null default 'other'
    check (origin in ('born_here', 'purchased', 'rescued', 'surrendered', 'stray', 'trapped', 'transferred_in', 'other')),
  arrived_on date not null,
  born_on date,
  microchip text not null default '',
  notes text not null default '',
  created_at timestamptz not null default now(),
  check (head_count = 1 or species in ('chicken', 'duck', 'other'))
);
create index animals_species_status on homestead.animals (species, status);

-- Vaccines, spay/neuter, deworming, vet visits…: what grant reports and
-- adopters ask for.
create table homestead.animal_care (
  id uuid primary key default gen_random_uuid(),
  animal_id uuid not null references homestead.animals (id) on delete cascade,
  occurred_on date not null,
  kind text not null
    check (kind in ('vaccine', 'spay_neuter', 'deworm', 'flea', 'vet_visit', 'medication', 'microchip', 'weight', 'other')),
  details text not null default '',
  provider text not null default '',
  cost numeric(10,2) check (cost is null or cost >= 0),
  created_at timestamptz not null default now()
);
create index animal_care_animal on homestead.animal_care (animal_id, occurred_on desc);

-- Where an animal went: foster, adoption, back to its colony after TNR, or
-- another rescue. The person's name only; contact details live on the
-- signed agreement in Documenso.
create table homestead.animal_placements (
  id uuid primary key default gen_random_uuid(),
  animal_id uuid not null references homestead.animals (id) on delete cascade,
  kind text not null check (kind in ('foster', 'adoption', 'return_to_colony', 'transfer')),
  person_name text not null default '',
  started_on date not null,
  fee numeric(10,2) check (fee is null or fee >= 0),
  notes text not null default '',
  created_at timestamptz not null default now(),
  check (fee is null or kind = 'adoption'),
  check (kind in ('return_to_colony') or length(btrim(person_name)) > 0)
);
create index animal_placements_animal on homestead.animal_placements (animal_id, started_on desc);

do $$
declare t text;
begin
  foreach t in array array['animals','animal_care','animal_placements']
  loop
    execute format('create trigger audit_%I after insert or update or delete on homestead.%I for each row execute function homestead.audit_row_change()', t, t);
  end loop;
end $$;

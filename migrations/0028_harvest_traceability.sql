-- Where a harvest went, traceably: a farm-stand sale can count against the
-- harvest it came from (as its 'sale' disposition, so it is never counted
-- twice), and a donated part of a harvest is the produce donation itself.

alter table homestead.harvest_dispositions
  add column stand_sale_id uuid unique references homestead.stand_sales (id),
  add constraint dispositions_stand_sale_is_a_sale check (stand_sale_id is null or kind = 'sale');

-- One donation record per donated part of a harvest.
create unique index produce_donations_one_per_disposition on homestead.produce_donations (disposition_id)
  where disposition_id is not null;

-- Harvest totals by crop or date for reports and the seed-lot trace.
create index harvests_harvested on homestead.harvests (harvested_on desc);

-- Two people splitting the same harvest at once could each see room for
-- their part and together exceed it. Locking the harvest row serialises them.
create or replace function homestead.disposition_fits_harvest() returns trigger language plpgsql as $$
declare h_qty numeric; h_unit text; used numeric;
begin
  select quantity, unit into h_qty, h_unit from homestead.harvests where id = new.harvest_id for update;
  if h_unit is null then
    raise exception 'harvest not found';
  end if;
  if new.unit is distinct from h_unit then
    raise exception 'disposition unit must match the harvest unit';
  end if;
  select coalesce(sum(quantity), 0) into used from homestead.harvest_dispositions
    where harvest_id = new.harvest_id and id is distinct from new.id;
  if used + new.quantity > h_qty then
    raise exception 'dispositions exceed the harvest';
  end if;
  return new;
end $$;

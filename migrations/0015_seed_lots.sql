-- Seed lots (from 0003) connected to vendors and plantings. What's left of a
-- lot is derived from the plantings that used it, never stored.

alter table homestead.seed_lots
  add column vendor_id uuid references homestead.vendors (id),
  add column quantity numeric(12, 3) check (quantity is null or quantity > 0),
  add column unit text check (unit in ('seeds', 'g', 'oz', 'packet', 'lb')),
  add constraint seed_lots_quantity_has_unit check ((quantity is null) = (unit is null));
create index seed_lots_vendor on homestead.seed_lots (vendor_id) where vendor_id is not null;

alter table homestead.plantings
  add column seed_quantity_used numeric(12, 3) check (seed_quantity_used is null or seed_quantity_used > 0),
  add constraint plantings_seed_use_names_its_lot check (seed_quantity_used is null or seed_lot_id is not null);

-- Seed packets and supplier certificates attach to the lot.
alter table homestead.attachments drop constraint attachments_resource_type_check,
  add constraint attachments_resource_type_check check (resource_type in
    ('animal', 'animal_care', 'purchase', 'gift', 'grant_campaign', 'seed_lot'));

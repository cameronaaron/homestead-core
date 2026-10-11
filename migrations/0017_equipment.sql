-- Equipment and its maintenance history.

create table homestead.equipment (
  id uuid primary key default gen_random_uuid(),
  name text not null check (length(btrim(name)) > 0),
  category text not null check (length(btrim(category)) > 0),
  manufacturer text not null default '',
  model text not null default '',
  serial_number text not null default '',
  purchased_on date,
  purchase_cost numeric(12, 2) check (purchase_cost is null or purchase_cost >= 0),
  purchase_id uuid references homestead.purchases (id),
  location_id uuid references homestead.locations (id),
  condition text not null default 'good' check (condition in ('new', 'good', 'fair', 'poor')),
  status text not null default 'in_service' check (status in
    ('in_service', 'needs_repair', 'out_of_service', 'retired')),
  notes text not null default '',
  created_at timestamptz not null default now()
);
create index equipment_active on homestead.equipment (category, name) where status <> 'retired';
-- The same maker's serial number is one machine.
create unique index equipment_serial on homestead.equipment (lower(manufacturer), serial_number)
  where serial_number <> '';

create table homestead.equipment_maintenance (
  id uuid primary key default gen_random_uuid(),
  equipment_id uuid not null references homestead.equipment (id),
  performed_on date not null,
  work text not null check (length(btrim(work)) > 0),
  cost numeric(12, 2) check (cost is null or cost >= 0),
  performed_by text not null default '',
  vendor_id uuid references homestead.vendors (id),
  next_service_on date,
  notes text not null default '',
  -- The reminder task made for the next service, if one was asked for.
  task_id uuid references homestead.tasks (id),
  created_at timestamptz not null default now(),
  check (next_service_on is null or next_service_on > performed_on)
);
-- "Latest record" (for next service) and each machine's history.
create index equipment_maintenance_history on homestead.equipment_maintenance (equipment_id, performed_on desc, created_at desc);

do $$
declare t text;
begin
  foreach t in array array['equipment', 'equipment_maintenance']
  loop
    execute format('create trigger audit_%I after insert or update or delete on homestead.%I for each row execute function homestead.audit_row_change()', t, t);
  end loop;
end $$;

-- Manuals and photos attach to the equipment; receipts to the service.
alter table homestead.attachments drop constraint attachments_resource_type_check,
  add constraint attachments_resource_type_check check (resource_type in
    ('animal', 'animal_care', 'purchase', 'gift', 'grant_campaign', 'seed_lot', 'equipment', 'equipment_maintenance'));

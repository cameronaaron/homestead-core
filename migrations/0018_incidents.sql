-- Incident and safety reports. Kept apart from ordinary farm records and
-- readable only with incidents.read (or by the person who filed one).

create table homestead.incidents (
  id uuid primary key default gen_random_uuid(),
  occurred_at timestamptz not null,
  location_id uuid references homestead.locations (id),
  location_detail text not null default '',
  kind text not null check (kind in
    ('injury', 'near_miss', 'animal_bite', 'property_damage', 'food_safety', 'hazard', 'other')),
  description text not null check (length(btrim(description)) between 1 and 4000),
  immediate_action text not null default '',
  people_involved text not null default '',
  witnesses text not null default '',
  -- How far treatment went; never what the injury was.
  treatment text check (treatment in ('none', 'first_aid', 'medical_care')),
  corrective_action text not null default '',
  follow_up_by date,
  status text not null default 'open' check (status in ('open', 'closed')),
  closure_notes text not null default '',
  reported_by uuid references homestead.actors (id),
  reported_by_name text not null,
  reported_at timestamptz not null default now(),
  closed_at timestamptz,
  closed_by uuid references homestead.actors (id),
  check (treatment is null or kind in ('injury', 'animal_bite')),
  check ((status = 'closed') = (closed_at is not null)),
  -- Nothing is closed without saying what was done about it.
  check (status = 'open' or length(btrim(corrective_action)) > 0)
);
create index incidents_open on homestead.incidents (occurred_at desc) where status = 'open';
create index incidents_occurred on homestead.incidents (occurred_at desc);
create index incidents_reporter on homestead.incidents (reported_by, reported_at desc);

create trigger audit_incidents after insert or update or delete on homestead.incidents
  for each row execute function homestead.audit_row_change();

-- Photos and documents of what happened attach to the incident.
alter table homestead.attachments drop constraint attachments_resource_type_check,
  add constraint attachments_resource_type_check check (resource_type in
    ('animal', 'animal_care', 'purchase', 'gift', 'grant_campaign', 'seed_lot', 'equipment', 'equipment_maintenance',
     'incident'));

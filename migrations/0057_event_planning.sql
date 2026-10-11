-- The small planning surface around a community event: who can come, the
-- work to finish, and the stock and equipment that need to be ready.

alter table homestead.community_events
  add column status text not null default 'scheduled'
    check (status in ('scheduled', 'cancelled'));

create table homestead.event_rsvps (
  event_id uuid not null references homestead.community_events (id) on delete cascade,
  volunteer_id uuid not null references homestead.volunteers (id) on delete cascade,
  availability text not null check (availability in ('attending', 'maybe', 'unavailable')),
  note text not null default '',
  updated_at timestamptz not null default now(),
  primary key (event_id, volunteer_id)
);

create table homestead.event_tasks (
  event_id uuid not null references homestead.community_events (id) on delete cascade,
  task_id uuid not null references homestead.tasks (id) on delete cascade,
  primary key (event_id, task_id)
);

create table homestead.event_inventory (
  event_id uuid not null references homestead.community_events (id) on delete cascade,
  item_id uuid not null references homestead.inventory_items (id) on delete cascade,
  quantity numeric(12,3) not null check (quantity > 0),
  note text not null default '',
  primary key (event_id, item_id)
);

create table homestead.event_equipment (
  event_id uuid not null references homestead.community_events (id) on delete cascade,
  equipment_id uuid not null references homestead.equipment (id) on delete cascade,
  note text not null default '',
  primary key (event_id, equipment_id)
);

do $$
declare t text;
begin
  foreach t in array array['event_rsvps','event_tasks','event_inventory','event_equipment']
  loop
    execute format('create trigger audit_%I after insert or update or delete on homestead.%I for each row execute function homestead.audit_row_change()', t, t);
  end loop;
end $$;

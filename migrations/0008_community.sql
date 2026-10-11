-- Community events (work days, potlucks, workshops, adoption events, talks)
-- and the volunteer hours that grant applications ask for.

create table homestead.community_events (
  id uuid primary key default gen_random_uuid(),
  title text not null check (length(btrim(title)) > 0),
  kind text not null
    check (kind in ('workshop', 'work_day', 'potluck', 'adoption_event', 'market', 'talk', 'meeting', 'other')),
  held_on date not null,
  time_note text not null default '',
  place text not null default '',
  description text not null default '',
  attendance integer check (attendance is null or attendance >= 0),
  created_at timestamptz not null default now()
);
create index community_events_held on homestead.community_events (held_on);

create table homestead.volunteer_shifts (
  id uuid primary key default gen_random_uuid(),
  person_name text not null check (length(btrim(person_name)) > 0),
  worked_on date not null,
  hours numeric(5,2) not null check (hours > 0 and hours <= 24),
  working_group text not null
    check (working_group in ('grants_business', 'farm_labor', 'social_media', 'cat_club', 'other')),
  event_id uuid references homestead.community_events (id) on delete set null,
  notes text not null default '',
  created_at timestamptz not null default now()
);
create index volunteer_shifts_worked on homestead.volunteer_shifts (worked_on desc);

do $$
declare t text;
begin
  foreach t in array array['community_events','volunteer_shifts']
  loop
    execute format('create trigger audit_%I after insert or update or delete on homestead.%I for each row execute function homestead.audit_row_change()', t, t);
  end loop;
end $$;

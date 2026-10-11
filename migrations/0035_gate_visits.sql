-- Signing in and out at the gate (a QR tag on the gate post). A visit is
-- open while out_at is null; checking out closes it and, when the time is
-- plausible, records a volunteer shift for it. Open visits are also who's
-- on site right now, which is what an evacuation headcount needs.

create table homestead.volunteer_visits (
  id uuid primary key default gen_random_uuid(),
  person_name text not null check (length(btrim(person_name)) between 1 and 80),
  actor_id uuid references homestead.actors (id),
  working_group text not null default 'farm_labor'
    check (working_group in ('grants_business', 'farm_labor', 'social_media', 'cat_club', 'other')),
  in_at timestamptz not null default now(),
  out_at timestamptz,
  -- The shift the visit became, when it became one.
  shift_id uuid references homestead.volunteer_shifts (id) on delete set null,
  check (out_at is null or out_at >= in_at),
  check (shift_id is null or out_at is not null)
);
-- One open visit per person, however they capitalise their name.
create unique index volunteer_visits_one_open on homestead.volunteer_visits (lower(btrim(person_name)))
  where out_at is null;
create index volunteer_visits_in on homestead.volunteer_visits (in_at desc);

create trigger audit_volunteer_visits after insert or update or delete on homestead.volunteer_visits
  for each row execute function homestead.audit_row_change();

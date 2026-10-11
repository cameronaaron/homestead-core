-- Volunteers as people, not spellings. Shifts and gate visits stored the
-- name as typed, so "Sam" and "Sam R." counted as two people. Now each
-- names a `volunteers` row. This is deliberately small: a name, optional
-- contact details, and whether they're still around.
--
-- A typed name finds its volunteer through `volunteer_names`, which holds
-- every name that means that person (their own included). Merging "Sam"
-- into "Sam R." moves the shifts, visits and the name "Sam" over, so the
-- next sign-in as "Sam" lands on Sam R. A name means one person at a
-- time; two different Sams need two different names.

create table homestead.volunteers (
  id uuid primary key default gen_random_uuid(),
  name text not null check (length(btrim(name)) between 1 and 80),
  email text check (email = lower(btrim(email)) and email ~ '^[^[:space:]@]+@[^[:space:]@]+\.[^[:space:]@]+$'),
  phone text not null default '' check (length(phone) <= 40),
  active boolean not null default true,
  created_at timestamptz not null default now()
);

create table homestead.volunteer_names (
  name text primary key check (name = lower(btrim(name)) and length(name) between 1 and 80),
  volunteer_id uuid not null references homestead.volunteers (id) on delete cascade
);
create index volunteer_names_volunteer on homestead.volunteer_names (volunteer_id);

-- One volunteer per distinct name already recorded (case and spaces
-- ignored, as the old counts did), keeping the earliest spelling.
insert into homestead.volunteers (name, created_at)
select distinct on (key) btrim(person_name), at
from (
  select lower(btrim(person_name)) as key, person_name, created_at as at from homestead.volunteer_shifts
  union all
  select lower(btrim(person_name)), person_name, in_at from homestead.volunteer_visits
) seen
order by key, at;
insert into homestead.volunteer_names (name, volunteer_id) select lower(btrim(name)), id from homestead.volunteers;

-- `person_name` stays as the name written at the time; `volunteer_id` is who.
alter table homestead.volunteer_shifts add column volunteer_id uuid references homestead.volunteers (id);
alter table homestead.volunteer_visits add column volunteer_id uuid references homestead.volunteers (id);
update homestead.volunteer_shifts s set volunteer_id = n.volunteer_id
  from homestead.volunteer_names n where n.name = lower(btrim(s.person_name));
update homestead.volunteer_visits v set volunteer_id = n.volunteer_id
  from homestead.volunteer_names n where n.name = lower(btrim(v.person_name));
alter table homestead.volunteer_shifts alter column volunteer_id set not null;
alter table homestead.volunteer_visits alter column volunteer_id set not null;
create index volunteer_shifts_volunteer on homestead.volunteer_shifts (volunteer_id);

-- One open visit per person, however they spell themselves.
drop index homestead.volunteer_visits_one_open;
create unique index volunteer_visits_one_open on homestead.volunteer_visits (volunteer_id) where out_at is null;

do $$
declare t text;
begin
  foreach t in array array['volunteers', 'volunteer_names']
  loop
    execute format('create trigger audit_%I after insert or update or delete on homestead.%I for each row execute function homestead.audit_row_change()', t, t);
  end loop;
end $$;

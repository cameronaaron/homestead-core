-- Adoption applications from the public /adopt page (linked from Instagram).
-- The page shows adoptable animals by name, species, age and the blurb
-- written for them here; never their notes or vet records. An application
-- is people's data (email, phone), so it's read by those who can change
-- animals and audited the same way.

alter table homestead.animals
  add column adoption_blurb text not null default '' check (length(adoption_blurb) <= 1000);

create table homestead.adoption_applications (
  id uuid primary key default gen_random_uuid(),
  -- Null: "any cat that fits", or the animal record was removed.
  animal_id uuid references homestead.animals (id) on delete set null,
  applicant_name text not null check (length(btrim(applicant_name)) between 1 and 80),
  email text not null check (email ~ '^[^@\s]+@[^@\s]+\.[^@\s]+$' and length(email) <= 254),
  phone text not null default '' check (length(phone) <= 40),
  about text not null default '' check (length(about) <= 4000),
  status text not null default 'new' check (status in ('new', 'approved', 'declined', 'withdrawn')),
  received_at timestamptz not null default now(),
  decided_at timestamptz,
  check ((status = 'new') = (decided_at is null))
);
-- One open application per person per animal: a double-submit is one.
create unique index adoption_applications_one_open on homestead.adoption_applications
  (lower(email), coalesce(animal_id, '00000000-0000-0000-0000-000000000000')) where status = 'new';
create index adoption_applications_received on homestead.adoption_applications (received_at desc);

create trigger audit_adoption_applications after insert or update or delete on homestead.adoption_applications
  for each row execute function homestead.audit_row_change();

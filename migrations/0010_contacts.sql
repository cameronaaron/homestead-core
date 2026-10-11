-- The mailing list: people who asked to hear from us (the website's
-- sign-up form, the farm stand clipboard, workshops). An unsubscribe is
-- permanent until the person signs up again themselves.

create table homestead.contacts (
  id uuid primary key default gen_random_uuid(),
  name text not null default '',
  email text not null unique
    check (email = lower(btrim(email)) and email ~ '^[^[:space:]@]+@[^[:space:]@]+\.[^[:space:]@]+$'),
  interests text[] not null default '{}'
    check (interests <@ array['newsletter', 'volunteering', 'workshops', 'cat_adoption', 'donating']::text[]),
  source text not null check (length(btrim(source)) > 0),
  consented_at timestamptz not null default now(),
  unsubscribed_at timestamptz,
  created_at timestamptz not null default now()
);
create index contacts_subscribed on homestead.contacts (email) where unsubscribed_at is null;

create trigger audit_contacts after insert or update or delete on homestead.contacts
  for each row execute function homestead.audit_row_change();

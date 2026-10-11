-- Reviewable, personalized grant outreach. Templates and recipient records
-- stay editable; generated campaign messages are immutable snapshots.

create table homestead.grant_templates (
  id uuid primary key default gen_random_uuid(),
  name text not null unique check (length(btrim(name)) > 0),
  subject_template text not null check (length(btrim(subject_template)) > 0),
  body_template text not null check (length(btrim(body_template)) > 0),
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now()
);

create table homestead.grant_recipients (
  id uuid primary key default gen_random_uuid(),
  organization text not null check (length(btrim(organization)) > 0),
  contact_name text not null default '',
  email text not null unique check (email = lower(btrim(email)) and email ~ '^[^[:space:]@]+@[^[:space:]@]+\.[^[:space:]@]+$'),
  program_name text not null default '',
  source text not null check (length(btrim(source)) > 0),
  custom_fields jsonb not null default '{}'::jsonb check (jsonb_typeof(custom_fields) = 'object'),
  suppressed_at timestamptz,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now()
);
create index grant_recipients_active on homestead.grant_recipients (organization) where suppressed_at is null;

create table homestead.grant_campaigns (
  id uuid primary key default gen_random_uuid(),
  name text not null check (length(btrim(name)) > 0),
  template_id uuid not null references homestead.grant_templates (id),
  status text not null default 'draft' check (status in ('draft', 'approved', 'completed')),
  created_at timestamptz not null default now(),
  approved_at timestamptz,
  completed_at timestamptz,
  check ((status = 'draft') = (approved_at is null)),
  check ((status = 'completed') = (completed_at is not null))
);

create table homestead.grant_messages (
  id uuid primary key,
  campaign_id uuid not null references homestead.grant_campaigns (id) on delete cascade,
  recipient_id uuid not null references homestead.grant_recipients (id),
  recipient_email text not null,
  recipient_name text not null,
  organization text not null,
  subject text not null check (length(btrim(subject)) > 0),
  body_text text not null check (length(btrim(body_text)) > 0),
  body_html text not null check (length(btrim(body_html)) > 0),
  status text not null default 'ready' check (status in ('ready', 'sending', 'sent', 'failed', 'suppressed')),
  provider_id text,
  last_error text,
  claimed_at timestamptz,
  sent_at timestamptz,
  unique (campaign_id, recipient_id),
  check ((status = 'sent') = (sent_at is not null))
);
create index grant_messages_due on homestead.grant_messages (campaign_id, status);

do $$
declare t text;
begin
  foreach t in array array['grant_templates','grant_recipients','grant_campaigns','grant_messages']
  loop
    execute format('create trigger audit_%I after insert or update or delete on homestead.%I for each row execute function homestead.audit_row_change()', t, t);
  end loop;
end $$;

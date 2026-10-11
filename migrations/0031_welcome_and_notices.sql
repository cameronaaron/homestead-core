-- First sign-in, and which farm events a person wants on Today or as a
-- Discord ping. Existing accounts already use the app, so they are marked
-- welcomed; a person added after this starts with welcomed_at null.

alter table homestead.actors add column welcomed_at timestamptz;
update homestead.actors set welcomed_at = now();

create table homestead.notification_preferences (
  actor_id uuid not null references homestead.actors (id) on delete cascade,
  channel text not null check (channel in ('today', 'discord')),
  kind text not null check (length(btrim(kind)) > 0),
  enabled boolean not null,
  primary key (actor_id, channel, kind)
);

create trigger audit_notification_preferences
  after insert or update or delete on homestead.notification_preferences
  for each row execute function homestead.audit_row_change();

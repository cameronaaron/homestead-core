-- Sensors publishing over MQTT (the farm's Mosquitto) and rules that turn
-- a value past a line into a task. The core never subscribes to MQTT: it
-- sleeps when idle and can't reach the farm's network. An on-site bridge
-- (`homestead mqtt-bridge`) batches messages and posts them to the API.

create table homestead.sensors (
  id uuid primary key default gen_random_uuid(),
  topic text not null check (length(btrim(topic)) between 1 and 200 and topic !~ '[+#]'),
  -- JSON key (dotted path) of its value in the payload; '' for a bare number.
  field text not null default '' check (length(field) <= 60),
  name text not null check (length(btrim(name)) between 1 and 60),
  kind text not null check (length(btrim(kind)) between 1 and 60 and kind = lower(kind)),
  value_name text not null default 'value' check (length(btrim(value_name)) between 1 and 60),
  unit text not null default '' check (length(unit) <= 20),
  location_id uuid references homestead.locations (id),
  animal_id uuid references homestead.animals (id),
  equipment_id uuid references homestead.equipment (id),
  sample_secs integer not null default 900 check (sample_secs between 60 and 86400),
  active boolean not null default true,
  last_value numeric(14, 4),
  last_seen_at timestamptz,
  created_at timestamptz not null default now(),
  check (num_nonnulls(location_id, animal_id, equipment_id) = 1),
  unique (topic, field)
);

-- Samples are kept apart from readings, and unaudited, because of volume:
-- one sensor every 15 minutes is 35,000 rows a year, and as readings each
-- would add two audit rows. Here a sample is ~60 bytes. Pages show sensor
-- trends beside readings. The key makes a re-sent batch a no-op.
create table homestead.sensor_samples (
  sensor_id uuid not null references homestead.sensors (id) on delete cascade,
  observed_at timestamptz not null,
  value numeric(14, 4) not null,
  primary key (sensor_id, observed_at)
);

create table homestead.sensor_rules (
  id uuid primary key default gen_random_uuid(),
  sensor_id uuid not null references homestead.sensors (id) on delete cascade,
  op text not null check (op in ('below', 'above')),
  threshold numeric(14, 4) not null,
  clear_at numeric(14, 4),
  task_title text not null check (length(btrim(task_title)) between 1 and 120),
  priority text not null default 'high' check (priority in ('low', 'normal', 'high', 'urgent')),
  armed boolean not null default true,
  last_fired_at timestamptz,
  last_task_id uuid references homestead.tasks (id) on delete set null,
  created_at timestamptz not null default now(),
  check (clear_at is null or (op = 'below' and clear_at >= threshold) or (op = 'above' and clear_at <= threshold))
);
create index sensor_rules_sensor on homestead.sensor_rules (sensor_id);

-- Topics the bridge heard that no sensor claims yet, so setting one up is a click.
create table homestead.sensor_topics_unclaimed (
  topic text primary key check (length(topic) between 1 and 200),
  last_payload text not null check (length(last_payload) <= 200),
  last_seen_at timestamptz not null,
  messages bigint not null default 1
);

create trigger audit_sensors after insert or update or delete on homestead.sensors
  for each row execute function homestead.audit_row_change();
create trigger audit_sensor_rules after insert or update or delete on homestead.sensor_rules
  for each row execute function homestead.audit_row_change();

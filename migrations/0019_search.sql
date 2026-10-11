-- Application-wide search with PostgreSQL full-text search (no extension,
-- no external index). Each searchable table gets a generated tsvector: the
-- name/title weighs most (A), identifying details next (B), notes least
-- (C). The 'simple' configuration doesn't stem or drop stop words, which
-- suits names ("Mochi", "Sungold", "INV-2291") better than English rules.
--
-- The only query is `search @@ to_tsquery('simple', <prefix query>)`, which
-- a GIN index on the column serves; `every_search_can_use_its_index` checks
-- each one is usable. Considered and rejected: pg_trgm (on Supabase
-- extensions live in the `extensions` schema, so opclass names differ
-- between hosts, and word-prefix matching is what people type anyway).

alter table homestead.animals add column search tsvector generated always as (
    setweight(to_tsvector('simple', name), 'A') ||
    setweight(to_tsvector('simple', species || ' ' || microchip), 'B') ||
    setweight(to_tsvector('simple', notes), 'C')) stored;
create index animals_search on homestead.animals using gin (search);

alter table homestead.tasks add column search tsvector generated always as (
    setweight(to_tsvector('simple', title), 'A') ||
    setweight(to_tsvector('simple', details), 'C')) stored;
create index tasks_search on homestead.tasks using gin (search);

alter table homestead.inventory_items add column search tsvector generated always as (
    setweight(to_tsvector('simple', name), 'A') ||
    setweight(to_tsvector('simple', unit), 'C')) stored;
create index inventory_items_search on homestead.inventory_items using gin (search);

alter table homestead.plantings add column search tsvector generated always as (
    setweight(to_tsvector('simple', crop), 'A') ||
    setweight(to_tsvector('simple', variety), 'B') ||
    setweight(to_tsvector('simple', notes), 'C')) stored;
create index plantings_search on homestead.plantings using gin (search);

alter table homestead.seed_lots add column search tsvector generated always as (
    setweight(to_tsvector('simple', crop), 'A') ||
    setweight(to_tsvector('simple', variety || ' ' || supplier || ' ' || supplier_lot), 'B') ||
    setweight(to_tsvector('simple', notes), 'C')) stored;
create index seed_lots_search on homestead.seed_lots using gin (search);

alter table homestead.locations add column search tsvector generated always as (
    setweight(to_tsvector('simple', name), 'A') ||
    setweight(to_tsvector('simple', kind), 'B') ||
    setweight(to_tsvector('simple', notes), 'C')) stored;
create index locations_search on homestead.locations using gin (search);

alter table homestead.contacts add column search tsvector generated always as (
    setweight(to_tsvector('simple', name), 'A') ||
    setweight(to_tsvector('simple', email), 'B')) stored;
create index contacts_search on homestead.contacts using gin (search);

alter table homestead.gifts add column search tsvector generated always as (
    setweight(to_tsvector('simple', donor_name), 'A') ||
    setweight(to_tsvector('simple', donor_email || ' ' || description || ' ' || fund || ' ' || goods_or_services), 'B') ||
    setweight(to_tsvector('simple', notes), 'C')) stored;
create index gifts_search on homestead.gifts using gin (search);

alter table homestead.community_events add column search tsvector generated always as (
    setweight(to_tsvector('simple', title), 'A') ||
    setweight(to_tsvector('simple', place || ' ' || description), 'B')) stored;
create index community_events_search on homestead.community_events using gin (search);

alter table homestead.equipment add column search tsvector generated always as (
    setweight(to_tsvector('simple', name), 'A') ||
    setweight(to_tsvector('simple', category || ' ' || manufacturer || ' ' || model || ' ' || serial_number), 'B') ||
    setweight(to_tsvector('simple', notes), 'C')) stored;
create index equipment_search on homestead.equipment using gin (search);

alter table homestead.vendors add column search tsvector generated always as (
    setweight(to_tsvector('simple', name), 'A') ||
    setweight(to_tsvector('simple', notes), 'C')) stored;
create index vendors_search on homestead.vendors using gin (search);

alter table homestead.purchases add column search tsvector generated always as (
    setweight(to_tsvector('simple', description), 'A') ||
    setweight(to_tsvector('simple', category || ' ' || coalesce(external_reference, '')), 'B')) stored;
create index purchases_search on homestead.purchases using gin (search);

-- The derived column is noise in audit history: leave it out of snapshots.
create or replace function homestead.audit_row_change() returns trigger language plpgsql as $$
declare old_row jsonb; new_row jsonb; row_id text;
begin
  old_row := case when tg_op = 'INSERT' then null else to_jsonb(old) - 'search' end;
  new_row := case when tg_op = 'DELETE' then null else to_jsonb(new) - 'search' end;
  row_id := coalesce(new_row ->> 'id', old_row ->> 'id');
  insert into homestead.audit_entries
    (actor_label, actor_id, request_id, action, resource_type, resource_id, before, after)
  values
    (coalesce(nullif(current_setting('homestead.actor', true), ''), 'system'),
     nullif(current_setting('homestead.actor_id', true), '')::uuid,
     nullif(current_setting('homestead.request_id', true), ''), lower(tg_op),
     tg_table_name, row_id, old_row, new_row);
  return coalesce(new, old);
end $$;

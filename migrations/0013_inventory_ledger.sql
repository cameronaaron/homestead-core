-- Inventory as a ledger. `stock_movements` is the only authority on what
-- we have; the per-location balances and each item's total are kept from it
-- by a trigger, in the same transaction, and can't be edited any other way.

alter table homestead.stock_movements
  add column kind text not null default 'adjustment' check (kind in
    ('receipt', 'consumption', 'harvest', 'waste', 'adjustment', 'transfer_out', 'transfer_in')),
  add column location_id uuid references homestead.locations (id),
  add column transfer_id uuid,
  add column purchase_line_id uuid;
alter table homestead.stock_movements alter column kind drop default;
alter table homestead.stock_movements
  -- Stock coming in is positive, going out negative; a count correction either.
  add constraint stock_movements_sign_matches_kind check (case
    when kind in ('receipt', 'harvest', 'transfer_in') then delta > 0
    when kind in ('consumption', 'waste', 'transfer_out') then delta < 0
    else true end),
  add constraint stock_movements_transfers_are_paired check ((kind like 'transfer_%') = (transfer_id is not null)),
  add constraint stock_movements_receipts_name_their_line check ((kind = 'receipt') = (purchase_line_id is not null));
-- Receiving a purchase line twice is impossible, not just unlikely.
create unique index stock_movements_one_receipt_per_line on homestead.stock_movements (purchase_line_id)
  where purchase_line_id is not null;
create index stock_movements_transfer on homestead.stock_movements (transfer_id) where transfer_id is not null;

create function homestead.reject_ledger_edit() returns trigger language plpgsql as $$
begin raise exception 'stock movements are append-only; record a correcting movement instead'; end $$;
create trigger stock_movements_append_only before update or delete on homestead.stock_movements
  for each row execute function homestead.reject_ledger_edit();

-- Stock per item and place. A NULL location is stock not put anywhere in
-- particular (everything recorded before locations were tracked).
create table homestead.inventory_balances (
  item_id uuid not null references homestead.inventory_items (id),
  location_id uuid references homestead.locations (id),
  on_hand numeric(12, 3) not null default 0 check (on_hand >= 0),
  unique nulls not distinct (item_id, location_id)
);

-- Carry existing stock over, and prove the old running total was right.
insert into homestead.inventory_balances (item_id, location_id, on_hand)
  select item_id, null, sum(delta) from homestead.stock_movements group by item_id having sum(delta) <> 0;
do $$
begin
  if exists (
    select 1 from homestead.inventory_items i
    where i.on_hand <> coalesce((select sum(delta) from homestead.stock_movements m where m.item_id = i.id), 0)
  ) then
    raise exception 'inventory_items.on_hand disagrees with stock_movements; reconcile before migrating';
  end if;
end $$;

-- The only writer of balances and totals.
-- (Not INSERT ... ON CONFLICT: Postgres checks the proposed row's CHECKs
-- first, so a withdrawal would fail on its own negative delta.)
create function homestead.apply_stock_movement() returns trigger language plpgsql as $$
begin
  update homestead.inventory_balances set on_hand = on_hand + new.delta
    where item_id = new.item_id and location_id is not distinct from new.location_id;
  if not found then
    insert into homestead.inventory_balances (item_id, location_id, on_hand)
      values (new.item_id, new.location_id, new.delta);
  end if;
  update homestead.inventory_items set on_hand = on_hand + new.delta where id = new.item_id;
  return new;
end $$;
create trigger stock_movements_apply after insert on homestead.stock_movements
  for each row execute function homestead.apply_stock_movement();

-- Anything else changing a cached quantity is refused: only the ledger
-- trigger (nesting depth > 1) may.
create function homestead.reject_direct_stock_edit() returns trigger language plpgsql as $$
begin
  if pg_trigger_depth() = 1 then
    raise exception 'on-hand quantities come from stock_movements; record a movement instead';
  end if;
  return new;
end $$;
create trigger inventory_items_on_hand_from_ledger before update of on_hand on homestead.inventory_items
  for each row when (old.on_hand is distinct from new.on_hand) execute function homestead.reject_direct_stock_edit();
create trigger inventory_balances_from_ledger before insert or update or delete on homestead.inventory_balances
  for each row execute function homestead.reject_direct_stock_edit();
-- New items start at zero; stock arrives only as movements.
create function homestead.new_items_start_empty() returns trigger language plpgsql as $$
begin
  if new.on_hand <> 0 then raise exception 'new items start with nothing on hand; record a movement'; end if;
  return new;
end $$;
create trigger inventory_items_start_empty before insert on homestead.inventory_items
  for each row execute function homestead.new_items_start_empty();

-- ── Purchasing ─────────────────────────────────────────────────────────────

create table homestead.purchase_lines (
  id uuid primary key default gen_random_uuid(),
  purchase_id uuid not null references homestead.purchases (id),
  item_id uuid references homestead.inventory_items (id),
  description text not null default '',
  quantity numeric(12, 3) not null check (quantity > 0),
  unit_cost numeric(12, 4) check (unit_cost is null or unit_cost >= 0),
  received_at timestamptz,
  received_location_id uuid references homestead.locations (id),
  created_at timestamptz not null default now(),
  check (item_id is not null or length(btrim(description)) > 0),
  -- Only stock can be received, and a received line says when.
  check (received_at is null or item_id is not null),
  check (received_location_id is null or received_at is not null)
);
create index purchase_lines_purchase on homestead.purchase_lines (purchase_id);
create index purchase_lines_item on homestead.purchase_lines (item_id) where item_id is not null;
alter table homestead.stock_movements add constraint stock_movements_purchase_line_fk
  foreign key (purchase_line_id) references homestead.purchase_lines (id);
create index purchases_vendor on homestead.purchases (vendor_id) where vendor_id is not null;

create trigger audit_purchase_lines after insert or update or delete on homestead.purchase_lines
  for each row execute function homestead.audit_row_change();

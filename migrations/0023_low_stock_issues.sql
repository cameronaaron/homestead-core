-- The Linear reorder issue raised for each low-stock crossing (the
-- `inventory.low_stock` event), recorded once Linear has it. Tasks keep
-- theirs in `tasks.linear_issue_id`.

create table homestead.low_stock_issues (
  event_id bigint primary key references homestead.events (id),
  item_id uuid not null references homestead.inventory_items (id),
  linear_issue_id text not null check (length(linear_issue_id) > 0),
  created_at timestamptz not null default now()
);
create index low_stock_issues_item on homestead.low_stock_issues (item_id, created_at desc);

create trigger audit_low_stock_issues after insert or update or delete on homestead.low_stock_issues
  for each row execute function homestead.audit_row_change();

-- Farm stand sales, and community compost drop-offs.
--
-- Registered egg handlers keep sale records for three years (CDFA); direct
-- sales to consumers don't need the buyer's name, so none is stored.

create table homestead.stand_sales (
  id uuid primary key default gen_random_uuid(),
  sold_on date not null,
  item text not null check (length(btrim(item)) > 0),
  quantity numeric(10,3) not null check (quantity > 0),
  unit text not null check (unit in ('dozen', 'bunch', 'bag', 'each', 'lb', 'packet')),
  amount numeric(10,2) not null check (amount >= 0),
  payment text not null default 'cash' check (payment in ('cash', 'card', 'venmo', 'zelle', 'other')),
  notes text not null default '',
  created_at timestamptz not null default now()
);
create index stand_sales_sold on homestead.stand_sales (sold_on desc);

create table homestead.compost_dropoffs (
  id uuid primary key default gen_random_uuid(),
  received_on date not null,
  contributor text not null default '',
  weight_lb numeric(8,2) check (weight_lb is null or weight_lb > 0),
  notes text not null default '',
  created_at timestamptz not null default now()
);
create index compost_dropoffs_received on homestead.compost_dropoffs (received_on desc);

do $$
declare t text;
begin
  foreach t in array array['stand_sales','compost_dropoffs']
  loop
    execute format('create trigger audit_%I after insert or update or delete on homestead.%I for each row execute function homestead.audit_row_change()', t, t);
  end loop;
end $$;

-- Gifts that don't arrive through Zeffy: sponsors, in-kind donations
-- (plants, lumber, feed), and cash or checks handed over in person. Zeffy
-- receipts its own donations; these are ours to acknowledge.

create table homestead.gifts (
  id uuid primary key default gen_random_uuid(),
  donor_name text not null check (length(btrim(donor_name)) > 0),
  donor_email text not null default '' check (donor_email = lower(btrim(donor_email))),
  kind text not null check (kind in ('cash', 'in_kind', 'sponsorship')),
  received_on date not null,
  -- Cash and sponsorships have an amount; in-kind gifts are described,
  -- because the IRS acknowledgment must not value donated property.
  amount numeric(12,2) check (amount is null or amount > 0),
  description text not null default '',
  -- The donor's own estimate for an in-kind gift, kept for our books only.
  estimated_value numeric(12,2) check (estimated_value is null or estimated_value >= 0),
  -- What we gave in return, if anything, and its good-faith value.
  goods_or_services text not null default '',
  goods_value numeric(12,2) check (goods_value is null or goods_value >= 0),
  fund text not null default '',
  acknowledged_on date,
  notes text not null default '',
  created_at timestamptz not null default now(),
  check ((kind = 'in_kind') = (amount is null)),
  check (kind <> 'in_kind' or length(btrim(description)) > 0),
  check (estimated_value is null or kind = 'in_kind'),
  check ((goods_value is null) or length(btrim(goods_or_services)) > 0)
);
create index gifts_received on homestead.gifts (received_on desc);
create index gifts_unacknowledged on homestead.gifts (received_on) where acknowledged_on is null;

create trigger audit_gifts after insert or update or delete on homestead.gifts
  for each row execute function homestead.audit_row_change();

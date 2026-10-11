-- Documenso returns a signing link for each recipient. Production Documenso
-- has no email yet, so the sender passes the link on themselves (text, DM,
-- or a phone handed over at the gate). Anyone holding it can sign, so it is
-- cleared as soon as the release is signed or declined.
alter table homestead.release_requests add column signing_url text;
alter table homestead.release_requests
  add constraint release_requests_link_only_while_pending check (status = 'sent' or signing_url is null);

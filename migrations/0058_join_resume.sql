-- A join's agreement is two Documenso calls: create the envelope, then
-- distribute it. On 2026-09-30 a core rollout cut three sends between the
-- two, leaving created-but-unsent envelopes, and the only way on was to
-- start over, which creates another. So a join now records its envelope as
-- soon as it exists, and a retry distributes that one.
alter table homestead.joins
  -- The id Documenso's webhook carries back (its externalId). Fixed when
  -- the join starts, so a retried send keeps it.
  add column release_id uuid not null default gen_random_uuid(),
  -- Created in Documenso, not yet recorded as sent.
  add column envelope_id text check (length(envelope_id) between 1 and 100),
  -- When the current send began. A send that neither finished nor failed
  -- for a while was cut off (the core restarted under it), and may be
  -- retried.
  add column send_started_at timestamptz not null default now();

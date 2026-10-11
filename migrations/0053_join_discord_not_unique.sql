-- Someone who joined, lost their page and joined again connects the same
-- Discord account to a second signed join. The unique index refused that
-- with "already exists", though they were already in. One account per
-- join is what matters (claim_join_discord); the reverse protects nothing.
alter table homestead.joins drop constraint joins_discord_user_id_key;
create index on homestead.joins (discord_user_id) where discord_user_id is not null;

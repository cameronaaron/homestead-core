-- The public join flow identifies the signer through Discord OAuth after
-- the agreement is signed. This is the reliable link between paperwork and
-- the account receiving the role; Discord invites do not grant roles.
alter table homestead.joins
  add column discord_user_id text unique,
  add column discord_username text;

alter table homestead.joins add constraint joins_discord_identity_together
  check ((discord_user_id is null) = (discord_username is null));

alter table homestead.joins
  drop column invite_code,
  drop column invite_created_at;

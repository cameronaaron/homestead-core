-- The morning digest (see homestead_domain::digest). One row per farm day
-- it was considered, whether or not there was anything to post: the
-- primary key is what makes it go out at most once a day however many
-- times or places the job runs. Unaudited, like `deadline_reminders`.
create table homestead.daily_digests (
  on_date date primary key,
  posted boolean not null,
  sent_at timestamptz not null default now()
);

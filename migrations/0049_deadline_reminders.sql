-- Deadline reminders (see homestead_domain::deadlines).
--
-- One row per reminder posted: a task, the due date it was about, and the
-- stage. The primary key is what makes a stage post once however many
-- times or places the reminder job runs; a moved due date is a new key, so
-- it is reminded afresh. Unaudited, like `deliveries`: it records that a
-- message went out, not a change anyone made.
create table homestead.deadline_reminders (
  task_id uuid not null references homestead.tasks (id) on delete cascade,
  due_on date not null,
  stage text not null check (stage in ('two_weeks', 'week', 'tomorrow', 'today', 'overdue')),
  sent_at timestamptz not null default now(),
  primary key (task_id, due_on, stage)
);

-- The Linear project an issue sits in, so its reminders can go to that
-- project's channel (grant deadlines to #grant-deadlines).
alter table homestead.linear_issues add column project text not null default '';

-- Pull every issue once more so issues already linked learn their project.
-- The pull is idempotent: it only moves tasks forward and never reopens one.
update homestead.sync_cursors set cursor = null where source = 'linear';

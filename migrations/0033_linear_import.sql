-- Linear issues become Homestead tasks: the pull (`homestead_server::linear_pull`)
-- imports open issues filed in Linear and follows every linked issue's state.
--
-- `linear_issues` is what Homestead knows about each linked issue beyond the
-- task itself: its SUN-n identifier, link, workflow state and assignee.
-- `imported` separates issues filed in Linear (Linear owns their title,
-- details, priority and due date) from issues mirrored from a Homestead task
-- (Homestead owns those; only closing flows back).

-- One task per issue, so a pull racing a mirror delivery can't double-link.
create unique index tasks_one_per_linear_issue on homestead.tasks (linear_issue_id)
  where linear_issue_id is not null;

create table homestead.linear_issues (
  issue_id    text primary key check (length(issue_id) > 0),
  task_id     uuid not null unique references homestead.tasks (id),
  identifier  text not null check (length(identifier) > 0),
  url         text not null,
  state_name  text not null default '',
  assignee    text not null default '',
  imported    boolean not null,
  -- Linear's updatedAt for the version last applied.
  updated_at  timestamptz not null
);

create trigger audit_linear_issues after insert or update or delete on homestead.linear_issues
  for each row execute function homestead.audit_row_change();

-- Where each inbound pull got to, and how its last attempt went.
create table homestead.sync_cursors (
  source      text primary key check (source in ('linear')),
  cursor      timestamptz,
  pulled_at   timestamptz,
  error       text not null default '',
  error_at    timestamptz
);

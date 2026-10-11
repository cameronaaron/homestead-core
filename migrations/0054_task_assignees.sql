-- Who is doing a task. Unassigned is a normal state ("anyone can take
-- it"), so the column is nullable. It names a Homestead person, not a
-- Linear user: Linear's assignee stays on `linear_issues` as the text
-- Linear reports.
alter table homestead.tasks
  add column assigned_to uuid references homestead.actors (id) on delete set null;
create index tasks_assigned_open on homestead.tasks (assigned_to) where status = 'open';

-- Only a person with a working account can be given a task: not a service
-- account (the API token, the Discord bot) and not someone disabled. An
-- existing assignment survives the person being disabled later, since it
-- is still true that it was theirs; only a new assignment is refused.
create function homestead.task_assignee_is_a_person() returns trigger language plpgsql as $$
begin
  if new.assigned_to is not null
     and (tg_op = 'INSERT' or new.assigned_to is distinct from old.assigned_to)
     and not exists (select 1 from homestead.actors a
                     where a.id = new.assigned_to and a.kind = 'human' and a.disabled_at is null) then
    raise exception 'a task can only be assigned to a person with an active account';
  end if;
  return new;
end $$;

create trigger tasks_assignee_is_a_person before insert or update of assigned_to on homestead.tasks
  for each row execute function homestead.task_assignee_is_a_person();

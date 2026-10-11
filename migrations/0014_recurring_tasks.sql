-- Recurring task templates (from 0003) with real schedules, and the key that
-- makes generation idempotent: one task per (template, occurrence date).

alter table homestead.recurring_task_templates
  alter column cadence_days drop not null,
  add column schedule text not null default 'days' check (schedule in ('days', 'weekdays', 'month_day')),
  add column weekdays smallint check (weekdays between 1 and 127),
  add column day_of_month smallint check (day_of_month between 1 and 31),
  -- `days` schedules count from here; it is always an occurrence.
  add column anchor_on date,
  add column lead_days smallint not null default 0 check (lead_days between 0 and 30),
  add column catch_up boolean not null default false,
  add column updated_at timestamptz not null default now();
update homestead.recurring_task_templates set anchor_on = next_due_on;
alter table homestead.recurring_task_templates
  alter column anchor_on set not null,
  alter column schedule drop default,
  add constraint recurring_schedule_fields check (
    (schedule = 'days') = (cadence_days is not null)
    and (schedule = 'weekdays') = (weekdays is not null)
    and (schedule = 'month_day') = (day_of_month is not null)),
  add constraint recurring_cursor_after_anchor check (next_due_on >= anchor_on);
create index recurring_task_templates_due on homestead.recurring_task_templates (next_due_on) where enabled;

alter table homestead.tasks
  add column template_id uuid references homestead.recurring_task_templates (id),
  add column occurs_on date,
  add constraint tasks_occurrence_names_its_template check ((template_id is null) = (occurs_on is null));
-- Running the scheduler twice, or on two instances, can't duplicate a task.
create unique index tasks_one_per_occurrence on homestead.tasks (template_id, occurs_on)
  where template_id is not null;

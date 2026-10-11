-- Saved letter templates select a presentation as well as wording. Generated
-- campaigns still snapshot the final HTML, so later design edits are harmless.
alter table homestead.grant_templates
  add column design text not null default 'sunrise'
  check (design in ('sunrise', 'meadow', 'ink', 'orchard'));

# LinkedIn publishing notes

Copy the full contents of [post.md](post.md) as the post. Attach the six PNGs below in order. [preview.png](preview.png) is a contact sheet for review and [mobile-preview.png](mobile-preview.png) shows each slide at 390 px wide; neither is a slide. The SVGs are the editable sources. PNGs are 1080 × 1350, rendered with headless Chrome:

    for f in 0*.svg; do "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --hide-scrollbars --window-size=1080,1350 --screenshot="$PWD/${f%.svg}.png" "file://$PWD/$f"; done

## Image order and alt text

1. **01-cover.png:** Goats. Gardens. A cat rescue. 55,000 lines of Rust. The software behind a community homestead in Van Nuys. A farm illustration shows a goat, chickens, seedlings, a cat and a sunrise. Caption: 209 commits. 5 days. In production.
2. **02-numbers.png:** Five days, by the numbers, Sept 27 to Oct 1: 55k lines of Rust in 5 crates, 548 tests, 197 routes, 54 SQL migrations, and 8 systems it talks to: Discord, Linear, GitHub, Buffer, Zeffy, Documenso, Resend and Cloudflare.
3. **03-outbox.png:** Never lose a harvest. A harvest row and its outbox event commit in one Postgres transaction. A dispatcher delivers to Discord, Linear and Buffer with lease, retry and dead-letter. Linear issue ids are derived from the record so redelivery can't duplicate. Every integration implements one Sink trait.
4. **04-tests.png:** Tests that read the code. Every route called with no permissions must return 403. Every store write must be attributed. Discord access is computed with real permission maths. A race test holds one transaction open, because racing two calls passed even with the lock removed.
5. **05-merge.png:** People still use the dashboards. A three-way merge of repo TOML, live API state, and the last observed state in refs/homestead/live. Repo-only edits keep the intent, live-only edits update TOML, independent edits merge, conflicts stop and name the field. The baseline is separate from the last commit because an unapplied setting isn't a dashboard edit.
6. **06-lessons.png:** Lessons learned the hard way. Linear rate limits arrive as HTTP 400. New Discord roles tie at position 1. Postgres upserts check constraints before the conflict. GitHub Actions billing went from about 24 to about 8 minutes per push. Less admin, more garden.

## Claim review

Checked against this checkout on October 3, 2026. The slides are explanatory graphics, not application screenshots.

| Claim | How it was checked |
|---|---|
| First commit Sept 27, 209 commits through Oct 1 | `git log` (root commit a26fef6 is 709 lines, so the codebase was not imported) |
| 55k lines of Rust, 5 crates | `find crates -name '*.rs' \| xargs cat \| wc -l` → 55,205; `ls crates` |
| 548 tests | `cargo test --workspace -- --list \| grep -c ': test$'` (a grep for `#[test]` attributes overcounts at 554) |
| 197 routes | unique `.route("…")` paths in `crates/server/src` |
| 54 migrations | files in `migrations/` (numbered to 0058; some numbers are unused) |
| In production | production Documenso and Supabase verified 2026-09-28/29 (AGENTS.md) |
| Outbox, lease/retry/dead-letter, Sink trait | [README](../README.md), `crates/bus/src/lib.rs` |
| Linear ids derived from the record | AGENTS.md, verified live 2026-09-28 |
| Route, attribution and access-matrix tests | `crates/server/tests/rbac.rs`, `crates/store/src/lib.rs`, `crates/integrations/src/discord/provision/fake.rs` |
| Race test story | `concurrent_splits_cannot_together_exceed_the_harvest` in `crates/store/tests/harvests.rs` and its doc comment |
| Three-way merge | `crates/server/src/systems_sync.rs` |
| Slide 6 lessons and CI minutes | AGENTS.md (each recorded with its date and measurement) |

The post makes no claim about time saved or adoption. "Five days" is the commit history and doesn't say how the code was written. If someone asks about AI assistance, answer plainly.

LinkedIn's own resizing and compression haven't been previewed in its composer.

# Agent notes — homestead core

Read README.md first. The notes below are the non-obvious findings, each
learned the hard way in this repo.

- **sqlx 0.9 rejects runtime-built SQL** (`format!`) at compile time
  (`SqlSafeStr`). Keep queries `&'static str`; share column lists via a
  `macro_rules!` + `concat!` (see `task_cols!` in `crates/store`).
- **`time` needs `serde-human-readable`**, or `Date` serialises as a
  `[year, ordinal]` tuple. Pinned by `dates_serialise_as_iso_strings`.
- **reqwest 0.13 gates `.query()` behind the `query` feature.**
- **Linear reports rate limits as HTTP 400 + `RATELIMITED`** in the body.
  Classify on the body before the status, or you dead-letter a transient
  error (`linear::interpret`).
- **`numeric(12,3)` round-trips `3` as `3.000`.** Compare decoded `Decimal`s
  in tests, not JSON strings.
- **Tests need Postgres** at `localhost:54329` (`just db`, Apple
  `container`; Docker Desktop is not used on this machine). `.cargo/config.toml`
  supplies `DATABASE_URL`, and `#[sqlx::test]` creates a throwaway DB per test.
- Enum string literals must match the migration's CHECK lists.
  `enum_strings_match_migration` enforces this, so extend both together.
- New tables go in the `homestead` schema, never `public` (see README →
  Security model). `nothing_is_created_in_the_public_schema` enforces it.
- **Discord removed bot guild creation (`POST /guilds`) on 2025-07-15.**
  A human creates the server; `discord-provision` configures it.
- **`@everyone` as a reader includes every role.** Access maths that treats
  readers as a plain set drops named writers: `{Farm Lead} ∩ {@everyone}`
  once silenced #announcements entirely. Use `Access::can_read`. The
  `access_matrix_matches_the_intended_model` test computes real Discord
  effective permissions and catches this class of bug.
- `just` loads blank `KEY=` lines from `.env` as empty strings. Treat blank
  as unset (`nonblank` in `main.rs`), or a missing token turns into a
  confusing 401.
- **Zeffy's API is read-only** (`GET /api/v1/campaigns`, bearer key). There
  is no way to create or edit a campaign programmatically, so Zeffy is
  verify-only. The only public status observed is `"active"`; drafts are
  treated as "anything else" until a real draft's status string is seen.
- **Zeffy added signed webhooks.** Verify the raw body using `Zeffy-Signature`
  (`HMAC-SHA256(secret, "{t}.{rawBody}")`), reject timestamps older than five
  minutes, and deduplicate the stable event `id`; retries are re-signed.
- This host has no `pg_dump`/`psql` on `PATH`. `scripts/pg.sh` runs
  PostgreSQL 17 tools from `PATH` only if they are 17 or newer (`pg_dump`
  refuses a newer server, and GitHub's runners ship 16); otherwise in
  `postgres:17-alpine` via Docker, or Apple `container` (a localhost URL
  runs inside `sunbrewed-pg`).
- **Linear rejects "Query too complex"** when one query's page sizes add up
  past its budget. `linear::provision::Client::fetch` uses several small
  queries on purpose.
- **GitHub Actions runs `run:` with `bash -eo pipefail`.** A step that
  captures `${PIPESTATUS[0]}` for a job summary needs `set +e` first, or it
  dies before writing the summary.
- **Discord creates new roles tied at position 1, the bot's own role
  included; nothing shifts.** Reordering them to 1..n puts them above the
  bot and fails with 50013 Missing Permissions. The bot *may* lift its own
  role above them in the same `PATCH /guilds/{id}/roles` request. The test
  fake first modelled "insert and shift up" and so passed while production
  failed. It now reproduces the observed behaviour (verified to fail against
  the old code before the fix went in).
- **Buffer's GraphQL uses custom scalar types** (`OrganizationId!`, not
  `String!`), and reports a wrong organization id as `UnexpectedError`
  "Organization not found", which is permanent, not retryable. The shape was
  validated against the live API with a bogus id, so nothing was created.
- **GitHub Actions' `GITHUB_TOKEN` can't manage org settings or teams.** The
  org spec needs a separate `GH_ORG_ADMIN_TOKEN` secret.
- **Typst `place()` in page content is relative to the text area, not the
  page.** A signature block placed at page coordinates landed 0.9 in right
  and 0.8 in low. Anything that must sit at page coordinates (the signing
  boxes Documenso fields target) goes in `page(foreground: ...)`.
- **Documenso's webhook form rejects private IPs in the browser, even when
  `NEXT_PRIVATE_WEBHOOK_SSRF_BYPASS_HOSTS` would let the server accept
  them.** Use an allowlisted *hostname*. Self-hosted Documenso also sends
  PostHog telemetry unless `DOCUMENSO_DISABLE_TELEMETRY=true`.
- **This Mac's firewall blocks all incoming connections**, so containers
  (Apple `container`, 192.168.64.0/24) can't reach services on the host at
  192.168.64.1. Run anything a container must call back into as a container
  too; don't weaken the firewall.
- **Config lives in `homestead.toml`; the environment is secrets only.** A
  stale `BIND_ADDR` in `.env` silently overrides `[server].bind`.
- **Apple `container build` (CLI 1.4.1, builder shim 0.13.1) drops the
  contents of every subdirectory in the build context**; only top-level
  files arrive (minimal repro: one file in `sub/`, then `COPY . .`, then
  `find`). GitHub's Docker buildx builds the same Dockerfile fine. Locally,
  compile in `rust:1-slim-trixie` with the source mounted
  (`CARGO_TARGET_DIR=/src/target/linux`) and run the binary in a plain
  Debian container instead.
- **Discord's List Guild Members needs the privileged Server Members intent,
  even over REST** (403 `Missing Access` 50001 without it). It is on for our
  bot, and the provisioner lists everyone so unlisted role-holders get
  reported. If it's ever turned off, Search Guild Members works without it,
  but it prefix-matches username, display name *and* nickname
  ("c" found `kamiof2000` through "Cam"), so filter to the exact username.
- **Granting a member a spec role fails (50013) until the role has been
  ordered below the bot's**, and ordering only happens on the pass after the
  roles are created. The plan therefore holds member grants back while any
  `CreateRole`/`ReorderRoles` is pending. The fake enforces the hierarchy on
  grants, and the brand-new-server members test failed without that hold.
- **Linear's `users` query includes app users** (e.g. "Linear",
  `app: true`), and its `UserRoleType` calls a member `user`. The spec says
  `member`, and apps are filtered out before planning. `userChangeRole` can't
  touch the owner, so an owner is never planned for a role change.
- **Settings pass-through fails on typos only because we make it.** Discord
  silently ignores unknown PATCH fields and Documenso strips unknown
  `meta`; Linear (GraphQL) and GitHub reject them. So Discord relies on
  "did not converge" naming the field (the fake drops unknown keys the same
  way), and Documenso's verify checks names against `openapi.json`.
  Compare with `settings::covers`, never `==`: GitHub nests extra fields
  under objects like `security_and_analysis`, and Linear Floats read back
  as `6` or `6.0`.
- **Apple containers start with no DNS servers**, so `cargo build` in one
  fails with "Could not resolve host: index.crates.io". Pass `--dns 1.1.1.1`.
  `debian:trixie-slim` also has no CA certificates, and the core refuses
  to start without them (the Dockerfile installs them; a local runner can
  use `rust:1-slim-trixie`). `scripts/onboarding-e2e.sh` encodes all of it.
- **The service worker caches only the manifest, the icon, and `/offline`.**
  Authenticated HTML stays on the network. A shared phone must not keep
  someone else's Today after they walk away, and a field post still works
  with JavaScript off. Offline notes wait in the page, not in the worker.
- **Documenso draws signing fields on a canvas**, not as DOM elements, so a
  browser test clicks them by page position. Text fields open a dialog
  with one input and an "Enter" button. Its container runs as non-root, so
  pinning `homestead-core` in `/etc/hosts` needs `container exec -u 0`.
- **Legal docs may run past one page** (since 2026-09-29): `release.typ`
  draws the signature block on the last page only, and a multi-page
  document must end in `signing[...]` so its closing terms sit on the
  signature's page (a bare signature page could be stapled to anything).
  The layout asserts it. The first version of that assert shared a
  `context` with the spacer: a failing assert dropped the spacer, the
  document shrank to one page, the assert passed, and Typst reported only
  "did not converge" instead of an error. Keep them separate.
- **A Cloudflare container bills its Durable Object too.** Each container
  has a Durable Object billed for wall-clock time while active (128 MB,
  400k GB-s/month included account-wide, overage rounded *up* to whole
  million-GB-s units at $12.50). A keep-alive cron therefore costs far more
  than the container's own ~$1.75. That's why deploy/cloudflare sleeps when
  idle.
- **`wrangler deploy` needs Docker Desktop, not Apple `container`**: Apple's
  builder drops subdirectories (see above), and the image must be
  linux/amd64. Right after the first deploy, requests fail with "There is
  no container instance that can be provided" until the image reaches
  Cloudflare's locations.
- `@cloudflare/workers-types` moved to major version 5; wrangler 4.143's
  optional peer dependency wants `^5.20260926`, and `^4` fails `npm install`.
- **Supabase (project znebkrbmufqxdzmlelwg, AWS us-east-2): use the session
  pooler `aws-0-us-east-2.pooler.supabase.com:5432`, user
  `postgres.<ref>`.** The dashboard's default string is the direct host,
  which is IPv6-only (AAAA record only), and `aws-1-us-east-2` rejects this
  project. Verified 2026-09-28: sqlx keeps `_sqlx_migrations` in `public`,
  but the Data API refuses it to the publishable key (no anon grants), and
  the `homestead` schema isn't exposed at all.
- **Documenso's migrations hard-code `"public"`** (e.g. `"public"."User"` in
  `20240205120648_create_delete_account`), so `?schema=documenso` fails
  partway. It needs its own *database*; Supabase's pooler reaches any
  database on the server (`.../documenso`).
- **A running Cloudflare container keeps its start-time environment.**
  `wrangler secret put` and `--containers-rollout=immediate` don't replace
  it, so both Workers fingerprint the environment at start and restart on a
  mismatch. Container stdout isn't in `wrangler tail`; reproduce locally
  with the same image and env (Docker Desktop) to see it.
- **Driving a React form before it hydrates submits it as a GET**, putting
  every field (a password once) in the URL and the Worker's logs. Wait for
  `networkidle`, and block any request whose URL contains `password=` with
  `page.route`. Read credentials from `.env` inside the script, never inline.
- macOS `sed` has no `\|` alternation in basic regexes: it silently doesn't
  match. Use `sed -E` with `(a|b)`, and check the result.
- Documenso signup needs a signature (the `signature-pad-dialog-button`),
  and without SMTP the confirmation email fails; mark the steward verified
  in the database (`"User"."emailVerified"`).
- **Resend idempotency keys expire after 24 hours.** Grant delivery reclaims a
  stale `sending` lease after ten minutes and reuses `grant-message/<uuid>`, so
  a normal post-send crash is retried inside that window. Keep the immutable
  message snapshot and short lease together; a manual resend days later is a
  materially different operation, not an automatic retry.
- **The claude.ai Linear connector is signed in to Cameron's personal
  workspace, not `sunbrewedhomestead`.** It found no team "SUN" and reported
  zero issues, which looked like an empty homestead workspace. Reach the
  homestead workspace through `LINEAR_API_KEY` (the provisioner, or GraphQL
  directly).
- **A Linear invite also creates an inactive placeholder user** for the
  invitee (observed 2026-09-29: `active: false`, `lastSeen: null`, beside
  an open `organizationInvites` row). Check open invites before treating
  an inactive user as suspended, or a pending invite tells a person to
  "unsuspend" someone who simply hasn't accepted yet.
- **Linear label groups are exclusive**: an issue takes at most one label
  from "Area", one from "Working group", one from "Season". `issueCreate`
  rejects two with "labelIds not exclusive child labels", and the rejected
  create still uses up an issue number.
- `string_enum!` is a `macro_rules!` in `domain/src/lib.rs`, so a domain
  submodule that uses it must be declared *after* the macro. Each new
  migration's CHECK lists are pinned by `tests::assert_check_list`.
- To tell an upsert's insert from its update, `returning (xmax = 0)` is true
  for a fresh row. `contacts` uses it, and its `where` clause makes the
  upsert return no row for someone who unsubscribed, so imports can't
  re-subscribe them.
- An incremental rustc ICE ("Found unstable fingerprints for
  evaluate_obligation") dropped `rustc-ice-*.txt` files mid-`just check`.
  The rerun passed. It's a compiler bug, not ours, and the files are
  gitignored.
- **Secrets live in `~/.local/share/sunbrewed/`, outside the iCloud-synced
  Desktop.** `.env`, `deploy/documenso/.env` and `deploy/documenso/cert.p12`
  are symlinks into it. Don't replace a symlink with a real file.
- Org facts (EIN, fiscal year, mailing address) come from the IRS master
  file, and the IRS acknowledgment letter prints them from
  `domain::gifts::ORG_*`. Check any claim about the org against a source
  before putting it in public copy. An earlier session invented the names
  "Ava, Maria, Omar" for the donor thank-you email.
- **Every store write runs in `self.begin()`**, which sets the
  transaction-local `homestead.actor`/`homestead.actor_id` the audit trigger
  reads. A write on `&self.pool` is silently attributed to `system`;
  `writes_are_attributed` scans the store's source and fails on one. Session
  settings (`set_config(..., false)`) would leak one request's identity into
  the next user of the pooled connection, so they stay transaction-local.
- **Handlers get a database only through `Can<cap::X>` / `Authed`.**
  `AppState.system_store` writes as `system` and is allowed only in sign-in,
  webhooks and health (`only_unauthenticated_paths_use_the_system_store`).
  Put `Can<…>` first in a handler's arguments: axum runs extractors in
  order, and a `Form` before it would answer 422 to someone who should get
  403. `a_caller_without_capabilities_is_refused_everywhere` reads every
  `.route(` in `crates/server/src` and fails on any non-403; it was verified
  by downgrading `/gifts` to `Authed` (it reported `GET /gifts → 200`).
- `sessions`/`setup_links`/`api_tokens` CHECK their hash column is 64 hex
  chars. Test fixtures that invent a token must use hex digits.
- **`INSERT ... ON CONFLICT DO UPDATE` checks CHECK constraints on the
  proposed row before the conflict path runs.** An upsert of `on_hand =
  -3` into a balance of 10 fails `on_hand >= 0`. `apply_stock_movement`
  therefore updates first and inserts only if nothing matched.
- Cached stock (`inventory_items.on_hand`, `inventory_balances`) may only
  change inside the ledger trigger: the guards allow it only at
  `pg_trigger_depth() > 1`. Never "fix" a quantity with an UPDATE; insert a
  movement.
- **Test S3 code locally with `rclone serve s3`**, not MinIO: the
  `quay.io`/Docker Hub MinIO pulls hung or needed credentials under Apple
  `container`. rclone verifies SigV4 (a forged secret is refused) but does
  **not** check `x-amz-content-sha256` against the body, so run
  `s3_live` with `S3_TEST_NO_HASH_CHECK=1` against it; that one property is
  only proven against real S3/R2. Verified 2026-09-28: PUT with stored
  type/disposition, presigned GET, unsigned GET refused, delete.
- **The previous Git revision is not a safe common ancestor for pulling live
  state.** Spec edits the provisioner couldn't apply (no `GH_ORG_ADMIN_TOKEN`
  in CI; 2FA and Buffer's org name need a person) are in every recent
  revision but were never live. With that ancestor, `systems-sync` would have
  "pulled" `default_repository_permission = "read"` and 2FA off over the
  hardened spec (verified 2026-09-28 against the live org). The ancestor is
  the live snapshot under `refs/homestead/live`, recorded after each apply.
  Provision/verify exit codes can't gate it either: `Pending` findings and
  GitHub advisories exit 0 without live matching the spec. The ref sits
  outside `refs/heads`/`refs/tags` so the mirror (tags, never forced) and
  Workers Builds ignore it.
- **Push to `Sunbrewed-Homestead/homestead`, not `cameronaaron/homestead`.**
  The personal repo is the mirror's target and has no Actions secrets. On
  2026-09-29 this checkout's `origin` pointed at it: five commits "pushed"
  but never deployed, and Systems/mirror failed there for missing
  `BUFFER_API_KEY`/`PERSONAL_MIRROR_KEY`, which reads like a broken
  pipeline. Check with `gh api repos/Sunbrewed-Homestead/homestead/commits/main`.
- **GitHub Actions minutes: the repo is private on the free plan (2,000
  min/month), and each job bills at least a whole minute.** Measured
  2026-09-28 over 71 runs: ~24 billed min per push to main, ~7,000/month
  at that pace. The image job's `cache-to: type=gha,mode=max` wrote ~330 MB
  per run and pushed the cache past its 10 GB cap, evicting the Rust caches
  (and never hit, since `COPY . .` precedes `cargo build`). Now: image.yml is
  path-filtered and uncached, Systems is one job, and CI and Systems share
  one Rust cache (`shared-key: homestead`, saved from main only, with
  `CARGO_PROFILE_*_DEBUG=line-tables-only` set identically in both, since
  profile env is part of the key). Warm: CI 280 s, Systems 66 s (build 42 s,
  down from a 106 s release build); ~8 billed min per push. A golden image
  was considered and rejected: custom runner images need paid larger
  runners, and pulling a prebuilt toolchain image costs about what the
  cache restore does (~16 s).
- **rust-cache only saves when the job succeeds**, so a red CI on main
  silently disables caching for every workflow that shares it. CI was red
  from 249fbb1 to f2ef986 for that reason as well as its test failures.
- `writes_are_attributed` reports line numbers one past the `sqlx::query`
  line (it counts the partial line, then adds one).
- **R2 has no object versioning** (`PutBucketVersioning` unimplemented).
  Its retention is bucket locks: an age per prefix, refusing delete *and*
  overwrite of younger objects for everyone, a stolen app key included. So
  an attachment delete can't remove bytes at once: it queues them in
  `attachment_purges` and the hourly schedule purges after
  `retain_deleted_days`, which must be at least the lock's age.
- **Linear's `issueCreate` keeps a client-chosen `id`** (UUID v4 format).
  Verified live 2026-09-28: the issue got exactly our id, a second create
  with it failed `INPUT_ERROR` "conflict on insert of Issue", and
  `issue(id:)` for an unknown id fails `INPUT_ERROR` "Entity not found:
  Issue". The sink derives ids from what it mirrors and looks up before
  creating; this is what makes redelivery duplicate-free.
- **iCloud evicts files under the synced Desktop, `node_modules`
  included.** An evicted `wrangler-dist/cli.js` (`ls -lO` shows
  `dataless`) makes `npx wrangler` print nothing and exit 0: a lock
  "added" that wasn't. `scripts/r2-setup.sh` runs wrangler from
  `~/.cache/homestead-wrangler` for that reason; check wrangler's output,
  not its exit code. Evicted sources also fail `cargo build` with "os error
  89"; reading the file brings it back.
- Wrangler's OAuth login can create R2 buckets and lock rules but not R2
  API tokens (no token scope). S3 keys for R2 are made in the dashboard.
- **A race test that races two store calls with `tokio::join!` doesn't
  race.** The disposition over-allocation test passed with the harvest-row
  lock removed (5/5 runs). Hold transaction A open with its insert
  uncommitted, start B, assert B is still blocked, then commit A: that
  failed without `for update` and passes with it
  (`concurrent_splits_cannot_together_exceed_the_harvest`).
- Trigger rule violations (`raise exception`, SQLSTATE P0001) map to
  `StoreError::Conflict` with the trigger's message. Keep those messages
  about the rule, never a row's data: they reach the person.
- **`backdrop-filter` (like `transform`/`filter`) makes an element the
  containing block for its `position: fixed` descendants.** The app bar's
  blur pinned the wide-screen navigation drawer inside the 4.5rem bar,
  showing only "Today". The drawer layout turns the blur off.
- **The inline stylesheet ships in every page body,** so a test that does
  `page.find("/setup/")` finds a CSS selector before the real link. Style
  with classes, not attribute selectors that quote URLs.
- **The Linear pull never reopens a task.** An issue that still looks open
  may be a Homestead completion whose `task.completed` delivery hasn't
  reached Linear yet; reopening would undo the person's "Done". Imports
  emit no `task.created`, or the first pull posts the whole backlog to
  Discord.
- **Discord "X joined" notices are messages** (type 7, empty content), and
  in a quiet server they're all #general holds. The feed shows only types
  0 and 19. Feed channels must be readable by the Community role in the
  spec: the Community page shows them to everyone who can open it.
- **Buffer's `Post.dueAt/sentAt` are ISO strings but `Idea.createdAt` is
  Unix seconds** (observed 2026-09-29), and every account starts with two
  sample ideas. Homestead's drafts are told apart by `buffer::DRAFT_MARK`.
- **`writes_are_attributed` lists store modules by hand** and fails on any
  file in `src/` missing from the list (`outbox.rs` excepted), so a second
  coverage test would only duplicate it. `field.rs` and `notify.rs` sat
  outside it (the test failed, unnoticed among other failures) until
  2026-09-29: a suite that's already red hides new failures.
- **sqlx decodes tuples only up to 16 columns.** `animals` passed that on
  2026-09-29 and failed with an unsatisfied `FromRow` bound; its rows now
  decode into a `#[derive(sqlx::FromRow)]` struct. Do the same for any
  wide table instead of splitting the select.
- **Maud escapes `& < > "` but not `'`.** A test looking for
  `Kiki&#39;s` fails; the page says `Kiki's`.
- **The full suite occasionally fails one test with "failed to connect
  test pool: unexpected response from SSLRequest: 0x00"** against the local
  Postgres container, under the load of every `#[sqlx::test]` creating a
  database at once. The same suite passed 3/3 alone and the rerun of the
  full gate passed. It's the container, not the code; rerun before
  theorising.
- **`cargo test … | grep …; git commit` commits on a failing test**,
  because the pipeline's exit status is grep's. Chain commits behind
  `just check` (or anything that exits non-zero on failure), never behind
  a grep of test output. A microchip commit went in red this way and was
  amended.
- **A pace or rate over "days of history" must round the elapsed time,
  not ceil it.** Rows written a moment before the query make ten days
  read as 10.00001, and `ceil` made every pace a day slower
  (`usage_paces`).
- **Public (no-account) pages are deliberate and enumerated**: `/adopt`
  and `/t/{code}` (QR tags) besides sign-in, webhooks and the PWA shell.
  Adding one means adding it to `OPEN` in `tests/rbac.rs` with the reason,
  bounding every field it takes, and writing through
  `AppState::tag_store`/`public_store`. The form/link sweep
  (`no_page_offers_a_form_or_link_its_viewer_cannot_use`) found 48
  forms and links shown to people who'd be refused when it was added.
- **Documenso v2 `envelope/distribute` returns each recipient's
  `signingUrl`**, and envelope `meta.distributionMethod = "NONE"` stops it
  trying to email. Verified 2026-09-29 against the production instance
  (probe envelope created, link read, envelope deleted). Production has no
  SMTP, so releases.toml sets NONE and the Releases page shows the links to
  senders only. `scripts/onboarding-e2e.sh` keeps NONE, like production,
  and opens the links in a browser.
- **"Observation" already means crop health** (`crop_observations`,
  `domain::health::Observation`). The generic measurement record is
  therefore a **reading** (`readings` + `reading_values`). Audit visibility
  is per table, but readings are visible per subject, so the audit rule is
  `animals.read`: the audit readers are steward, board and service
  accounts, and only service accounts lack `animals.read`.
- **A self-crossing outline's shoelace area is wrong, not just odd**: a
  bow tie measures zero. `map::Shape::validated` checks crossings *before*
  area so the person is told the real reason.
- `homestead.local.toml` (git-ignored) drifts behind `homestead.toml`: on
  2026-09-29 it lacked `[grants]`, `[attachments]` and `[stand]`, and the
  server refused to start. For a local preview, copy it to a temp file,
  append the missing sections and set every `enabled = true` to false.
- **Documenso fields come from the rendered layout, not from TOML.** Every
  box in `legal/release.typ` emits `<field>` metadata, and `build.sh`
  writes it to `legal/<name>.fields.json` (`typst eval
  'query(<field>)…'`); `here().page()` inside the page foreground reports
  the true page. A document whose text overflows draws a second
  signature box, which the build and `parse_layout` refuse. Body text
  passed in as a content block (`[...]`) drops its leading paragraph
  break, so the layout adds `parbreak()` after the intro; without it §1
  ran into the intro.
- **`.env`'s `DOCUMENSO_API_TOKEN` is the local instance's; production's
  is `PROD_DOCUMENSO_API_TOKEN`.** The production instance answers the
  local one with 401 "Invalid token". Production took 33 s to answer from
  cold (2026-09-29), which is why `/onboard` defers its Discord reply and
  edits it when the link is ready. Documenso v2 also accepts INITIALS and
  RADIO fields (`fieldMeta.values`), verified live that day; the
  multi-page packet that used them was replaced by a one-page agreement.
- **The admin token is an API bearer, not a browser session.** The old
  `documenso-e2e.sh` posted the Releases form with `sbh_session=$ADMIN_TOKEN`
  and would have been redirected to /login; a browser needs `user-add` then
  `setup-link`. `scripts/onboarding-e2e.sh` replaced it (2026-09-29): it
  runs production's pinned Documenso image (the e2e first ran `latest`,
  2.19.0, against production's 2.18.0), signs in Chrome, and checks the
  stored rows and the sealed PDF. In Documenso's signing page, clicking a
  filled field clears it, so a document must be signed in one pass.
- **Discord OAuth redirect URIs can only be added in the Developer Portal.**
  `PATCH /applications/@me` with `redirect_uris` answers 200 and changes
  nothing (verified 2026-09-29), and the client secret can't be read back,
  only reset there. `/join` stays "not open" until the Worker has
  `DISCORD_CLIENT_SECRET`; with the secret but no registered
  `{public_url}/join/discord`, every joiner fails at "Connect Discord".
- **This private free-plan repo has no branch protection or rulesets**
  (403 "Upgrade to GitHub Pro"), so `gh pr merge --auto` can't wait for
  checks. `dependabot-automerge.yml` labels non-major updates and merges
  them daily once every check is green, then dispatches Systems, because a
  `GITHUB_TOKEN` merge starts no push workflows (Workers Builds still
  deploys it).
- **`startAndWaitForPorts` isn't free on a running container**
  (`@cloudflare/containers` 0.3.7): it still pings the port and runs
  `onStart` under `blockConcurrencyWhile`, so calling it on every request
  queued all of Documenso's requests behind each other (20 parallel took
  ~4.6 s each; the sign Worker's median wall time was ~10 s). Each call
  on a container stub is also a separate Durable Object round trip. Both
  Workers now make one `fetch` call and do the checks inside the object,
  starting the container only when it isn't healthy. Worker wall time per
  script is in the GraphQL `workersInvocationsAdaptive` dataset; wrangler's
  OAuth token can read it but not the observability telemetry API.
- **To test a migration's backfill, stop partway with
  `MIGRATOR.run_to(n, &pool)`** under `#[sqlx::test(migrations = false)]`,
  insert rows in the old shape, then `MIGRATOR.run(&pool)`.
  `existing_names_become_one_volunteer_each` does this for 0055. Raw-SQL
  fixtures in other tests break when a migration adds a `not null`
  column (the reports fixture did); fix the fixture, not the constraint.
- **An open task's assignee is current responsibility, not history.**
  Disabling a person releases only their open tasks; completed tasks retain
  who did them. Keep that transition in `Store::set_actor_disabled` so the
  actor change and task audit rows share one attributed transaction.
- **Serde treats a missing `Option<T>` field as `None`, even through a
  transparent newtype.** When JSON `null` is meaningful but omission or a
  typo must fail, use a non-optional enum that accepts a UUID or null and
  deny unknown fields. `TaskAssignment` pins both cases with negative tests.
- **Back up only the `homestead` and `public` schemas.** A whole-database
  dump of Supabase restores nowhere else (`extension "supabase_vault" is not
  available`), and `pg_restore` of it fails on `CREATE SCHEMA public`, so the
  drill skips that TOC entry. The drill compares table and migration counts
  with the source (a restore that "succeeds" missing a table was the failure
  to guard against; verified by excluding one). `backup.yml` runs nightly,
  encrypts to the age key in `scripts/backup-age-recipient.txt` (private half
  in `~/.local/share/sunbrewed/homestead/`, not GitHub) and keeps artifacts
  90 days, GitHub's maximum: older monthly points are a manual download.
- **`extract(epoch from ...)` is `numeric` in Postgres 14+, not `float8`.**
  Decoding it as `f64` fails only once the value is non-NULL, so
  `/api/v1/metrics` returned 500 from the first recorded restore drill until
  it was cast (`metrics_report_the_age_of_a_recorded_restore_drill`). Cast
  aggregates to the Rust type at the query; an empty table hides the bug.
- **The core's own request counters restart at zero whenever its container
  wakes** (idle sleep after 10 minutes), so a 5xx alert on them sees little.
  The Worker counts 5xx in its Durable Object
  (`homestead_edge_server_errors_*`, `deploy/cloudflare/src/errors.ts`) and
  `scripts/check-metrics.sh` fails the nightly job on them. A real error
  therefore keeps that job red for 24 hours after the fix.
- **`echo "x=$(cmd)" >> $GITHUB_OUTPUT` hides `cmd`'s failure** (echo exits 0).
  Assign on its own line first. Same trap as the grep pipeline above.
- **Apple `container`'s service can die** ("XPC connection error"; `container
  system start` revives it, `just db` restarts `sunbrewed-pg`), and after one
  restart new containers could not resolve DNS even with `--dns`. Docker
  Desktop was the fallback for reaching Supabase.
- **A core rollout kills work spawned after a response** (`tokio::spawn`),
  and the page never hears. On 2026-09-30 three joins stopped between
  Documenso's envelope create and distribute. So a join records its envelope
  between the two calls, and a send that neither finished nor failed within
  five minutes counts as stalled and can be retried. Documenso's
  `envelope/distribute` is idempotent (a second call returns the same
  signing link, verified in production), so retrying it is safe; retrying
  `create` is not.
- **California's wildlife-confinement rule changed on 2025-08-13.** Under
  14 CCR §679.2 an unpermitted finder now has at most 24 hours (48 only for a
  California licensed veterinarian), and must notify a permitted rehabilitator
  or CDFW within that same 24 hours. The old §679/48-hour copy is stale.

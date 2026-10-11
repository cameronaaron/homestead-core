I made the first commit on September 27. Five days and 209 commits later, a community homestead in Van Nuys was running on it in production: 55,000 lines of Rust and 548 tests.

Sun Brewed Homestead grows food, cares for animals, runs workshops and a farm stand, and rehomes rescue cats through Kiki's Cat Club. All of that means records: what was planted and harvested, who's doing which job, what's running low, who signed which volunteer agreement.

A few engineering decisions I'm proud of:

→ Transactional outbox. A harvest and its event are written in the same Postgres transaction. A dispatcher delivers to Discord, Linear and Buffer with leases, retries and a dead-letter queue. An outage delays a message but doesn't lose it, and Linear issue ids are derived from the record, so a retry can't create a duplicate.

→ Tests that read the code. One test finds every route in the source and calls it with no permissions; anything other than a 403 fails the build. Another scans the data layer and fails on any write that isn't attributed to a person in the audit log.

→ Race conditions, tested honestly. My first concurrency test passed even with the row lock deleted. Two parallel calls don't reliably overlap. The version that shipped holds one transaction open on purpose, and it fails every time without the lock.

→ A three-way merge for dashboards. Discord, Linear and GitHub are configured in TOML. When someone edits a setting in a dashboard, a sync compares the repo, the live API, and the last observed state (stored in its own Git ref). Dashboard edits come back as commits, independent edits merge, and conflicts stop the sync and name the field.

The APIs taught me things too. Linear reports rate limits as HTTP 400. New Discord roles all land at position 1. A Postgres upsert checks constraints before it checks for a conflict. Each of those is now a test or a written note in the repo.

And all of it is there so the people running the farm spend less time on admin and more time in the garden.

#Rust #PostgreSQL #SoftwareEngineering #GitOps

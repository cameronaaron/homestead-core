# Homestead core on Cloudflare Containers

A small Worker (`src/index.ts`) forwards every request to **one** container
running the repo's production `Dockerfile`. The core does its own
authentication, applies its database migrations at startup, and runs the
outbox dispatcher in the same process.

## What it costs

Prices from developers.cloudflare.com (checked 2026-09-28). Containers need
the **Workers Paid plan, $5/month**, which this account already has for
another container, so that isn't new money.

Three things are billed while the core is **awake**, and nothing while it
sleeps:

| While awake | Rate | Included each month (shared with the account's other containers and objects) |
|---|---|---|
| Container memory: `lite` = 0.25 GiB | $0.0000025 per GiB-second | 25 GiB-hours |
| Container disk: 2 GB | $0.00000007 per GB-second | 200 GB-hours |
| Container CPU: only what it actually uses | $0.000020 per vCPU-second | 375 vCPU-minutes |
| Its Durable Object: 128 MB | $12.50 per million GB-seconds, **rounded up to a whole million** | 400,000 GB-seconds |

**Default: sleeps when idle.** The core stops 10 minutes after the last
request (`sleepAfter`), and there is no keep-alive cron. Even an hour of
use every day (~30 hours a month) is about $0.08 of container time (before
the included amounts) and 13,500 GB-seconds of Durable Object time, far
inside the 400,000 included. Expect cents.

One cron runs daily at 15:00 UTC, to send deadline reminders when nobody
has opened the app. Each wake keeps the core up about 10 minutes: ~5 hours
of container time a month and 0.125 GB × 600 s × 30 ≈ 2,250 GB-seconds of
Durable Object time, well inside the included amounts. And
`max_instances: 1` means traffic can never start a second container.

The trade-offs of sleeping:

- The first request after a nap waits a few seconds while the core starts.
- A delivery to Discord, Linear or Buffer that fails and needs a retry waits
  until the core is next awake. Normal deliveries go out within seconds of
  the change that caused them, while the core is still up.
- Discord slash commands must answer within 3 seconds, which a cold start
  can miss. They aren't registered yet.

**Always-on (opt-in).** Add `"triggers": { "crons": ["*/5 * * * *"] }` to
`wrangler.jsonc`. The container itself is then about $1.75/month (memory
$1.42 + disk $0.32 after the included amounts; up to $2.01 if other
containers already use those). **But the Durable Object stays active all
month too: 128 MB × 730 h ≈ 336,000 GB-s.** Together with the account's
other always-on container that probably passes the 400,000 included, and
any overage is billed as a full million GB-s: **+$12.50/month**. Check
Workers & Pages → Durable Objects usage in the dashboard before turning it
on.

## Deploy

Needs Docker running (the image is built for linux/amd64) and
`npx wrangler login`.

```sh
cd deploy/cloudflare
npm ci
# Secrets, once (each prompts for the value):
for s in DATABASE_URL ADMIN_TOKEN DISCORD_WEBHOOK_URL DISCORD_BOT_TOKEN DISCORD_CLIENT_SECRET \
  DISCORD_GUILD_ID LINEAR_API_KEY BUFFER_API_KEY DOCUMENSO_API_TOKEN DOCUMENSO_WEBHOOK_SECRET; do
  npx wrangler secret put "$s"
done
npx wrangler deploy
```

- `DATABASE_URL` is Supabase's **session pooler** string (port 5432). The
  direct `db.<ref>.supabase.co` host is IPv6-only, and the transaction
  pooler (6543) breaks prepared statements.
- Each integration enabled in `homestead.toml` refuses to start without its
  secret. Documenso is off until it's hosted somewhere reachable; then set
  its `base_url`, enable it, and add `DOCUMENSO_API_TOKEN` and
  `DOCUMENSO_WEBHOOK_SECRET`.
- Logs: `npx wrangler tail`, or the Workers dashboard (observability is on).

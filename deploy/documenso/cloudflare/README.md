# Documenso on Cloudflare Containers

Legal releases are signed at <https://sign.example.org>.
A Worker (`src/index.ts`) forwards every request to **one** container running
Documenso's official image, pinned in `Dockerfile`.

**Why a container and not Pages or a plain Worker:** Documenso's server needs
native Node modules (Prisma's query engine, Skia for rendering PDFs, argon2
for passwords). Workers only run JavaScript and WebAssembly, so it can't run
there without rewriting it.

## What it costs

`basic` (1/4 vCPU, 1 GiB memory, 4 GB disk), because Documenso's Node server
uses ~410 MB. It sleeps 15 minutes after the last request and costs nothing
while asleep. Awake it is about **$0.01 an hour** (memory $0.009, disk
$0.001; CPU is billed only when busy). Twenty signings a month at ~15
minutes each comes to about **$0.05**. `max_instances: 1` keeps it at one
container whatever the traffic. The Workers Paid plan is already paid for.

The first request after a nap waits while Documenso starts (Node startup
plus its migration check), which can take a while; the Worker allows two
minutes. Later requests are fast.

## Data

- **Database:** a separate `documenso` **database** on the homestead's
  Supabase server (`.../documenso` instead of `.../postgres` on the pooler
  string). Not a schema: Documenso's migrations hard-code `"public"."User"`
  and fail anywhere else (the first attempt stopped at migration
  `20240205120648_create_delete_account`). Supabase's Data API only serves
  the `postgres` database, so none of this is exposed. Uploaded PDFs are
  stored in the database too (Documenso's default upload transport).
- **Keys:** `NEXTAUTH_SECRET` and the two encryption keys are copied in
  `homestead/.env` as `PROD_DOCUMENSO_*`. **If the encryption keys are lost,
  stored documents can't be decrypted.**
- **Signing certificate:** the same self-signed `deploy/documenso/cert.p12`
  as the local instance, passed as base64 in
  `NEXT_PRIVATE_SIGNING_LOCAL_FILE_CONTENTS`.

## Secrets

Set once with `npx wrangler secret put NAME` (`just documenso-cloudflare-secrets`
does all of it from `.env`):

| Secret | From |
|---|---|
| `NEXT_PRIVATE_DATABASE_URL`, `NEXT_PRIVATE_DIRECT_DATABASE_URL` | `PROD_DATABASE_URL` with database `documenso` |
| `NEXTAUTH_SECRET`, `NEXT_PRIVATE_ENCRYPTION_KEY`, `NEXT_PRIVATE_ENCRYPTION_SECONDARY_KEY` | `PROD_DOCUMENSO_*` in `.env` |
| `NEXT_PRIVATE_SIGNING_PASSPHRASE`, `NEXT_PRIVATE_SIGNING_LOCAL_FILE_CONTENTS` | `deploy/documenso/.env` and `cert.p12` |

Non-secret settings are `vars` in `wrangler.jsonc`. Signup is off: only
stewards have accounts.

## Email (not set up yet)

Without SMTP, Documenso can't email signing links; send the link from the
Releases page yourself. The domain's mail is Google Workspace, so the
simplest setup is Gmail SMTP with an app password from a Workspace mailbox
(e.g. `sign@sunbrewedhomestead.org`): no DNS changes, and SPF already <!-- oss-publish:allow: org domain example -->
authorizes Google. Then set:

```sh
npx wrangler secret put NEXT_PRIVATE_SMTP_USERNAME   # the mailbox
npx wrangler secret put NEXT_PRIVATE_SMTP_PASSWORD   # its app password
```

and add to `vars`: `NEXT_PRIVATE_SMTP_TRANSPORT = "smtp-auth"`,
`NEXT_PRIVATE_SMTP_HOST = "smtp.gmail.com"`, `NEXT_PRIVATE_SMTP_PORT = "465"`,
`NEXT_PRIVATE_SMTP_SECURE = "true"`,
`NEXT_PRIVATE_SMTP_FROM_ADDRESS = "<the mailbox>"`.

Cloudflare's own Email Service would need the domain's DNS moved from Wix to
Cloudflare.

## Deploy

Needs Docker running, or Workers Builds (see the repo README).

```sh
cd deploy/documenso/cloudflare && npm ci && npx wrangler deploy
```

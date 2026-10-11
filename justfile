set dotenv-load

# Local Postgres 17 for dev and tests (Apple `container`).
db:
    container system start >/dev/null 2>&1 || true
    container start sunbrewed-pg 2>/dev/null || container run -d --name sunbrewed-pg \
        -e POSTGRES_PASSWORD=homestead -e POSTGRES_DB=homestead -p 54329:5432 \
        docker.io/library/postgres:17-alpine

test:
    cargo test --workspace

check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

run:
    cargo run -p homestead-server --bin homestead -- serve

migrate:
    cargo run -p homestead-server --bin homestead -- migrate

# Add a person and print their one-time setup link (roles comma-separated:
# steward, board, farm_lead, crew, volunteer). Runs against DATABASE_URL.
user-add email name roles:
    cargo run -q -p homestead-server --bin homestead -- user-add "{{email}}" "{{name}}" "{{roles}}"

# A fresh setup link for someone who lost their password.
setup-link email:
    cargo run -q -p homestead-server --bin homestead -- setup-link "{{email}}"

discord-register:
    cargo run -p homestead-server --bin homestead -- discord-register

# Show what would change on the Discord server (read-only).
discord-plan:
    cargo run -q -p homestead-server --bin homestead -- discord-provision

# Make the Discord server match discord/server.toml.
discord-provision:
    cargo run -q -p homestead-server --bin homestead -- discord-provision --apply

# Show what would change in Linear (read-only).
linear-plan:
    cargo run -q -p homestead-server --bin homestead -- linear-provision

# Make Linear match linear/workspace.toml.
linear-provision:
    cargo run -q -p homestead-server --bin homestead -- linear-provision --apply

# Check the live Zeffy campaign against zeffy/campaign.toml.
zeffy-verify:
    cargo run -q -p homestead-server --bin homestead -- zeffy-verify

# Render the copy-ready Zeffy build sheet from zeffy/campaign.toml.
zeffy-kit:
    python3 zeffy/render_kit.py

# Check the Buffer account against buffer/account.toml.
buffer-verify:
    cargo run -q -p homestead-server --bin homestead -- buffer-verify

# Show what would change in the GitHub org (read-only).
github-plan:
    cargo run -q -p homestead-server --bin homestead -- github-provision

# Make the GitHub org match github/org.toml.
github-provision:
    cargo run -q -p homestead-server --bin homestead -- github-provision --apply

# Render the legal releases (legal/*.typ → PDF).
legal:
    ./legal/build.sh

# Check the release PDFs, the counsel gate and the Documenso instance.
documenso-verify:
    cargo run -q -p homestead-server --bin homestead -- documenso-verify

# Pull edits made in the live systems since CI last recorded their state
# (refs/homestead/live). Independent edits merge; same-field edits fail.
systems-sync *args:
    git fetch --quiet origin '+refs/homestead/*:refs/homestead/*'
    cargo run -q -p homestead-server --bin homestead -- systems-sync --write {{args}}

# Joining end to end: /onboard, a real local Documenso (production's pinned
# image), a browser that declines then signs, the webhook, and every stored
# result checked (see the script header).
onboarding-e2e *args:
    scripts/onboarding-e2e.sh {{args}}

backup destination="backups":
    ./scripts/backup.sh {{destination}}

restore-test dump:
    ./scripts/restore-test.sh {{dump}}

# Create the R2 attachment buckets and set their lock (retention) rules.
r2-setup:
    ./scripts/r2-setup.sh

# Copy attachment bytes to the backup bucket, checked against the database
# (flags: --verify, --prune, --restore). Needs PROD_DATABASE_URL and both
# buckets' keys in .env; nightly, .github/workflows/backup.yml runs it.
attachments-backup *flags:
    #!/usr/bin/env sh
    set -eu
    val() { grep "^$1=" .env | head -1 | cut -d= -f2-; }
    DATABASE_URL=$(val PROD_DATABASE_URL) \
    ATTACHMENTS_ACCESS_KEY_ID=$(val ATTACHMENTS_READ_ACCESS_KEY_ID) \
    ATTACHMENTS_SECRET_ACCESS_KEY=$(val ATTACHMENTS_READ_SECRET_ACCESS_KEY) \
    ATTACHMENTS_BACKUP_ACCESS_KEY_ID=$(val ATTACHMENTS_BACKUP_ACCESS_KEY_ID) \
    ATTACHMENTS_BACKUP_SECRET_ACCESS_KEY=$(val ATTACHMENTS_BACKUP_SECRET_ACCESS_KEY) \
      cargo run -q -p homestead-server --bin homestead -- attachments-backup {{flags}}

# Give the nightly backup job its secrets from .env (values never printed).
backup-secrets:
    #!/usr/bin/env sh
    set -eu
    val() { grep "^$1=" .env | head -1 | cut -d= -f2-; }
    for name in PROD_DATABASE_URL ATTACHMENTS_READ_ACCESS_KEY_ID ATTACHMENTS_READ_SECRET_ACCESS_KEY ATTACHMENTS_BACKUP_ACCESS_KEY_ID ATTACHMENTS_BACKUP_SECRET_ACCESS_KEY; do
      v=$(val "$name")
      if [ -z "$v" ]; then echo "skipped $name: not set in .env"; continue; fi
      printf '%s' "$v" | gh secret set "$name" -R Sunbrewed-Homestead/homestead && echo "set $name"
    done

# Load the hosted core's secrets from .env into Cloudflare (values never
# printed). PROD_DATABASE_URL and PROD_ADMIN_TOKEN are the production ones;
# the integration keys are shared with local runs.
cloudflare-secrets:
    #!/usr/bin/env sh
    set -eu
    val() { grep "^$1=" .env | head -1 | cut -d= -f2-; }
    cd deploy/cloudflare
    for pair in DATABASE_URL:PROD_DATABASE_URL ADMIN_TOKEN:PROD_ADMIN_TOKEN DISCORD_WEBHOOK_URL:DISCORD_WEBHOOK_URL DISCORD_BOT_TOKEN:DISCORD_BOT_TOKEN DISCORD_GUILD_ID:DISCORD_GUILD_ID LINEAR_API_KEY:LINEAR_API_KEY BUFFER_API_KEY:BUFFER_API_KEY DOCUMENSO_API_TOKEN:PROD_DOCUMENSO_API_TOKEN DOCUMENSO_WEBHOOK_SECRET:PROD_DOCUMENSO_WEBHOOK_SECRET RESEND_API_KEY:RESEND_API_KEY ATTACHMENTS_ACCESS_KEY_ID:ATTACHMENTS_ACCESS_KEY_ID ATTACHMENTS_SECRET_ACCESS_KEY:ATTACHMENTS_SECRET_ACCESS_KEY; do
      name=${pair%%:*}; v=$(cd ../.. && val "${pair##*:}")
      if [ -z "$v" ]; then echo "skipped $name: ${pair##*:} is not set in .env"; continue; fi
      printf '%s' "$v" | npx wrangler secret put "$name" >/dev/null && echo "set $name"
    done

# Build the image (Docker Desktop must be running) and deploy to Cloudflare.
cloudflare-deploy:
    # A deploy must not publish stale integration data. Pull first, then stop
    # if it changed a spec: review and commit that single sync before deploying.
    git fetch --quiet origin '+refs/homestead/*:refs/homestead/*'
    cargo run -q -p homestead-server --bin homestead -- systems-sync --write
    git diff --quiet -- discord/server.toml linear/workspace.toml github/org.toml zeffy/campaign.toml buffer/account.toml || { echo "live integration changes were written to TOML; review and commit them, then deploy again" >&2; exit 1; }
    cd deploy/cloudflare && npm ci && npx wrangler deploy

# Load Documenso-on-Cloudflare's secrets from .env, deploy/documenso/.env and
# cert.p12 (values never printed). See deploy/documenso/cloudflare/README.md.
documenso-cloudflare-secrets:
    #!/usr/bin/env sh
    set -eu
    val() { grep "^$1=" "$2" | head -1 | cut -d= -f2-; }
    db=$(val PROD_DATABASE_URL .env)
    [ -n "$db" ] || { echo "PROD_DATABASE_URL is not set in .env" >&2; exit 1; }
    # Documenso's migrations hard-code the public schema, so it gets its own
    # database on the same server (not exposed by Supabase's Data API).
    db=$(printf '%s' "$db" | sed -E 's#/postgres([?]|$)#/documenso\1#')
    case "$db" in */documenso|*/documenso\?*) ;; *) echo "could not point the URL at the documenso database" >&2; exit 1;; esac
    pass=$(val NEXT_PRIVATE_SIGNING_PASSPHRASE deploy/documenso/.env)
    cert=$(base64 < deploy/documenso/cert.p12 | tr -d '\n')
    put() { [ -n "$2" ] || { echo "skipped $1: no value" >&2; return; }; printf '%s' "$2" | (cd deploy/documenso/cloudflare && npx wrangler secret put "$1" >/dev/null) && echo "set $1"; }
    put NEXT_PRIVATE_DATABASE_URL "$db"
    put NEXT_PRIVATE_DIRECT_DATABASE_URL "$db"
    put NEXTAUTH_SECRET "$(val PROD_DOCUMENSO_NEXTAUTH_SECRET .env)"
    put NEXT_PRIVATE_ENCRYPTION_KEY "$(val PROD_DOCUMENSO_ENCRYPTION_KEY .env)"
    put NEXT_PRIVATE_ENCRYPTION_SECONDARY_KEY "$(val PROD_DOCUMENSO_ENCRYPTION_SECONDARY_KEY .env)"
    put NEXT_PRIVATE_SIGNING_PASSPHRASE "$pass"
    put NEXT_PRIVATE_SIGNING_LOCAL_FILE_CONTENTS "$cert"

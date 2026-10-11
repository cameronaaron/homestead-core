#!/bin/sh
# End-to-end test of joining: the core (this checkout, in a container) gets
# the /join form and a signed `/onboard`, sends the welcome agreement through a real local
# Documenso, a real browser declines it and then signs a fresh one, and
# Documenso's own webhook settles each in the core. Every stored result is
# checked; the run exits non-zero on the first that's wrong.
#
#   scripts/onboarding-e2e.sh           build everything fresh and run
#   scripts/onboarding-e2e.sh status    what the core has recorded
#
# Needs: the sunbrewed-pg container running (`just db`), node, and Google
# Chrome (Playwright drives the system Chrome). Discord isn't contacted
# for real: the core trusts a throwaway interaction key made here, and its
# bot token is fake, so the role grant is expected to fail with 401 (the
# sink's own tests cover a successful grant against a mock).
set -eu
cd "$(dirname "$0")/.."
E="$PWD/target/e2e"
DB=homestead_e2e
q() { container exec sunbrewed-pg psql -U postgres -d "$DB" -Atc "$1"; }
ip() { container ls | awk -v n="$1" '$1 == n { sub(/\/.*/, "", $6); print $6 }'; }
fail() { echo "FAIL: $*" >&2; exit 1; }
expect() { [ "$2" = "$3" ] || fail "$1: expected [$3], got [$2]"; echo "ok   $1"; }

if [ "${1:-}" = status ]; then
  container exec sunbrewed-pg psql -U postgres -d "$DB" \
    -c "select release_key, status, discord_user_id, completed_at from homestead.release_requests order by sent_at" \
    -c "select * from homestead.onboardings" -c "select id, kind from homestead.events order by id"
  exit 0
fi

# One run per machine: every run uses the same container names and
# databases, so two at once (two worktrees, two agents) kill each other's
# containers and fail at random steps.
lock="$HOME/.cache/homestead-e2e.lock"
if ! mkdir "$lock" 2>/dev/null; then
  other=$(cat "$lock/pid" 2>/dev/null || true)
  if [ -n "$other" ] && kill -0 "$other" 2>/dev/null; then
    fail "another onboarding e2e is running (pid $other); wait for it to finish"
  fi
  echo "$$" > "$lock/pid" # a crashed run's lock
else
  echo "$$" > "$lock/pid"
fi
trap 'rm -rf "$lock"' EXIT

pg=$(ip sunbrewed-pg)
[ -n "$pg" ] || fail "start the sunbrewed-pg container first (just db)"
mkdir -p "$E" && chmod 700 "$E"
umask 077

# Playwright, outside the repo so node_modules never lands in it.
pw="$HOME/.cache/homestead-e2e"
[ -d "$pw/node_modules/playwright" ] || (mkdir -p "$pw" && cd "$pw" && npm init -y >/dev/null && npm install --silent playwright@1.55.0)
cp scripts/onboarding-e2e.mjs "$pw/"
step() { (cd "$pw" && E2E="$E" node onboarding-e2e.mjs "$@"); }

# Fresh databases and secrets for every run. A failed run leaves its
# containers connected, which blocked the drop (silently, until the error
# stopped being discarded), so remove them first and force it.
container rm -f sbh-core sbh-documenso >/dev/null 2>&1 || true
for d in documenso_e2e homestead_e2e; do
  container exec sunbrewed-pg psql -U postgres -qc "drop database if exists $d with (force)" -c "create database $d" >/dev/null \
    || fail "couldn't recreate the $d database"
done
openssl rand -base64 18 | tr -d '/+=' | sed 's/$/Aa1!/' > "$E/steward.pw"
openssl rand -hex 32 > "$E/admin-token"
openssl rand -hex 24 > "$E/webhook-secret"
node -e '
const c = require("crypto"), fs = require("fs"), E = process.argv[1];
const { publicKey, privateKey } = c.generateKeyPairSync("ed25519");
fs.writeFileSync(E + "/discord-key.pem", privateKey.export({ format: "pem", type: "pkcs8" }));
fs.writeFileSync(E + "/discord-key.pub", publicKey.export({ format: "der", type: "spki" }).subarray(-32).toString("hex"));
' "$E"

# Documenso: the exact image production runs (its Dockerfile's pin), from
# the local deploy's secrets, on its own database. Its public URL is
# localhost:3000 so signing links open in the host browser.
image=$(sed -nE 's/^FROM (docker\.io\/documenso\/documenso[^ ]*).*/\1/p' deploy/documenso/cloudflare/Dockerfile)
[ -n "$image" ] || fail "no Documenso pin in deploy/documenso/cloudflare/Dockerfile"
container image pull --platform linux/amd64 "$image" >/dev/null
pgpw=$(sed -nE 's#.*postgres://postgres:([^@]+)@localhost.*#\1#p' .cargo/config.toml)
sed -E -e "s#^(NEXT_PRIVATE(_DIRECT)?_DATABASE_URL)=.*#\1=postgresql://postgres:$pgpw@$pg:5432/documenso_e2e#" \
  -e 's#^NEXT_PUBLIC_WEBAPP_URL=.*#NEXT_PUBLIC_WEBAPP_URL=http://localhost:3000#' \
  -e 's#^NEXT_PRIVATE_INTERNAL_WEBAPP_URL=.*#NEXT_PRIVATE_INTERNAL_WEBAPP_URL=http://localhost:3000#' \
  -e 's#^NEXT_PRIVATE_SIGNING_LOCAL_FILE_PATH=.*#NEXT_PRIVATE_SIGNING_LOCAL_FILE_PATH=/opt/documenso/cert.p12#' \
  deploy/documenso/.env > "$E/documenso.env"
container rm -f sbh-documenso >/dev/null 2>&1 || true
container run -d --name sbh-documenso --platform linux/amd64 -m 3g -c 4 --dns 1.1.1.1 \
  --env-file "$E/documenso.env" -e DOCUMENSO_DISABLE_TELEMETRY=true -e NEXT_PRIVATE_SIGNING_TRANSPORT=local \
  -p 127.0.0.1:3000:3000 -v "$PWD/deploy/documenso/cert.p12:/opt/documenso/cert.p12:ro" \
  "$image" >/dev/null
for _ in $(seq 60); do curl -fs -o /dev/null http://localhost:3000/signin && break; sleep 3; done
step signup
container exec sunbrewed-pg psql -U postgres -d documenso_e2e -qc \
  "update \"User\" set \"emailVerified\" = now() where email = 'steward@e2e.test'"
step token

# The core: this checkout, built for Linux (containers can't reach the
# host; the Mac's firewall blocks them), Linear and Buffer off, Discord
# trusting the throwaway key.
container run --rm --dns 1.1.1.1 -m 8g -c 8 -v "$PWD":/src -w /src \
  -e CARGO_TARGET_DIR=/src/target/linux -e CARGO_HOME=/src/target/linux-cargo \
  rust:1-slim-trixie cargo build -q --release -p homestead-server --bin homestead
python3 - "$E" "$(ip sbh-documenso)" "$(cat "$E/discord-key.pub")" <<'EOF'
import re, sys
E, documenso, key = sys.argv[1:]
s = open("homestead.toml").read()
def section(name, change):
    global s
    m = re.search(r"(?ms)^\[" + re.escape(name) + r"\]\n.*?(?=^\[|\Z)", s)
    s = s[: m.start()] + change(m.group(0)) + s[m.end():]
# public_url is where Documenso sends a /join signer back: the same
# hostname the webhook uses, which the e2e browser maps to the core.
section("server", lambda t: re.sub(r'public_url = ".*"', 'public_url = "http://homestead-core:8088"',
    t.replace('bind = "127.0.0.1:8080"', 'bind = "0.0.0.0:8088"').replace("secure_cookies = true", "secure_cookies = false")))
section("discord", lambda t: re.sub(r"feed_channels = \[[^\]]*\]", "feed_channels = []",
    re.sub(r"deadline_channels = \{[^}]*\}", "deadline_channels = {}", re.sub(r'public_key = "\w+"', f'public_key = "{key}"', t))))
section("linear", lambda t: t.replace("enabled = true", "enabled = false", 1))
section("buffer", lambda t: t.replace("enabled = true", "enabled = false", 1))
section("documenso", lambda t: re.sub(r'base_url = ".*"', f'base_url = "http://{documenso}:3000"', t))
open(E + "/homestead.toml", "w").write(s)
EOF
printf 'DATABASE_URL=postgres://postgres:%s@%s:5432/%s\nADMIN_TOKEN=%s\nDOCUMENSO_API_TOKEN=%s\nDOCUMENSO_WEBHOOK_SECRET=%s\nDISCORD_WEBHOOK_URL=http://127.0.0.1:9/hook\nDISCORD_BOT_TOKEN=e2e-not-a-real-token\nDISCORD_CLIENT_SECRET=e2e-not-a-real-secret\nDISCORD_GUILD_ID=1\nHOMESTEAD_CONFIG=target/e2e/homestead.toml\n' \
  "$pgpw" "$pg" "$DB" "$(cat "$E/admin-token")" "$(cat "$E/documenso-token")" "$(cat "$E/webhook-secret")" > "$E/core.env"
container rm -f sbh-core >/dev/null 2>&1 || true
container run -d --name sbh-core --dns 1.1.1.1 --env-file "$E/core.env" -v "$PWD":/app -w /app \
  rust:1-slim-trixie /app/target/linux/release/homestead serve >/dev/null
core=$(ip sbh-core)
for _ in $(seq 30); do curl -fs -o /dev/null "http://$core:8088/healthz" && break; sleep 1; done
# Documenso's webhook form refuses IPs, so it targets the hostname
# homestead-core; pin it (the file is root-owned, and sed -i can't write
# a temp file in /etc).
container exec -u 0 sbh-documenso sh -c \
  "grep -v homestead-core /etc/hosts > /tmp/hosts; cat /tmp/hosts > /etc/hosts; echo '$core homestead-core' >> /etc/hosts"
step webhook

onboard() { (cd "$pw" && E2E="$E" CORE="$core" USER_ID=123456789012345678 ROLE="$1" node onboarding-e2e.mjs onboard); }
wait_for() { for _ in $(seq 60); do [ -n "$(q "$1")" ] && return 0; sleep 1; done; fail "timed out waiting for: $1"; }
link() { q "select signing_url from homestead.release_requests where status = 'sent' and signing_url is not null"; }

# 1. /onboard answers at once (deferred, only to the asker) and sends the agreement.
expect "/onboard defers, ephemeral" "$(onboard Volunteer)" '{"data":{"flags":64},"type":5}'
wait_for "select 1 from homestead.release_requests where status = 'sent' and signing_url is not null"
expect "the welcome agreement was sent" "$(q "select release_key from homestead.release_requests")" onboarding

# 2. Declining settles it through Documenso's webhook, and joins nobody.
(cd "$pw" && E2E="$E" URL="$(link)" node onboarding-e2e.mjs reject)
wait_for "select 1 from homestead.release_requests where status = 'declined'"
expect "decline clears the link" "$(q "select count(*) from homestead.release_requests where signing_url is not null")" 0
expect "decline onboards nobody" "$(q "select count(*) from homestead.onboardings where completed_at is not null")" 0

# 3. Asking again sends a fresh one; signing it onboards them.
expect "/onboard again defers" "$(onboard Volunteer)" '{"data":{"flags":64},"type":5}'
wait_for "select 1 from homestead.release_requests where status = 'sent' and signing_url is not null"
(cd "$pw" && E2E="$E" URL="$(link)" node onboarding-e2e.mjs sign)
wait_for "select 1 from homestead.release_requests where status = 'signed'"
expect "signed request, link cleared" \
  "$(q "select status, signing_url is null, discord_user_id from homestead.release_requests where status = 'signed'")" \
  "signed|t|123456789012345678"
expect "onboarding complete, as Volunteer" \
  "$(q "select role, completed_at is not null from homestead.onboardings")" "Volunteer|t"
expect "events" "$(q "select string_agg(kind, ',' order by id) from homestead.events")" \
  "release.declined,release.signed,member.onboarded"
expect "no email in the outbox" "$(q "select count(*) from homestead.events where payload::text like '%@%'")" 0
wait_for "select 1 from homestead.deliveries where sink = 'discord-roles' and attempts > 0"
echo "ok   the role sink tried to grant it ($(q "select left(last_error, 60) from homestead.deliveries where sink = 'discord-roles'"))"

# 4. Signed members change role without signing again.
case "$(onboard Community)" in
  *"already signed"*Community*) echo "ok   a second role needs no second signature" ;;
  *) fail "a signed member asking for Community wasn't told they're set" ;;
esac
expect "role now Community" "$(q "select role from homestead.onboardings")" Community

# 5. The Instagram link: the form sends the agreement, signing it in the
#    browser returns them to their join page, and it's recorded signed.
#    Discord OAuth itself is covered by the HTTP contract test; this local
#    run stops at the real authorization link because its credentials are fake.
page=$(cd "$pw" && E2E="$E" CORE="$core" node onboarding-e2e.mjs join)
case "$page" in
  *"Signed, thank you"*) echo "ok   Documenso sent the signer back to their join page, signed" ;;
  *) fail "the join page after signing said: $page" ;;
esac
expect "the join's agreement is signed" \
  "$(q "select r.status, r.discord_user_id is null from homestead.joins j join homestead.release_requests r on r.id = j.release_request_id")" \
  "signed|t"
expect "no Discord identity before OAuth" "$(q "select count(*) from homestead.joins where discord_user_id is not null")" 0

# 6. Documenso stored a sealed PDF with what was typed in the browser.
envelope=$(q "select envelope_id from homestead.release_requests where status = 'signed' and discord_user_id is not null")
token=$(cat "$E/documenso-token")
item=$(curl -fs -H "Authorization: $token" "http://localhost:3000/api/v2/envelope/$envelope" |
  python3 -c 'import json, sys; d = json.load(sys.stdin); assert d["status"] == "COMPLETED", d["status"]; print(d["envelopeItems"][0]["id"])')
curl -fs -H "Authorization: $token" "http://localhost:3000/api/v2/envelope/item/$item/download?version=signed" -o "$E/signed.pdf"
pdfsig "$E/signed.pdf" 2>/dev/null | grep -q "Signature is Valid" || fail "the signed PDF's digital signature doesn't verify"
echo "ok   the signed PDF's digital signature verifies"
# The whole document: the signing block is on its last page.
text=$(pdftotext "$E/signed.pdf" -)
for want in "Welcome Agreement" "Sam Tester" "818-555-0142" "Riley Tester" "riley@e2e.test"; do
  case "$text" in *"$want"*) ;; *) fail "the signed PDF is missing \"$want\"" ;; esac
done
echo "ok   the signed PDF carries the name, email and emergency contact"
# Each release box is initialled; Documenso fills them from the name.
n=$(printf '%s\n' "$text" | grep -cx "RT" || true)
want=$(grep -c '"INITIALS"' legal/onboarding.fields.json)
expect "every release is initialled" "$n" "$want"
echo "PASS against $image (signed copy: $E/signed.pdf)"

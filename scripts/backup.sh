#!/bin/sh
set -eu

: "${DATABASE_URL:?DATABASE_URL is required}"
# shellcheck source=scripts/pg.sh
. "$(dirname "$0")/pg.sh"
destination=${1:-backups}
mkdir -p "$destination"
stamp=$(date -u +%Y%m%dT%H%M%SZ)
dump="$destination/homestead-$stamp.dump"

umask 077
# A failed dump must not leave a partial file that looks like a backup.
ok=
trap 'test -n "$ok" || rm -f "$dump" "$dump.sha256"' EXIT
# Only our schemas: `homestead`, and `public` for sqlx's migration history.
# Supabase adds its own (vault, extensions, auth, …) whose extensions exist
# only on Supabase, so a whole-database dump won't restore anywhere else
# (`extension "supabase_vault" is not available`).
pg_tool pg_dump --format=custom --compress=9 --no-owner --no-acl \
  --schema=homestead --schema=public > "$dump"
sha256=$(shasum -a 256 "$dump" | awk '{print $1}')
printf '%s  %s\n' "$sha256" "$(basename "$dump")" > "$dump.sha256"
ok=1
printf '%s\n' "$dump"

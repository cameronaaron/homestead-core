#!/bin/sh
set -eu

: "${DATABASE_URL:?DATABASE_URL is required so the successful drill can be recorded}"
# shellcheck source=scripts/pg.sh
. "$(dirname "$0")/pg.sh"
dump=${1:?usage: scripts/restore-test.sh BACKUP.dump}
test -f "$dump"
name="homestead-restore-$$"
cleanup() { cli_remove "$name" >/dev/null 2>&1 || true; }
trap cleanup EXIT INT TERM

expected=$(awk '{print $1}' "$dump.sha256")
actual=$(shasum -a 256 "$dump" | awk '{print $1}')
test "$expected" = "$actual"

"$cli" run -d --name "$name" -e POSTGRES_PASSWORD=restore-test -e POSTGRES_DB=homestead "$PG_IMAGE" >/dev/null
# Over TCP: the image's init-time server listens only on its socket, so a
# socket check can pass before the real server has started.
i=0
until "$cli" exec "$name" pg_isready -h 127.0.0.1 -U postgres -d homestead >/dev/null 2>&1; do
  i=$((i + 1)); test "$i" -lt 60 || { echo "restore Postgres did not become ready" >&2; exit 1; }
  sleep 1
done
"$cli" cp "$dump" "$name:/tmp/backup.dump"
# Every database already has a `public` schema, so skip the dump's CREATE.
"$cli" exec "$name" sh -c "pg_restore -l /tmp/backup.dump |
  grep -v -E '^[0-9]+; [0-9]+ [0-9]+ (SCHEMA|COMMENT) - public ' > /tmp/toc"
"$cli" exec "$name" pg_restore --exit-on-error --no-owner --no-acl -L /tmp/toc \
  -h 127.0.0.1 -U postgres -d homestead /tmp/backup.dump

# The restore must hold what the source holds: every homestead table and
# every applied migration (both change only on deploy), and the ledgers
# must be readable. Row counts of live tables aren't compared: writes
# after the dump would make them differ.
shape="select count(*) from information_schema.tables
         where table_schema = 'homestead' and table_type = 'BASE TABLE';
       select count(*) from public._sqlx_migrations;"
checks=$(printf '%s\n' "$shape
  select count(*) from homestead.events;
  select count(*) from homestead.audit_entries;" |
  "$cli" exec -i "$name" psql -v ON_ERROR_STOP=1 -h 127.0.0.1 -AtU postgres -d homestead)
restored=$(printf '%s\n' "$checks" | head -2 | tr '\n' ' ')
source=$(printf '%s\n' "$shape" | pg_tool psql -v ON_ERROR_STOP=1 -At | tr '\n' ' ')
if [ "$restored" != "$source" ]; then
  echo "restore differs from the source: tables, migrations restored $restored; source $source" >&2
  exit 1
fi
case $restored in 0\ *) echo "restore has no homestead tables" >&2; exit 1 ;; esac

version=$("$cli" exec "$name" psql -h 127.0.0.1 -AtU postgres -d homestead -c 'show server_version')
pg_tool psql -v ON_ERROR_STOP=1 \
  --set=backup_name="$(basename "$dump")" --set=backup_sha256="$actual" --set=postgres_version="$version" <<'SQL'
insert into homestead.restore_tests (backup_name, backup_sha256, postgres_version, integrity_ok)
values (:'backup_name', :'backup_sha256', :'postgres_version', true);
SQL
echo "restore verified: $(basename "$dump") ($actual) on PostgreSQL $version; tables, migrations: $restored"

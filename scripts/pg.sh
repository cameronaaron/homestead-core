# Sourced by backup.sh and restore-test.sh: runs PostgreSQL 17 client tools
# against $DATABASE_URL, and names the container CLI for throwaway servers.
#
# pg_dump refuses a server newer than itself, and GitHub's runners ship
# PostgreSQL 16 tools, so a tool on PATH is used only if it is 17 or newer.
# Otherwise the tool runs in the same image the restore drill uses:
#   - docker (CI): host networking reaches both remote and localhost URLs.
#   - Apple `container` (this Mac): a localhost URL is the `just db`
#     container, which a new container can't reach (the firewall blocks
#     192.168.64.1), so the tool runs inside it; anything else runs in a
#     fresh container, which needs explicit DNS.

PG_IMAGE=docker.io/library/postgres:17-alpine

if [ -n "${CONTAINER_CLI:-}" ]; then
  cli=$CONTAINER_CLI
elif command -v container >/dev/null 2>&1; then
  cli=container
else
  cli=docker
fi

# Removes a container, whichever CLI made it.
cli_remove() {
  case $cli in
    docker) docker rm -f "$1" ;;
    *) "$cli" delete --force "$1" ;;
  esac
}

# pg_tool TOOL ARGS...: TOOL (pg_dump, psql, …) connected to $DATABASE_URL.
# Standard input and output pass through.
pg_tool() {
  tool=$1
  shift
  if command -v "$tool" >/dev/null 2>&1 &&
    [ "$("$tool" --version | sed -E 's/.* ([0-9]+)(\.[0-9]+)*.*/\1/')" -ge 17 ]; then
    "$tool" -d "$DATABASE_URL" "$@"
  elif [ "$cli" = docker ]; then
    docker run --rm -i --network host -e DATABASE_URL "$PG_IMAGE" \
      sh -c 'tool=$1; shift; exec "$tool" -d "$DATABASE_URL" "$@"' - "$tool" "$@"
  else
    case $DATABASE_URL in
      *@localhost[:/]* | *@127.0.0.1[:/]*)
        "$cli" exec -i "${PG_DUMP_CONTAINER:-sunbrewed-pg}" "$tool" -U postgres -d homestead "$@" ;;
      *)
        "$cli" run --rm -i --dns 1.1.1.1 -e DATABASE_URL "$PG_IMAGE" \
          sh -c 'tool=$1; shift; exec "$tool" -d "$DATABASE_URL" "$@"' - "$tool" "$@" ;;
    esac
  fi
}

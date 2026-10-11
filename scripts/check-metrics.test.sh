#!/bin/sh
# Each case feeds scripts/check-metrics.sh a metrics page and states whether
# it must pass or fail, and with what message.
set -u
here=$(dirname "$0")
failures=0

healthy='homestead_bus_events{state="dead"} 0
homestead_http_requests_total{class="5xx"} 0
homestead_edge_server_errors_24h 0
homestead_last_restore_test_age_seconds 86400'

# expect NAME (pass|fail) [MESSAGE] METRICS
expect() {
  name=$1 want=$2 message=$3 input=$4
  out=$(printf '%s\n' "$input" | "$here/check-metrics.sh"); status=$?
  if [ "$want" = pass ] && [ "$status" -ne 0 ]; then echo "not ok: $name should pass: $out"; failures=$((failures + 1))
  elif [ "$want" = fail ] && [ "$status" -eq 0 ]; then echo "not ok: $name should fail"; failures=$((failures + 1))
  elif [ "$want" = fail ] && ! printf '%s' "$out" | grep -q "$message"; then echo "not ok: $name: no \"$message\" in: $out"; failures=$((failures + 1))
  else echo "ok: $name"; fi
}

expect "healthy" pass "" "$healthy"
expect "dead letters" fail "dead-lettered" "$(printf '%s\n' "$healthy" | sed 's/state="dead"} 0/state="dead"} 2/')"
expect "core 5xx" fail "since the core last started" "$(printf '%s\n' "$healthy" | sed 's/class="5xx"} 0/class="5xx"} 1/')"
expect "worker 5xx" fail "sent by the Worker" "$(printf '%s\n' "$healthy" | sed 's/errors_24h 0/errors_24h 3/')"
expect "never drilled" fail "no restore drill" "$(printf '%s\n' "$healthy" | sed 's/age_seconds 86400/age_seconds NaN/')"
expect "stale drill" fail "9 days old" "$(printf '%s\n' "$healthy" | sed 's/age_seconds 86400/age_seconds 800000/')"
expect "missing series" fail "is missing" "$(printf '%s\n' "$healthy" | grep -v edge_server)"
expect "empty page" fail "is missing" ""
[ "$failures" -eq 0 ]

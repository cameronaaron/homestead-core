#!/bin/sh
# Reads Prometheus text from stdin (GET /api/v1/metrics) and exits non-zero,
# naming each problem, if the deployment needs attention. The nightly Backup
# workflow runs it against production, so a failure emails the owner; that is
# the alert path while no Prometheus scrapes deploy/prometheus/alerts.yml.
# The limits mirror that file.
set -eu

awk '
function fail(msg) { print "FAIL: " msg; bad = 1 }
/^homestead_bus_events\{state="dead"\}/ { seen_dead = 1; if ($2 > 0) fail($2 " dead-lettered integration deliveries") }
/^homestead_http_requests_total\{class="5xx"\}/ { seen_5xx = 1; if ($2 > 0) fail($2 " server errors since the core last started") }
/^homestead_edge_server_errors_24h / { seen_edge = 1; if ($2 > 0) fail($2 " server errors sent by the Worker in the last 24 hours") }
/^homestead_last_restore_test_age_seconds / {
  seen_age = 1
  if ($2 == "NaN") fail("no restore drill has ever been recorded")
  else if ($2 > 8 * 24 * 3600) fail("the last restore drill is " int($2 / 86400) " days old (limit 8)")
}
END {
  if (!seen_dead) fail("homestead_bus_events{state=\"dead\"} is missing from the metrics")
  if (!seen_5xx) fail("homestead_http_requests_total{class=\"5xx\"} is missing from the metrics")
  if (!seen_edge) fail("homestead_edge_server_errors_24h is missing (is the Worker adding it?)")
  if (!seen_age) fail("homestead_last_restore_test_age_seconds is missing from the metrics")
  exit bad
}'

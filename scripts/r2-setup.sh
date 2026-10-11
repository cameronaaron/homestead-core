#!/usr/bin/env bash
# Creates the attachment buckets and sets their lock (retention) rules.
# Safe to rerun: existing buckets are kept, and `lock set` replaces each
# bucket's rules with exactly these.
#
#   homestead-attachments         private; att/ locked 30 days (matches
#                                 [attachments] retain_deleted_days)
#   homestead-attachments-backup  private; att/ locked 90 days; written
#                                 only by .github/workflows/backup.yml
#
# R2 has no object versioning. A lock refuses delete and overwrite of an
# object younger than its age, whoever asks (a stolen app key included);
# only an account admin can remove a rule.
#
# Runs wrangler from outside the iCloud-synced Desktop: iCloud evicts
# node_modules files there, and an evicted wrangler exits 0 silently.
set -euo pipefail

W="${WRANGLER_HOME_DIR:-$HOME/.cache/homestead-wrangler}"
mkdir -p "$W"
[ -x "$W/node_modules/.bin/wrangler" ] || npm --prefix "$W" install --silent wrangler@4.143.0
wrangler() { "$W/node_modules/.bin/wrangler" "$@"; }

lock() { # bucket rule-id days
  local file
  file="$(mktemp)"
  printf '{"rules":[{"id":"%s","enabled":true,"prefix":"att/","condition":{"type":"Age","maxAgeSeconds":%d}}]}\n' \
    "$2" "$(($3 * 86400))" >"$file"
  wrangler r2 bucket lock set "$1" --file "$file" --force
  rm -f "$file"
}

for b in homestead-attachments homestead-attachments-backup; do
  wrangler r2 bucket info "$b" >/dev/null 2>&1 || wrangler r2 bucket create "$b" --location wnam
  # Never public: no r2.dev URL (downloads are presigned by the app).
  wrangler r2 bucket dev-url disable "$b" --force >/dev/null 2>&1 || true
done
lock homestead-attachments att-30-days 30
lock homestead-attachments-backup att-90-days 90
for b in homestead-attachments homestead-attachments-backup; do
  wrangler r2 bucket lock list "$b"
done

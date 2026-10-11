#!/usr/bin/env bash
# Renders every release in documenso/releases.toml (its `source`) to its
# `pdf`, and writes the signing fields its layout drew (legal/release.typ,
# `<field>` metadata) to `<pdf minus .pdf>.fields.json`; the core sends
# exactly those fields to Documenso. The DRAFT watermark stays on until
# releases.toml says approved = true. Deterministic (bundled fonts, fixed
# timestamp) so CI can check the committed files match their sources.
# Fails if the signature isn't on the last page: text that overflows its
# page would push the boxes off the fields.
set -euo pipefail
cd "$(dirname "$0")/.."
spec=documenso/releases.toml
draft=true
grep -Eq '^approved *= *true' "$spec" && draft=false
pages=$(mktemp -d)
trap 'rm -rf "$pages"' EXIT
sources=$(sed -nE 's/^source *= *"(.*)"/\1/p' "$spec")
[ -n "$sources" ] || { echo "no releases in $spec" >&2; exit 1; }
for src in $sources; do
  pdf=$(awk -v s="$src" '$1=="pdf"{p=$3} $1=="source"{gsub(/"/,"",$3); if ($3==s) {gsub(/"/,"",p); print p}}' "$spec")
  fields="${pdf%.pdf}.fields.json"
  typst compile --ignore-system-fonts --creation-timestamp 0 --input "draft=$draft" "$src" "$pdf"
  typst eval --ignore-system-fonts --input "draft=$draft" --in "$src" --pretty \
    'query(<field>).map(it => it.value)' > "$fields" 2> >(grep -v -e '^$' >&2 || true)
  rm -f "$pages"/*
  typst compile --ignore-system-fonts --input "draft=$draft" --format png --ppi 10 "$src" "$pages/p{p}.png"
  n=$(find "$pages" -name '*.png' | wc -l | tr -d ' ')
  signed=$(grep -c '"kind": "SIGNATURE"' "$fields" || true)
  [ "$signed" = 1 ] || { echo "$src draws $signed signature boxes; it needs exactly one" >&2; exit 1; }
  last=$(awk '/"kind": "SIGNATURE"/{s=1} s && /"page":/{gsub(/[^0-9]/,""); print; exit}' "$fields")
  [ "$last" = "$n" ] || { echo "$src runs to $n pages but signs on page $last; shorten the text" >&2; exit 1; }
done
echo "rendered $(echo "$sources" | wc -l | tr -d ' ') legal PDFs (draft=$draft)"

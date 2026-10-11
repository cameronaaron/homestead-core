#!/usr/bin/env bash
# Creates the self-signed signing certificate Documenso seals documents with,
# and records its passphrase in .env. A password is mandatory: Documenso
# fails to sign with a passwordless .p12.
set -euo pipefail
cd "$(dirname "$0")"
[ -f cert.p12 ] && { echo "cert.p12 already exists; delete it first to replace it"; exit 1; }
pass=$(openssl rand -hex 24)
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
openssl req -x509 -newkey rsa:3072 -sha256 -days 3650 -nodes \
  -keyout "$tmp/key.pem" -out "$tmp/cert.pem" \
  -subj "/CN=Sun Brewed Homestead Inc. Document Signing/O=Sun Brewed Homestead Inc./L=Van Nuys/ST=California/C=US" 2>/dev/null
CERT_PASS="$pass" openssl pkcs12 -export -out cert.p12 -inkey "$tmp/key.pem" -in "$tmp/cert.pem" -password env:CERT_PASS
chmod 400 cert.p12
touch .env
if grep -q '^NEXT_PRIVATE_SIGNING_PASSPHRASE=' .env; then
  sed -i.bak "s|^NEXT_PRIVATE_SIGNING_PASSPHRASE=.*|NEXT_PRIVATE_SIGNING_PASSPHRASE=$pass|" .env && rm -f .env.bak
else
  echo "NEXT_PRIVATE_SIGNING_PASSPHRASE=$pass" >> .env
fi
echo "wrote cert.p12 and its passphrase to .env"

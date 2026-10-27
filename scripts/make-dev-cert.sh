#!/bin/bash
# Creates a self-signed code-signing certificate named "Capturra Dev" in your login keychain.
#
# Why: macOS ties Screen Recording / Accessibility / Input Monitoring permissions to an app's code
# signature. An unsigned or ad-hoc build gets a new signature every time it is rebuilt, so the
# permissions are lost on every build. Signing every build with the SAME certificate keeps them.
# This is for development on your own Mac only; it is not for distributing the app.
#
# It changes your keychain (adds a certificate and trusts it for code signing), so macOS will ask
# for your login password. Run it yourself:   ! scripts/make-dev-cert.sh
set -euo pipefail

NAME="${CAPTURRA_SIGN_ID:-Capturra Dev}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

if security find-identity -v -p codesigning | grep -q "\"$NAME\""; then
  echo "A valid code-signing identity named \"$NAME\" already exists. Nothing to do."
  exit 0
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
PASS="capturra-$$"

openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
  -keyout "$WORK/key.pem" -out "$WORK/cert.pem" \
  -subj "/CN=$NAME" \
  -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=critical,codeSigning" 2>/dev/null
openssl pkcs12 -export -inkey "$WORK/key.pem" -in "$WORK/cert.pem" -out "$WORK/cert.p12" -passout "pass:$PASS"

security import "$WORK/cert.p12" -k "$KEYCHAIN" -P "$PASS" -T /usr/bin/codesign
# Trust it for code signing only (asks for your password).
security add-trusted-cert -r trustRoot -p codeSign -k "$KEYCHAIN" "$WORK/cert.pem"

echo
if security find-identity -v -p codesigning | grep "\"$NAME\""; then
  echo "Done. Build and sign with:  scripts/build-app.sh"
else
  echo "The certificate was added but is not listed as valid yet. Open Keychain Access, find \"$NAME\","
  echo "open it, expand Trust, and set Code Signing to Always Trust." >&2
  exit 1
fi

#!/bin/bash
# Builds Capturra.app (debug) and signs it with a stable identity, so macOS keeps the permissions
# you grant it between builds. Create the identity once with scripts/make-dev-cert.sh.
#
#   scripts/build-app.sh            build + sign with "Capturra Dev"
#   CAPTURRA_SIGN_ID="Apple Development: you (TEAMID)" scripts/build-app.sh
#   CAPTURRA_SIGN_ID=- scripts/build-app.sh      ad-hoc signature (permissions will NOT persist)
#   CAPTURRA_SKIP_BUILD=1 scripts/build-app.sh   sign the app that is already built
set -euo pipefail

ID="${CAPTURRA_SIGN_ID:-Capturra Dev}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/target/debug/bundle/macos/Capturra.app"

if [ "$ID" != "-" ] && ! security find-identity -v -p codesigning | grep -q "\"$ID\""; then
  echo "No valid code-signing identity named \"$ID\" on this Mac." >&2
  echo "Create one with:  scripts/make-dev-cert.sh   (or set CAPTURRA_SIGN_ID to an identity you have)" >&2
  echo "Available identities:" >&2
  security find-identity -v -p codesigning >&2
  exit 1
fi

if [ -z "${CAPTURRA_SKIP_BUILD:-}" ]; then
  (cd "$ROOT/apps/capturra-app" && npm install --silent && npx tauri build --debug --bundles app)
fi
[ -d "$APP" ] || { echo "$APP does not exist; run without CAPTURRA_SKIP_BUILD." >&2; exit 1; }

codesign --force --deep --sign "$ID" "$APP"
codesign --verify --deep --strict "$APP"
echo
codesign -dv "$APP" 2>&1 | grep -E "Identifier|Authority|Signature"
echo
echo "Signed with: $ID"
echo "Run it with:    open \"$APP\""
echo "Then grant Screen Recording, Accessibility and Input Monitoring to \"Capturra\" (the popover has links),"
echo "and quit and reopen it once. The grants survive later builds signed with the same identity."

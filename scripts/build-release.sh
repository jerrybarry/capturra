#!/bin/bash
# Builds Capturra in release mode and produces a macOS .app and .dmg bundle.
#
# Usage:
#   ./scripts/build-release.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "==> Building frontend assets..."
(cd "$ROOT/apps/capturra-app" && npm run build)

echo "==> Building release bundles (app + dmg)..."
(cd "$ROOT/apps/capturra-app" && npx tauri build)

echo ""
echo "==> Done! Bundles generated at:"
ls -la "$ROOT/target/release/bundle/dmg/" || true
ls -la "$ROOT/target/release/bundle/macos/" || true

#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if command -v cargo >/dev/null 2>&1; then
  (cd "$ROOT/core" && cargo generate-lockfile && cargo metadata --locked --format-version 1 >/dev/null)
elif command -v docker >/dev/null 2>&1; then
  docker run --rm -v "$ROOT:/src" -w /src/core rust:1.91.0-bookworm sh -lc 'cargo generate-lockfile && cargo metadata --locked --format-version 1 >/dev/null'
else
  echo "Need cargo or Docker to generate core/Cargo.lock." >&2
  exit 2
fi
sha256sum "$ROOT/core/Cargo.lock" 2>/dev/null || shasum -a 256 "$ROOT/core/Cargo.lock"

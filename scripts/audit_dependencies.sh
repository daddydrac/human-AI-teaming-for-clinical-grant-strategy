#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"; cd "$ROOT"; missing=0
if command -v cargo-audit >/dev/null 2>&1; then (cd core && cargo audit); else echo "WARNING: cargo-audit unavailable; Rust vulnerability audit skipped." >&2; missing=1; fi
if command -v pip-audit >/dev/null 2>&1; then
  pip-audit -r ui/requirements.txt
  pip-audit -r renderer/requirements.txt
  pip-audit -r embedding_cpu/requirements.txt
else echo "WARNING: pip-audit unavailable; Python vulnerability audit skipped." >&2; missing=1; fi
if [[ "${REQUIRE_AUDIT_TOOLS:-false}" == "true" && "$missing" != 0 ]]; then echo "ERROR: dependency-audit tools are required." >&2; exit 8; fi
echo "Dependency audit stage completed."

#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
"$ROOT/scripts/configure_runtime.sh" "$ROOT/.runtime.env"
echo
echo "Generated $ROOT/.runtime.env. start.sh loads it automatically."

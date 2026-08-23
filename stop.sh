#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"
[[ -f .env ]] && { set -a; source .env; set +a; }
[[ -f .runtime.env ]] && { set -a; source .runtime.env; set +a; }
docker compose down

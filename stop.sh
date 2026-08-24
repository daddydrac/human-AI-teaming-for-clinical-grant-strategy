#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"
COMPOSE=(docker compose --project-directory "$ROOT" -f "$ROOT/docker-compose.yml")
[[ -f .env ]] && { set -a; source .env; set +a; }
[[ -f .runtime.env ]] && { set -a; source .runtime.env; set +a; }
"${COMPOSE[@]}" down

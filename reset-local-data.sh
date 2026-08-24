#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

ENV_FILE="$ROOT/.env"
[[ -f "$ENV_FILE" ]] || { echo "No .env file exists. Run ./install.sh first." >&2; exit 2; }

set -a
source "$ENV_FILE"
set +a

DATA_SETTING="${GRANT_DATA_HOME:-.grantspace-data}"
if [[ "$DATA_SETTING" = /* ]]; then
  DATA_DIR="$DATA_SETTING"
else
  DATA_DIR="$ROOT/${DATA_SETTING#./}"
fi

case "$DATA_DIR" in
  "$ROOT"|/|"")
    echo "Refusing to reset an unsafe data path: $DATA_DIR" >&2
    exit 3
    ;;
esac

COMPOSE=(docker compose --project-directory "$ROOT" -f "$ROOT/docker-compose.yml")
"${COMPOSE[@]}" down

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
RESET_BACKUP_ROOT="$ROOT/backups/local-resets"
RESET_BACKUP="$RESET_BACKUP_ROOT/$STAMP"
mkdir -p "$RESET_BACKUP_ROOT"

if [[ -e "$DATA_DIR" ]]; then
  mv "$DATA_DIR" "$RESET_BACKUP"
  echo "Previous accounts and grants were moved to: $RESET_BACKUP"
else
  echo "No prior application data existed at: $DATA_DIR"
fi
mkdir -p "$DATA_DIR"

TEMP_ENV="$(mktemp "$ROOT/.env.reset.XXXXXX")"
trap 'rm -f "$TEMP_ENV"' EXIT
awk '
  BEGIN { replaced=0 }
  /^INITIAL_ADMIN_SETUP_TOKEN=/ {
    if (!replaced) print "INITIAL_ADMIN_SETUP_TOKEN="
    replaced=1
    next
  }
  { print }
  END { if (!replaced) print "INITIAL_ADMIN_SETUP_TOKEN=" }
' "$ENV_FILE" > "$TEMP_ENV"
chmod 600 "$TEMP_ENV"
mv "$TEMP_ENV" "$ENV_FILE"
trap - EXIT

"$ROOT/scripts/ensure_admin_setup_token.sh" "$ENV_FILE"

echo
echo "Fresh local state is ready. Ollama models and Docker image caches were preserved."
echo "Start the application with: ./start.sh"
echo "Then open: http://localhost:7860/setup"

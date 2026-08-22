#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"
[[ -f .env ]] && { set -a; source .env; set +a; }
[[ -f .runtime.env ]] && { set -a; source .runtime.env; set +a; }
docker compose down
if [[ "${GRANT_RUNTIME_PROFILE:-}" == "apple_mlx" ]]; then
  PID_FILE="$HOME/Library/Application Support/GrantWriter/mlx-runtime/server.pid"
  if [[ -f "$PID_FILE" ]]; then
    PID="$(cat "$PID_FILE" 2>/dev/null || true)"
    [[ -n "$PID" ]] && kill "$PID" 2>/dev/null || true
    rm -f "$PID_FILE"
  fi
fi
if [[ "${GRANT_RUNTIME_PROFILE:-}" == "apple_ollama" ]]; then
  RUNTIME_DIR="$HOME/Library/Application Support/GrantWriter/ollama-runtime"
  PID_FILE="$RUNTIME_DIR/server.pid"
  STARTED_FILE="$RUNTIME_DIR/started-by-grant-writer"
  if [[ -f "$PID_FILE" && -f "$STARTED_FILE" ]]; then
    PID="$(cat "$PID_FILE" 2>/dev/null || true)"
    [[ -n "$PID" ]] && kill "$PID" 2>/dev/null || true
    rm -f "$PID_FILE" "$STARTED_FILE"
  fi
fi

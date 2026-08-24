#!/usr/bin/env bash
set -euo pipefail

RUNTIME_DIR="${GRANT_OLLAMA_RUNTIME_DIR:-$HOME/Library/Application Support/GrantWriter/ollama-runtime}"
PID_FILE="$RUNTIME_DIR/server.pid"
STARTED_FILE="$RUNTIME_DIR/started-by-grant-writer"

if [[ ! -f "$STARTED_FILE" || ! -f "$PID_FILE" ]]; then
  echo "Native Ollama is externally managed; leaving it running."
  exit 0
fi
PID="$(tr -d '[:space:]' < "$PID_FILE")"
if [[ "$PID" =~ ^[0-9]+$ ]] && kill -0 "$PID" 2>/dev/null; then
  COMMAND="$(ps -p "$PID" -o command= 2>/dev/null || true)"
  if [[ "$COMMAND" == *"ollama serve"* ]]; then
    kill "$PID"
    for _ in $(seq 1 20); do kill -0 "$PID" 2>/dev/null || break; sleep 0.25; done
    echo "Stopped the Grantspace-managed native Ollama process."
  else
    echo "Refusing to stop PID $PID because it is not an Ollama server." >&2
  fi
fi
rm -f "$PID_FILE" "$STARTED_FILE"

#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
  echo "The apple_ollama profile requires an Apple Silicon Mac." >&2
  exit 2
fi
if ! command -v ollama >/dev/null 2>&1; then
  echo "Ollama is required. Install it from https://ollama.com/download/mac and rerun ./start.sh." >&2
  exit 3
fi

MODEL="${LOCAL_LLM_API_MODEL:-${OLLAMA_MODEL:-qwen3:1.7b}}"
PORT="${OLLAMA_PORT:-11434}"
CONTEXT="${OLLAMA_CONTEXT_LENGTH:-4096}"
RUNTIME_DIR="${GRANT_OLLAMA_RUNTIME_DIR:-$HOME/Library/Application Support/GrantWriter/ollama-runtime}"
LOG_DIR="${GRANT_OLLAMA_LOG_DIR:-$HOME/Library/Logs/GrantWriter}"
PID_FILE="$RUNTIME_DIR/server.pid"
STARTED_FILE="$RUNTIME_DIR/started-by-grant-writer"
MODELS_URL="http://127.0.0.1:$PORT/v1/models"
export OLLAMA_HOST="127.0.0.1:$PORT"

valid_model='^[A-Za-z0-9][A-Za-z0-9_.:/-]*$'
[[ "$MODEL" =~ $valid_model ]] || { echo "Invalid Ollama model name: $MODEL" >&2; exit 4; }
[[ "$CONTEXT" =~ ^[0-9]+$ ]] || { echo "OLLAMA_CONTEXT_LENGTH must be an integer." >&2; exit 4; }

mkdir -p "$RUNTIME_DIR" "$LOG_DIR"
if ! curl -fsS "$MODELS_URL" >/dev/null 2>&1; then
  echo "Starting Ollama on 127.0.0.1:$PORT with a ${CONTEXT}-token context ceiling..."
  OLLAMA_CONTEXT_LENGTH="$CONTEXT" \
  OLLAMA_NUM_PARALLEL=1 \
  OLLAMA_MAX_LOADED_MODELS=1 \
  OLLAMA_FLASH_ATTENTION=1 \
  nohup ollama serve >"$LOG_DIR/ollama.log" 2>&1 &
  echo $! > "$PID_FILE"
  : > "$STARTED_FILE"
  for _ in $(seq 1 60); do
    curl -fsS "$MODELS_URL" >/dev/null 2>&1 && break
    sleep 1
  done
  curl -fsS "$MODELS_URL" >/dev/null || {
    echo "Ollama failed to become ready. See $LOG_DIR/ollama.log" >&2
    exit 5
  }
else
  echo "Using the Ollama service already running on 127.0.0.1:$PORT."
fi

if ! ollama show "$MODEL" >/dev/null 2>&1; then
  echo "Downloading $MODEL. This happens once and may take several minutes..."
  ollama pull "$MODEL"
fi

curl -fsS "$MODELS_URL" | python3 -c '
import json, sys
model=sys.argv[1]
available={item.get("id") for item in json.load(sys.stdin).get("data", [])}
if model not in available:
    raise SystemExit(f"Ollama is ready but does not report configured model {model!r}")
' "$MODEL"

echo "Ollama model ready: $MODEL"

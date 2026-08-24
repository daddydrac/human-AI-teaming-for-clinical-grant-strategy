#!/usr/bin/env bash
set -euo pipefail
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
  echo "The apple_ollama profile requires an Apple Silicon Mac." >&2
  exit 2
fi
if ! command -v ollama >/dev/null 2>&1; then
  "$ROOT/scripts/bootstrap_dependencies.sh"
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

echo "Verifying native inference and Apple GPU placement..."
OLLAMA_HOST="$OLLAMA_HOST" ollama run "$MODEL" "Reply with exactly: READY" >/dev/null
if ! ollama ps 2>/dev/null | awk -v model="$MODEL" '$1 == model {found=1; if ($0 ~ /GPU/) gpu=1} END {exit !(found && gpu)}'; then
  echo "ERROR: Ollama answered, but $MODEL is not reported on the Apple GPU. Refusing a silent CPU fallback." >&2
  echo "Run 'ollama ps' and inspect $LOG_DIR/ollama.log before retrying." >&2
  exit 6
fi

curl -fsS "$MODELS_URL" | python3 -c '
import json, sys
model=sys.argv[1]
available={item.get("id") for item in json.load(sys.stdin).get("data", [])}
if model not in available:
    raise SystemExit(f"Ollama is ready but does not report configured model {model!r}")
' "$MODEL"

echo "Ollama model ready: $MODEL"

#!/usr/bin/env bash
set -euo pipefail
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"
run_compose() {
  docker compose --project-directory "$ROOT" -f "$ROOT/docker-compose.yml" "$@"
}
if [[ ! -f .env ]]; then
  TEMPLATE=.env.example
  if [[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "arm64" ]]; then
    MEM_BYTES="$(sysctl -n hw.memsize 2>/dev/null || echo 0)"
    if [[ "$MEM_BYTES" -le 9663676416 && -f env.m2Mac.8gb.txt ]]; then
      TEMPLATE=env.m2Mac.8gb.txt
    elif [[ -f env.m4Mac.qwen3.txt ]]; then
      TEMPLATE=env.m4Mac.qwen3.txt
    fi
  elif [[ "$(uname -s)" == "Linux" && "$(uname -m)" == "x86_64" && -f env.linux.nvidia.txt ]]; then
    TEMPLATE=env.linux.nvidia.txt
  fi
  cp "$TEMPLATE" .env
  echo "Created .env from $TEMPLATE."
fi
./scripts/ensure_admin_setup_token.sh .env
set -a
source .env
set +a
mkdir -p "${GRANT_DATA_HOME:-$ROOT/.grantspace-data}" "${GRANT_EXPORT_HOME:-./exports}"

./scripts/bootstrap_dependencies.sh

./scripts/configure_runtime.sh .runtime.env
set -a
source .runtime.env
set +a

./scripts/preflight.sh

if [[ ! -f core/Cargo.lock ]]; then
  echo "WARNING: core/Cargo.lock is absent. Development builds can continue, but a reproducible release requires ./scripts/freeze_rust_dependencies.sh." >&2
fi

# The Python UI is bind-mounted for immediate local updates, while the Rust API
# is a compiled image. Rebuild the API automatically whenever its build inputs
# change so a current UI can never call routes from a stale core binary.
hash_stream() {
  if command -v shasum >/dev/null 2>&1; then shasum -a 256
  else sha256sum
  fi
}
core_source_hash() {
  {
    find core/src hpc -type f \( -name '*.rs' -o -name '*.c' -o -name '*.h' \) -print 2>/dev/null
    for candidate in core/Cargo.toml core/Cargo.lock core/build.rs Dockerfile.core; do
      [[ -f "$candidate" ]] && printf '%s\n' "$candidate"
    done
  } | LC_ALL=C sort | while IFS= read -r source_file; do
    hash_stream < "$source_file"
  done | hash_stream | awk '{print $1}'
}
BUILD_STATE_DIR="$ROOT/.grantspace-build"
CORE_HASH_FILE="$BUILD_STATE_DIR/core-source.sha256"
mkdir -p "$BUILD_STATE_DIR"
CURRENT_CORE_HASH="$(core_source_hash)"
SAVED_CORE_HASH="$(test -f "$CORE_HASH_FILE" && tr -d '[:space:]' < "$CORE_HASH_FILE" || true)"
CORE_SOURCE_CHANGED=0
if [[ "$CURRENT_CORE_HASH" != "$SAVED_CORE_HASH" ]]; then CORE_SOURCE_CHANGED=1; fi

if [[ "$MODEL_ROUTING_MODE" == "claude_only" || "$MODEL_ROUTING_MODE" == "hybrid" ]]; then
  if [[ -z "${ANTHROPIC_API_KEY:-}" ]]; then
    echo "ERROR: MODEL_ROUTING_MODE=$MODEL_ROUTING_MODE requires ANTHROPIC_API_KEY in .env." >&2
    exit 4
  fi
fi
if [[ "$GRANT_RUNTIME_PROFILE" == "apple_ollama" && "$MODEL_ROUTING_MODE" != "claude_only" ]]; then
  # A container from the retired Mac profile may still own port 11434. Stop it
  # without deleting its volume before starting the Metal-enabled host runtime.
  run_compose --profile local-model stop ollama >/dev/null 2>&1 || true
  ./scripts/start_ollama.sh
elif [[ "$GRANT_RUNTIME_PROFILE" == "linux_nvidia_ollama" && "$MODEL_ROUTING_MODE" != "claude_only" ]]; then
  run_compose --profile local-model up -d ollama
  for _ in $(seq 1 90); do run_compose exec -T ollama ollama list >/dev/null 2>&1 && break; sleep 2; done
  run_compose exec -T ollama ollama list >/dev/null 2>&1 || { echo "Containerized Ollama failed to become ready." >&2; exit 5; }
  MODEL="${LOCAL_LLM_API_MODEL:-${OLLAMA_MODEL:-qwen3:1.7b}}"
  if ! run_compose exec -T ollama ollama show "$MODEL" >/dev/null 2>&1; then
    echo "Downloading local model $MODEL into the Docker model volume..."
    run_compose exec -T ollama ollama pull "$MODEL"
  fi
  echo "Verifying NVIDIA model inference and GPU placement..."
  run_compose exec -T ollama ollama run "$MODEL" "Reply with exactly: READY" >/dev/null
  run_compose exec -T ollama ollama ps | awk -v model="$MODEL" '$1 == model {found=1; if ($0 ~ /GPU/) gpu=1} END {exit !(found && gpu)}' || {
    echo "ERROR: Ollama answered, but $MODEL is not reported on an NVIDIA GPU. Refusing a silent CPU fallback." >&2
    exit 5
  }
elif [[ "$MODEL_ROUTING_MODE" == "claude_only" ]]; then
  run_compose --profile local-model stop ollama >/dev/null 2>&1 || true
  if [[ "$GRANT_RUNTIME_PROFILE" == "apple_ollama" ]]; then ./scripts/stop_ollama.sh; fi
fi

if [[ "${REBUILD:-0}" == "1" ]]; then
  run_compose up -d --build
  printf '%s\n' "$CURRENT_CORE_HASH" > "$CORE_HASH_FILE"
else
  if [[ "$CORE_SOURCE_CHANGED" == "1" ]]; then
    echo "Rust API source changed; rebuilding the core image before startup..."
    run_compose build core
    printf '%s\n' "$CURRENT_CORE_HASH" > "$CORE_HASH_FILE"
  fi
  run_compose up -d
fi
# Refresh the two configuration-consuming entry services sequentially so
# credentials and routing changed in .env cannot remain stale. --no-deps avoids
# Docker Desktop races while Ollama or another large dependency is already up.
run_compose up -d --no-deps --force-recreate core
run_compose up -d --no-deps --force-recreate ui

for _ in $(seq 1 60); do
  curl -fsS http://127.0.0.1:7860 >/dev/null 2>&1 && break
  sleep 2
done
curl -fsS http://127.0.0.1:7860 >/dev/null || { run_compose ps; exit 6; }
DISPLAY_URL="${APP_PUBLIC_URL:-http://localhost:7860}"
echo "Grant Writer is ready: $DISPLAY_URL"
[[ "$(uname -s)" == "Darwin" ]] && command -v open >/dev/null 2>&1 && open "$DISPLAY_URL" || true

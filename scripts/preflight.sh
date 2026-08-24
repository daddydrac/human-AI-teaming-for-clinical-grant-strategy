#!/usr/bin/env bash
set -euo pipefail
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
COMPOSE=(docker compose --project-directory "$ROOT" -f "$ROOT/docker-compose.yml")
[[ -f "$ROOT/.runtime.env" ]] || "$ROOT/scripts/configure_runtime.sh" "$ROOT/.runtime.env" >/dev/null
set -a
[[ -f "$ROOT/.env" ]] && source "$ROOT/.env"
source "$ROOT/.runtime.env"
set +a

command -v docker >/dev/null 2>&1 || { echo "Docker Desktop/CLI is required." >&2; exit 2; }
docker info >/dev/null 2>&1 || { echo "Docker daemon is not running." >&2; exit 3; }
if [[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "x86_64" ]]; then
  HV="$(sysctl -n kern.hv_support 2>/dev/null || echo 0)"
  [[ "$HV" == "1" ]] || { echo "This Intel Mac does not expose Apple's Hypervisor Framework required by Docker Desktop." >&2; exit 4; }
fi
case "${MODEL_ROUTING_MODE:-}" in
  local_only|hybrid|claude_only) ;;
  *) echo "MODEL_ROUTING_MODE must be local_only, hybrid, or claude_only." >&2; exit 5 ;;
esac
if [[ "$MODEL_ROUTING_MODE" == "hybrid" || "$MODEL_ROUTING_MODE" == "claude_only" ]]; then
  [[ -n "${ANTHROPIC_API_KEY:-}" ]] || { echo "ANTHROPIC_API_KEY is required in $MODEL_ROUTING_MODE mode." >&2; exit 5; }
fi
if [[ "$GRANT_RUNTIME_PROFILE" == "apple_ollama" ]]; then
  [[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "arm64" ]] || { echo "apple_ollama requires Apple Silicon macOS." >&2; exit 6; }
  if [[ "$MODEL_ROUTING_MODE" != "claude_only" ]]; then
    command -v ollama >/dev/null 2>&1 || { echo "Native Ollama is missing; rerun ./install.sh." >&2; exit 6; }
    [[ "$LOCAL_LLM_URL" == http://host.docker.internal:* ]] || { echo "Apple containers must reach native Ollama through host.docker.internal." >&2; exit 6; }
  fi
elif [[ "$GRANT_RUNTIME_PROFILE" == "linux_nvidia_ollama" ]]; then
  [[ "$(uname -s)" == "Linux" && "$(uname -m)" == "x86_64" ]] || { echo "linux_nvidia_ollama requires Linux x86_64." >&2; exit 6; }
  if [[ "$MODEL_ROUTING_MODE" != "claude_only" ]]; then
    command -v nvidia-smi >/dev/null 2>&1 || { echo "The NVIDIA driver is not visible." >&2; exit 6; }
    nvidia-smi >/dev/null 2>&1 || { echo "The NVIDIA driver cannot communicate with the GPU." >&2; exit 6; }
    docker info --format '{{json .Runtimes}}' | grep -q 'nvidia' || { echo "Docker's NVIDIA runtime is not configured." >&2; exit 6; }
  fi
  if ! COMPOSE_SERVICES="$("${COMPOSE[@]}" --profile local-model config --services)"; then
    echo "The containerized Ollama service configuration could not be loaded." >&2
    exit 6
  fi
  OLLAMA_SERVICE_FOUND=false
  while IFS= read -r service; do
    if [[ "$service" == "ollama" ]]; then
      OLLAMA_SERVICE_FOUND=true
      break
    fi
  done <<< "$COMPOSE_SERVICES"
  [[ "$OLLAMA_SERVICE_FOUND" == "true" ]] || { echo "The containerized Ollama service is not defined in docker-compose.yml." >&2; exit 6; }
elif [[ "$GRANT_RUNTIME_PROFILE" == "docker_cpu" ]]; then
  [[ "$MODEL_ROUTING_MODE" == "claude_only" ]] || { echo "docker_cpu supports claude_only routing." >&2; exit 6; }
else
  echo "Unsupported runtime profile: $GRANT_RUNTIME_PROFILE" >&2
  exit 6
fi
if [[ -z "${OPENALEX_API_KEY:-}" ]]; then
  echo "WARNING: OPENALEX_API_KEY is not configured; Phase 6 publication discovery will be skipped." >&2
fi
if [[ -z "${BRAVE_SEARCH_API_KEY:-}" ]]; then
  echo "WARNING: BRAVE_SEARCH_API_KEY is not configured; online evidence research and Phase 6 patent/IP + technology web enrichment will be skipped." >&2
fi

EXPORT_DIR="${GRANT_EXPORT_HOME:-$ROOT/exports}"
mkdir -p "$EXPORT_DIR"
[[ -w "$EXPORT_DIR" ]] || { echo "Export directory is not writable: $EXPORT_DIR" >&2; exit 7; }
echo "Preflight passed: profile=$GRANT_RUNTIME_PROFILE exports=$EXPORT_DIR"

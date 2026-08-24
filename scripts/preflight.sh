#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
COMPOSE=(docker compose --project-directory "$ROOT" -f "$ROOT/docker-compose.yml")
[[ -f "$ROOT/.runtime.env" ]] || "$ROOT/scripts/configure_runtime.sh" "$ROOT/.runtime.env" >/dev/null
set -a
[[ -f "$ROOT/.env" ]] && source "$ROOT/.env"
source "$ROOT/.runtime.env"
set +a

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "WARNING: production target is macOS; continuing validation on $(uname -s)." >&2
fi
command -v docker >/dev/null 2>&1 || { echo "Docker Desktop/CLI is required." >&2; exit 2; }
docker info >/dev/null 2>&1 || { echo "Docker daemon is not running." >&2; exit 3; }
if [[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "x86_64" ]]; then
  HV="$(sysctl -n kern.hv_support 2>/dev/null || echo 0)"
  [[ "$HV" == "1" ]] || { echo "This Intel Mac does not expose Apple's Hypervisor Framework required by Docker Desktop." >&2; exit 4; }
fi
if [[ "$GRANT_RUNTIME_PROFILE" == "docker_cpu" ]]; then
  [[ -n "${ANTHROPIC_API_KEY:-}" ]] || { echo "ANTHROPIC_API_KEY is required in docker_cpu mode." >&2; exit 5; }
elif [[ "$GRANT_RUNTIME_PROFILE" == "container_ollama" ]]; then
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
fi
if [[ "${MODEL_ROUTING_MODE:-}" == "hybrid" && "${REQUIRE_CLAUDE_IN_HYBRID:-false}" == "true" && -z "${ANTHROPIC_API_KEY:-}" ]]; then
  echo "ANTHROPIC_API_KEY is required because this hybrid profile requires Claude escalation." >&2
  exit 8
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

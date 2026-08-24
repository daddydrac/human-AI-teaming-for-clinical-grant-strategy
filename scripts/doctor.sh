#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"; cd "$ROOT"
COMPOSE=(docker compose --project-directory "$ROOT" -f "$ROOT/docker-compose.yml")
echo "Grant Writer doctor"
echo "  OS: $(uname -s) $(uname -m)"
echo "  Docker CLI: $(command -v docker >/dev/null && echo yes || echo no)"
if command -v docker >/dev/null 2>&1; then
  docker info >/dev/null 2>&1 && DOCKER_DAEMON=ready || DOCKER_DAEMON=unavailable
else
  DOCKER_DAEMON=unavailable
fi
echo "  Docker daemon: $DOCKER_DAEMON"
echo "  Cargo lock: $([[ -f core/Cargo.lock ]] && echo present || echo absent)"
[[ -f .runtime.env ]] || ./scripts/configure_runtime.sh .runtime.env >/dev/null
set -a; [[ -f .env ]] && source .env; source .runtime.env; set +a
echo "  Runtime profile: ${GRANT_RUNTIME_PROFILE:-unknown}"
echo "  Model routing: ${MODEL_ROUTING_MODE:-unknown}"
echo "  Local provider/model: ${LOCAL_LLM_PROVIDER:-legacy}/${LOCAL_LLM_API_MODEL:-${OLMO_API_MODEL:-unknown}}"
echo "  OMP/Rayon/BLAS: ${OMP_NUM_THREADS:-?}/${RAYON_NUM_THREADS:-?}/${OPENBLAS_NUM_THREADS:-?}"
echo "  Competitive refresh: ${COMPETITIVE_REFRESH_TTL_SECONDS:-14400}s"
echo "  Exports: ${GRANT_EXPORT_HOME:-./exports}"
if docker info >/dev/null 2>&1; then
  "${COMPOSE[@]}" ps || true
  if [[ "${GRANT_RUNTIME_PROFILE:-}" == "linux_nvidia_ollama" && "${MODEL_ROUTING_MODE:-}" != "claude_only" ]]; then
    "${COMPOSE[@]}" exec -T ollama ollama list 2>/dev/null || echo "  Containerized Ollama: unavailable"
  fi
  curl -fsS http://127.0.0.1:8080/health/ready 2>/dev/null || true
fi
if [[ "${GRANT_RUNTIME_PROFILE:-}" == "apple_ollama" && "${MODEL_ROUTING_MODE:-}" != "claude_only" ]]; then
  ollama list 2>/dev/null || echo "  Native Ollama: unavailable"
  ollama ps 2>/dev/null || true
fi

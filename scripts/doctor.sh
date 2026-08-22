#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"; cd "$ROOT"
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
if [[ "${GRANT_RUNTIME_PROFILE:-}" == "apple_ollama" ]]; then
  curl -fsS "http://127.0.0.1:${OLLAMA_PORT:-11434}/api/version" 2>/dev/null || echo "  Ollama: unavailable"
fi
if docker info >/dev/null 2>&1; then docker compose ps || true; curl -fsS http://127.0.0.1:8080/health/ready 2>/dev/null || true; fi

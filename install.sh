#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"
if [[ "$(uname -s)" != "Darwin" ]]; then echo "ERROR: Grant Writer production installation targets macOS." >&2; exit 2; fi
if [[ ! -f .env ]]; then
  TEMPLATE=.env.example
  if [[ "$(uname -m)" == "arm64" ]]; then
    MEM_BYTES="$(sysctl -n hw.memsize 2>/dev/null || echo 0)"
    if [[ "$MEM_BYTES" -le 9663676416 && -f env.m2Mac.8gb.txt ]]; then
      TEMPLATE=env.m2Mac.8gb.txt
    elif [[ -f env.m4Mac.txt ]]; then
      TEMPLATE=env.m4Mac.txt
    fi
  fi
  cp "$TEMPLATE" .env
  echo "Created .env from $TEMPLATE; review its routing and credential settings before startup."
fi
set -a; source .env; set +a
mkdir -p "${GRANT_EXPORT_HOME:-$ROOT/exports}" "${BACKUP_DIR:-$ROOT/backups}" "${BENCHMARK_OUTPUT_DIR:-$ROOT/benchmarks}" "${RELEASE_DIR:-$ROOT/releases}"
FREE_KB="$(df -Pk "$ROOT" | awk 'NR==2{print $4}')"
if [[ "${FREE_KB:-0}" -lt 10485760 ]]; then echo "WARNING: less than 10 GB free disk space is available; model/container installation may fail." >&2; fi
./scripts/configure_runtime.sh .runtime.env
set -a; source .runtime.env; set +a
command -v docker >/dev/null 2>&1 || { echo "ERROR: Docker Desktop is required." >&2; exit 3; }
docker info >/dev/null 2>&1 || { echo "ERROR: Docker Desktop is installed but its daemon is not running." >&2; exit 4; }
if [[ "$GRANT_RUNTIME_PROFILE" == "apple_mlx" ]]; then command -v uv >/dev/null 2>&1 || { echo "ERROR: Apple-Silicon MLX mode requires uv." >&2; exit 5; }; fi
if [[ "$GRANT_RUNTIME_PROFILE" == "apple_ollama" ]]; then command -v ollama >/dev/null 2>&1 || { echo "ERROR: The 8 GB Apple-Silicon profile requires Ollama. Install it from https://ollama.com/download/mac." >&2; exit 6; }; fi
./scripts/validate.sh
echo
printf 'Installation/bootstrap complete.\nRuntime profile: %s\n' "$GRANT_RUNTIME_PROFILE"
if [[ "${MODEL_ROUTING_MODE:-}" == "local_only" ]]; then
  echo "Next: review .env, then run ./start.sh (no cloud model credential is required)."
else
  echo "Next: edit .env with required provider credentials, then run ./start.sh"
fi

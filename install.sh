#!/usr/bin/env bash
set -euo pipefail
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"
COMPOSE=(docker compose --project-directory "$ROOT" -f "$ROOT/docker-compose.yml")
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
  echo "Created .env from $TEMPLATE; review its routing and credential settings before startup."
fi
./scripts/ensure_admin_setup_token.sh .env
set -a; source .env; set +a
mkdir -p "${GRANT_DATA_HOME:-$ROOT/.grantspace-data}" "${GRANT_EXPORT_HOME:-$ROOT/exports}" "${BACKUP_DIR:-$ROOT/backups}" "${BENCHMARK_OUTPUT_DIR:-$ROOT/benchmarks}" "${RELEASE_DIR:-$ROOT/releases}"
FREE_KB="$(df -Pk "$ROOT" | awk 'NR==2{print $4}')"
if [[ "${FREE_KB:-0}" -lt 10485760 ]]; then echo "WARNING: less than 10 GB free disk space is available; model/container installation may fail." >&2; fi
./scripts/bootstrap_dependencies.sh
./scripts/configure_runtime.sh .runtime.env
set -a; source .runtime.env; set +a
command -v docker >/dev/null 2>&1 || { echo "ERROR: automatic Docker installation did not provide the Docker CLI." >&2; exit 3; }
docker info >/dev/null 2>&1 || { echo "ERROR: Docker is installed but its daemon is not available to this account." >&2; exit 4; }
if [[ "${RUN_FULL_VALIDATION:-false}" == "true" ]]; then
  echo "Running the full build, test, and release validation suite..."
  ./scripts/validate.sh
else
  python3 -m py_compile ui/app.py renderer/app.py embedding_cpu/app.py ingestion/app.py
  "${COMPOSE[@]}" config >/dev/null
  echo "Fast installation checks passed. Full validation was skipped; set RUN_FULL_VALIDATION=true to run it."
fi
echo
printf 'Installation/bootstrap complete.\nRuntime profile: %s\n' "$GRANT_RUNTIME_PROFILE"
if [[ "${START_AFTER_INSTALL:-true}" == "true" ]]; then
  echo "Starting the model runtime and application containers..."
  exec "$ROOT/start.sh"
fi
echo "Automatic startup was disabled with START_AFTER_INSTALL=false. Run ./start.sh when ready."

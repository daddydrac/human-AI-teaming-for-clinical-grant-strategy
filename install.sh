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
./scripts/bootstrap_dependencies.sh
./scripts/configure_runtime.sh .runtime.env
set -a; source .runtime.env; set +a
command -v docker >/dev/null 2>&1 || { echo "ERROR: automatic Docker Desktop installation did not provide the Docker CLI." >&2; exit 3; }
docker info >/dev/null 2>&1 || { echo "ERROR: Docker Desktop is installed but its daemon is not running." >&2; exit 4; }
./scripts/validate.sh
echo
printf 'Installation/bootstrap complete.\nRuntime profile: %s\n' "$GRANT_RUNTIME_PROFILE"
if [[ "${START_AFTER_INSTALL:-true}" == "true" ]]; then
  echo "Starting the model runtime and application containers..."
  exec "$ROOT/start.sh"
fi
echo "Automatic startup was disabled with START_AFTER_INSTALL=false. Run ./start.sh when ready."

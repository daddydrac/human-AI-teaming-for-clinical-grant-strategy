#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"
if [[ ! -f .env ]]; then
  TEMPLATE=.env.example
  if [[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "arm64" && -f env.m4Mac.txt ]]; then TEMPLATE=env.m4Mac.txt; fi
  cp "$TEMPLATE" .env
  echo "Created .env from $TEMPLATE. Review credentials and rerun ./start.sh."
  exit 2
fi
set -a
source .env
set +a
mkdir -p "${GRANT_EXPORT_HOME:-./exports}"

./scripts/configure_runtime.sh .runtime.env
set -a
source .runtime.env
set +a

./scripts/preflight.sh

if [[ ! -f core/Cargo.lock ]]; then
  echo "WARNING: core/Cargo.lock is absent. Development builds can continue, but a reproducible release requires ./scripts/freeze_rust_dependencies.sh." >&2
fi

if [[ "$GRANT_RUNTIME_PROFILE" == "docker_cpu" ]]; then
  if [[ -z "${ANTHROPIC_API_KEY:-}" ]]; then
    echo "ERROR: low-memory Docker mode requires ANTHROPIC_API_KEY for fast generation." >&2
    echo "Set ANTHROPIC_API_KEY in .env. Local OLMo 7B is intentionally disabled on this hardware profile." >&2
    exit 4
  fi
else
  LOG_DIR="$HOME/Library/Logs/GrantWriter"
  RUNTIME_DIR="$HOME/Library/Application Support/GrantWriter/mlx-runtime"
  mkdir -p "$LOG_DIR" "$RUNTIME_DIR"
  if ! curl -fsS "http://127.0.0.1:${OLMO_PORT:-8000}/v1/models" >/dev/null 2>&1; then
    echo "Starting native Apple MLX service..."
    nohup "$ROOT/scripts/start_mlx.sh" >"$LOG_DIR/mlx.log" 2>&1 &
    echo $! > "$RUNTIME_DIR/server.pid"
    for _ in $(seq 1 90); do
      curl -fsS "http://127.0.0.1:${OLMO_PORT:-8000}/v1/models" >/dev/null 2>&1 && break
      sleep 2
    done
    curl -fsS "http://127.0.0.1:${OLMO_PORT:-8000}/v1/models" >/dev/null || {
      echo "Native MLX failed to become ready. See $LOG_DIR/mlx.log" >&2; exit 5;
    }
  fi
fi

if [[ "${REBUILD:-0}" == "1" ]]; then
  docker compose up -d --build
else
  docker compose up -d
fi

for _ in $(seq 1 60); do
  curl -fsS http://127.0.0.1:7860 >/dev/null 2>&1 && break
  sleep 2
done
curl -fsS http://127.0.0.1:7860 >/dev/null || { docker compose ps; exit 6; }
echo "Grant Writer is ready: http://localhost:7860"
command -v open >/dev/null 2>&1 && open http://localhost:7860 || true

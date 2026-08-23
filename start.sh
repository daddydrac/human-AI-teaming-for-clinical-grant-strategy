#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"
if [[ ! -f .env ]]; then
  TEMPLATE=.env.example
  if [[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "arm64" ]]; then
    MEM_BYTES="$(sysctl -n hw.memsize 2>/dev/null || echo 0)"
    if [[ "$MEM_BYTES" -le 9663676416 && -f env.m2Mac.8gb.txt ]]; then
      TEMPLATE=env.m2Mac.8gb.txt
    elif [[ -f env.m4Mac.txt ]]; then
      TEMPLATE=env.m4Mac.txt
    fi
  fi
  cp "$TEMPLATE" .env
  echo "Created .env from $TEMPLATE."
fi
set -a
source .env
set +a
if [[ "${AUTH_MODE:-internal_accounts}" == "internal_accounts" && -z "${INITIAL_ADMIN_SETUP_TOKEN:-}" ]]; then
  command -v openssl >/dev/null 2>&1 || { echo "ERROR: openssl is required to generate the one-time administrator setup token." >&2; exit 3; }
  INITIAL_ADMIN_SETUP_TOKEN="$(openssl rand -hex 32)"
  export INITIAL_ADMIN_SETUP_TOKEN
  printf '\nINITIAL_ADMIN_SETUP_TOKEN=%s\n' "$INITIAL_ADMIN_SETUP_TOKEN" >> .env
  echo "Generated the one-time initial administrator setup token and saved it in .env."
  echo "Enter this token on the first-start setup screen: $INITIAL_ADMIN_SETUP_TOKEN"
fi
mkdir -p "${GRANT_EXPORT_HOME:-./exports}"

./scripts/bootstrap_dependencies.sh

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
elif [[ "$GRANT_RUNTIME_PROFILE" == "container_ollama" ]]; then
  docker compose --profile local-model up -d ollama
  for _ in $(seq 1 90); do docker compose exec -T ollama ollama list >/dev/null 2>&1 && break; sleep 2; done
  docker compose exec -T ollama ollama list >/dev/null 2>&1 || { echo "Containerized Ollama failed to become ready." >&2; exit 5; }
  MODEL="${LOCAL_LLM_API_MODEL:-${OLLAMA_MODEL:-qwen3:1.7b}}"
  if ! docker compose exec -T ollama ollama show "$MODEL" >/dev/null 2>&1; then
    echo "Downloading local model $MODEL into the Docker model volume..."
    docker compose exec -T ollama ollama pull "$MODEL"
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

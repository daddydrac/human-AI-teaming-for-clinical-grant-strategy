#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

./scripts/preflight_oidc_gateway.sh

if [[ "${REBUILD:-0}" == "1" ]]; then
  docker compose -f docker-compose.yml -f docker-compose.oidc.yml up -d --build
else
  docker compose -f docker-compose.yml -f docker-compose.oidc.yml up -d
fi

set -a
source .env
set +a
for _ in $(seq 1 90); do
  curl --fail --silent --show-error --cacert "$OIDC_TLS_CERT_FILE" \
    "https://${OIDC_PUBLIC_HOST}:${OIDC_HTTPS_PORT:-8443}/gateway-health" >/dev/null 2>&1 && break
  sleep 2
done
curl --fail --silent --show-error --cacert "$OIDC_TLS_CERT_FILE" \
  "https://${OIDC_PUBLIC_HOST}:${OIDC_HTTPS_PORT:-8443}/gateway-health" >/dev/null || {
    docker compose -f docker-compose.yml -f docker-compose.oidc.yml ps
    exit 6
  }
echo "Grantspace OIDC gateway is ready: https://${OIDC_PUBLIC_HOST}:${OIDC_HTTPS_PORT:-8443}"

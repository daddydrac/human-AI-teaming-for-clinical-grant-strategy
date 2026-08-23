#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"; cd "$ROOT"
./scripts/validate.sh
[[ -f core/Cargo.lock ]] || ./scripts/freeze_rust_dependencies.sh
./scripts/audit_dependencies.sh
./scripts/preflight.sh
# validate.sh already runs the locked Rust suite in the pinned BLAS/OpenMP
# container. Re-running it on macOS would incorrectly require host cblas headers.
docker compose config >/dev/null
docker compose --profile cpu-embedding config >/dev/null
docker compose build
docker compose --profile cpu-embedding build
# Acceptance uses the authenticated single-user profile so it can exercise the
# complete HTTP authorization/idempotency stack without production SMTP or an
# institutional OIDC tenant. Internal-account and trusted-header flows have
# separate contract tests and deployment checks.
AUTH_MODE=local_single_user MODEL_ROUTING_MODE=local_only docker compose up -d
cleanup(){ [[ "${KEEP_RUNNING_AFTER_ACCEPTANCE:-false}" == "true" ]] || ./stop.sh >/dev/null 2>&1 || true; }
trap cleanup EXIT
for _ in $(seq 1 60); do curl -fsS http://127.0.0.1:8080/health >/dev/null 2>&1 && break; sleep 2; done
curl -fsS http://127.0.0.1:8080/health >/dev/null
./scripts/smoke_test.sh
./scripts/benchmark.sh
./scripts/backup.sh
mkdir -p "${RELEASE_DIR:-$ROOT/releases}"
python3 scripts/generate_sbom.py "${RELEASE_DIR:-$ROOT/releases}/acceptance-sbom.cdx.json" >/dev/null
python3 scripts/generate_release_manifest.py "${RELEASE_DIR:-$ROOT/releases}/acceptance-manifest.json" >/dev/null
echo "Phase 8 release acceptance passed."

#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"; cd "$ROOT"
./scripts/validate.sh
[[ -f core/Cargo.lock ]] || ./scripts/freeze_rust_dependencies.sh
./scripts/audit_dependencies.sh
./scripts/preflight.sh
(cd core && cargo test --release --locked)
docker compose config >/dev/null
docker compose --profile cpu-embedding config >/dev/null
docker compose build
docker compose --profile cpu-embedding build
REBUILD=0 ./start.sh
cleanup(){ [[ "${KEEP_RUNNING_AFTER_ACCEPTANCE:-false}" == "true" ]] || ./stop.sh >/dev/null 2>&1 || true; }
trap cleanup EXIT
./scripts/smoke_test.sh
./scripts/benchmark.sh
./scripts/backup.sh
mkdir -p "${RELEASE_DIR:-$ROOT/releases}"
python3 scripts/generate_sbom.py "${RELEASE_DIR:-$ROOT/releases}/acceptance-sbom.cdx.json" >/dev/null
python3 scripts/generate_release_manifest.py "${RELEASE_DIR:-$ROOT/releases}/acceptance-manifest.json" >/dev/null
echo "Phase 8 release acceptance passed."

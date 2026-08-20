#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"; cd "$ROOT"
VERSION="$(python3 - <<'PYVER'
import tomllib
print(tomllib.load(open('core/Cargo.toml','rb'))['package']['version'])
PYVER
)"
OUTDIR="${RELEASE_DIR:-$ROOT/releases}"; mkdir -p "$OUTDIR"
./scripts/validate.sh
[[ -f core/Cargo.lock ]] || { echo "ERROR: core/Cargo.lock is required for a reproducible release. Run ./scripts/freeze_rust_dependencies.sh first." >&2; exit 4; }
python3 scripts/generate_sbom.py "$OUTDIR/mdanderson-grant-agent-$VERSION.sbom.cdx.json" >/dev/null
python3 scripts/generate_release_manifest.py "$OUTDIR/mdanderson-grant-agent-$VERSION.manifest.json" >/dev/null
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/mdanderson-grant-agent"
rsync -a --delete --exclude '.git' --exclude '.env' --exclude '.runtime.env' --exclude '__pycache__' --exclude '*.pyc' \
  --exclude 'target' --exclude '.venv' --exclude '.pytest_cache' --exclude 'exports' --exclude 'backups' --exclude 'benchmarks' --exclude 'releases' --exclude 'release-sbom.cdx.json' --exclude 'release-manifest.json' ./ "$TMP/mdanderson-grant-agent/"
cp "$OUTDIR/mdanderson-grant-agent-$VERSION.sbom.cdx.json" "$TMP/mdanderson-grant-agent/RELEASE_SBOM.cdx.json"
cp "$OUTDIR/mdanderson-grant-agent-$VERSION.manifest.json" "$TMP/mdanderson-grant-agent/RELEASE_MANIFEST.json"
"$ROOT/scripts/security_scan.sh" "$TMP/mdanderson-grant-agent"
ART="$OUTDIR/mdanderson-grant-agent-$VERSION.zip"; rm -f "$ART"
(cd "$TMP" && zip -qr "$ART" mdanderson-grant-agent)
"$ROOT/scripts/sign_release.sh" "$ART"
unzip -tq "$ART" >/dev/null
echo "Release artifact: $ART"

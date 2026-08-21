#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
OUTDIR="${BACKUP_DIR:-$ROOT/backups}"
mkdir -p "$OUTDIR"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="${1:-$OUTDIR/grant-data-$TS.tar.gz}"
TMP="$OUT.tmp"
WAS_RUNNING=0
rm -f "$TMP"
command -v docker >/dev/null 2>&1 || { echo "ERROR: Docker is required." >&2; exit 2; }
docker compose ps core --status running --quiet 2>/dev/null | grep -q . && WAS_RUNNING=1 || true
if [[ "$WAS_RUNNING" == 1 ]]; then
  docker compose stop ui renderer core >/dev/null
fi
restart() {
  if [[ "$WAS_RUNNING" == 1 ]]; then docker compose up -d core renderer ui >/dev/null || true; fi
}
trap 'rm -f "$TMP"; restart' EXIT
# With writers stopped, SQLite DB/WAL, MMAP, Parquet, and project files are captured
# at the same filesystem point in time. Embedding model cache is intentionally excluded.
docker compose run --rm -T --no-deps --entrypoint /bin/sh core -lc 'tar -C /workspace -czf - .' > "$TMP"
[[ -s "$TMP" ]] || { echo "ERROR: backup archive is empty." >&2; exit 3; }
mv "$TMP" "$OUT"
SHA="$(shasum -a 256 "$OUT" | awk '{print $1}')"
printf '%s  %s\n' "$SHA" "$(basename "$OUT")" > "$OUT.sha256"
python3 - "$OUT.manifest.json" "$OUT" "$SHA" <<'PYBACKUP'
import datetime,json,pathlib,sys
m={'schema_version':1,'created_at_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),
   'archive':pathlib.Path(sys.argv[2]).name,'sha256':sys.argv[3],'scope':'grant-data:/workspace',
   'consistency':'application writers stopped during archive','embedding_model_cache_included':False}
pathlib.Path(sys.argv[1]).write_text(json.dumps(m,indent=2,sort_keys=True)+'\n')
PYBACKUP
restart
WAS_RUNNING=0
trap 'rm -f "$TMP"' EXIT
echo "Backup created: $OUT"
echo "SHA-256: $SHA"

#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
BACKUP="${1:-}"
[[ -n "$BACKUP" && -f "$BACKUP" ]] || { echo "Usage: $0 /path/to/grant-data.tar.gz" >&2; exit 2; }
[[ "${CONFIRM_RESTORE:-}" == "RESTORE" ]] || { echo "Restore is destructive. Re-run with CONFIRM_RESTORE=RESTORE." >&2; exit 3; }
if [[ -f "$BACKUP.sha256" ]]; then
  (cd "$(dirname "$BACKUP")" && shasum -a 256 -c "$(basename "$BACKUP").sha256")
fi
python3 - "$BACKUP" <<'PYRESTORE'
import pathlib,sys,tarfile
p=pathlib.Path(sys.argv[1])
with tarfile.open(p,'r:gz') as tf:
    members=tf.getmembers()
    if not members: raise SystemExit('ERROR: empty backup archive')
    for m in members:
        n=pathlib.PurePosixPath(m.name)
        if n.is_absolute() or '..' in n.parts: raise SystemExit(f'ERROR: unsafe archive path: {m.name}')
        if m.issym() or m.islnk() or m.isdev(): raise SystemExit(f'ERROR: links/devices are not permitted in backup: {m.name}')
print(f'Archive safety validation passed ({len(members)} entries).')
PYRESTORE

docker compose stop ui renderer core >/dev/null 2>&1 || true
restore_failed=1
restart_on_exit(){ if [[ "$restore_failed" == 1 ]]; then docker compose up -d core renderer ui >/dev/null 2>&1 || true; fi; }
trap restart_on_exit EXIT
cat "$BACKUP" | docker compose run --rm -T --no-deps --entrypoint /bin/sh core -lc \
  'find /workspace -mindepth 1 -maxdepth 1 -exec rm -rf {} +; tar -xzf - -C /workspace'
docker compose up -d core renderer ui
for _ in $(seq 1 60); do curl -fsS http://127.0.0.1:8080/health/ready >/dev/null 2>&1 && break; sleep 2; done
curl -fsS http://127.0.0.1:8080/health/ready >/dev/null
restore_failed=0
trap - EXIT
echo "Restore completed and services are healthy."

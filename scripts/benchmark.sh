#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
CORE="${CORE_URL:-http://127.0.0.1:8080}"
OUTDIR="${BENCHMARK_OUTPUT_DIR:-$ROOT/benchmarks}"
ITERATIONS="${BENCHMARK_ITERATIONS:-3}"
mkdir -p "$OUTDIR"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="$OUTDIR/benchmark-$TS.json"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

curl -fsS "$CORE/health/ready" > "$TMP/ready.json"
curl -fsS "$CORE/api/system/info" > "$TMP/system.json"
for i in $(seq 1 "$ITERATIONS"); do
  curl -fsS -X POST "$CORE/api/hpc/benchmark" > "$TMP/hpc-$i.json"
done

docker stats --no-stream --format '{{json .}}' > "$TMP/docker-stats.jsonl" 2>/dev/null || true
python3 - "$OUT" "$TMP" "$ITERATIONS" <<'PY'
import json,platform,statistics,sys,pathlib,subprocess,os
out=pathlib.Path(sys.argv[1]); tmp=pathlib.Path(sys.argv[2]); n=int(sys.argv[3])
hpc=[json.load(open(tmp/f'hpc-{i}.json')) for i in range(1,n+1)]
def median(key):
    vals=[float(x[key]) for x in hpc if key in x and x[key] is not None]
    return statistics.median(vals) if vals else None
stats=[]
p=tmp/'docker-stats.jsonl'
if p.exists():
    for line in p.read_text().splitlines():
        try: stats.append(json.loads(line))
        except Exception: pass
report={
 'schema_version':1,
 'generated_at_utc':subprocess.check_output(['date','-u','+%Y-%m-%dT%H:%M:%SZ'],text=True).strip(),
 'host':{'platform':platform.platform(),'machine':platform.machine(),'python':platform.python_version()},
 'system':json.load(open(tmp/'system.json')),
 'readiness':json.load(open(tmp/'ready.json')),
 'iterations':hpc,
 'summary':{
   'normalize_ms_median':median('normalize_ms'),
   'sgemv_ms_median':median('sgemv_ms'),
   'mmap_create_ms_median':median('mmap_create_ms'),
   'mmap_open_ms_median':median('mmap_open_ms'),
   'mmap_score_ms_median':median('mmap_score_ms')
 },
 'docker_stats':stats
}
out.write_text(json.dumps(report,indent=2))
print(out)
PY
echo "Benchmark report: $OUT"

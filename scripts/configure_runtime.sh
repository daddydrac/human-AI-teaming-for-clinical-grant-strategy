#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/.runtime.env}"
ARCH="$(uname -m 2>/dev/null || echo unknown)"
CORES="$(sysctl -n hw.logicalcpu 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2)"
MEM_BYTES="$(sysctl -n hw.memsize 2>/dev/null || echo 8589934592)"
MEM_GB=$(( MEM_BYTES / 1024 / 1024 / 1024 ))
OVERRIDE="${GRANT_RUNTIME_PROFILE:-auto}"

if [[ "$OVERRIDE" != "auto" ]]; then
  PROFILE="$OVERRIDE"
elif [[ "$ARCH" == "arm64" && "$MEM_GB" -ge 16 ]]; then
  PROFILE="apple_mlx"
else
  PROFILE="docker_cpu"
fi

if [[ "$PROFILE" == "apple_mlx" ]]; then
  OMP=$(( CORES > 10 ? CORES-4 : (CORES > 6 ? CORES-2 : (CORES > 2 ? CORES-1 : 1)) ))
  cat > "$OUT" <<EOF
GRANT_RUNTIME_PROFILE=apple_mlx
COMPOSE_PROFILES=
MODEL_ROUTING_MODE=hybrid
EMBEDDING_URL=http://host.docker.internal:8000/v1/embeddings
EMBEDDING_API_MODEL=grant-embedding
EMBEDDING_DOCUMENT_PREFIX=
EMBEDDING_QUERY_PREFIX=
EMBEDDING_BATCH_SIZE=64
INDEX_EMBED_BATCH_RECORDS=64
OMP_NUM_THREADS=$OMP
RAYON_NUM_THREADS=$OMP
OPENBLAS_NUM_THREADS=1
RESEARCH_MAX_CONCURRENCY=8
CONTEXT_RETRIEVAL_K=24
CONTEXT_MAX_CHARS=48000
CORE_MEMORY_LIMIT=2g
UI_MEMORY_LIMIT=768m
RENDERER_MEMORY_LIMIT=1g
INGESTION_MEMORY_LIMIT=1g
CORE_CPU_LIMIT=$OMP
UI_CPU_LIMIT=1.0
RENDERER_CPU_LIMIT=1.5
INGESTION_CPU_LIMIT=1.5
EMBEDDING_CPU_LIMIT=2.0
EOF
else
  THREADS=$(( CORES >= 8 ? 3 : (CORES >= 4 ? 2 : 1) ))
  [[ "$MEM_GB" -le 8 && "$THREADS" -gt 2 ]] && THREADS=2
  BATCH=$(( MEM_GB <= 8 ? 8 : 16 ))
  CTX=$(( MEM_GB <= 8 ? 24000 : 36000 ))
  K=$(( MEM_GB <= 8 ? 12 : 18 ))
  cat > "$OUT" <<EOF
GRANT_RUNTIME_PROFILE=docker_cpu
COMPOSE_PROFILES=cpu-embedding
MODEL_ROUTING_MODE=claude_only
EMBEDDING_URL=http://embedding-cpu:8010/v1/embeddings
EMBEDDING_API_MODEL=grant-embedding-cpu
CPU_EMBEDDING_API_MODEL=grant-embedding-cpu
CPU_EMBEDDING_MODEL=BAAI/bge-small-en-v1.5
CPU_EMBEDDING_THREADS=$THREADS
CPU_EMBEDDING_BATCH_SIZE=$BATCH
EMBEDDING_DOCUMENT_PREFIX="passage: "
EMBEDDING_QUERY_PREFIX="query: "
EMBEDDING_BATCH_SIZE=$BATCH
INDEX_EMBED_BATCH_RECORDS=$BATCH
OMP_NUM_THREADS=$THREADS
RAYON_NUM_THREADS=$THREADS
OPENBLAS_NUM_THREADS=1
RESEARCH_MAX_CONCURRENCY=3
CONTEXT_RETRIEVAL_K=$K
CONTEXT_MAX_CHARS=$CTX
DOCUMENT_CHUNK_WORDS=320
DOCUMENT_CHUNK_OVERLAP_WORDS=48
CORE_MEMORY_LIMIT=1200m
UI_MEMORY_LIMIT=640m
RENDERER_MEMORY_LIMIT=768m
INGESTION_MEMORY_LIMIT=768m
EMBEDDING_MEMORY_LIMIT=768m
CORE_CPU_LIMIT=2.0
UI_CPU_LIMIT=0.75
RENDERER_CPU_LIMIT=1.0
INGESTION_CPU_LIMIT=1.0
EMBEDDING_CPU_LIMIT=2.0
EOF
fi

cat <<EOF
Grant Writer runtime profile: $PROFILE
  architecture: $ARCH
  logical CPUs: $CORES
  memory: ${MEM_GB} GB
  runtime overrides: $OUT
EOF
if [[ "$PROFILE" == "docker_cpu" ]]; then
  echo "  inference: Claude API (local 7B disabled for speed/memory)"
  echo "  embeddings: FastEmbed/ONNX CPU container"
else
  echo "  inference: native Apple MLX + selective Claude escalation"
  echo "  embeddings: native Apple MLX"
fi

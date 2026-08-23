#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/.runtime.env}"
ARCH="$(uname -m 2>/dev/null || echo unknown)"
CORES="$(sysctl -n hw.logicalcpu 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2)"
MEM_BYTES="$(sysctl -n hw.memsize 2>/dev/null || echo 8589934592)"
MEM_GB=$(( MEM_BYTES / 1024 / 1024 / 1024 ))
OVERRIDE="${GRANT_RUNTIME_PROFILE:-auto}"

if [[ "$OVERRIDE" == "apple_mlx" || "$OVERRIDE" == "apple_ollama" ]]; then
  echo "Native runtime profile '$OVERRIDE' is being migrated to the portable container_ollama profile." >&2
  PROFILE="container_ollama"
elif [[ "$OVERRIDE" != "auto" ]]; then
  PROFILE="$OVERRIDE"
elif [[ "$ARCH" == "arm64" ]]; then
  PROFILE="container_ollama"
else
  PROFILE="docker_cpu"
fi

case "$PROFILE" in
  container_ollama|docker_cpu) ;;
  *) echo "Unsupported GRANT_RUNTIME_PROFILE: $PROFILE" >&2; exit 2 ;;
esac

if [[ "$PROFILE" == "container_ollama" ]]; then
  THREADS=$(( CORES >= 8 ? 2 : 1 ))
  ROUTING_MODE="${MODEL_ROUTING_MODE:-local_only}"
  LOCAL_MODEL="${LOCAL_LLM_API_MODEL:-${OLLAMA_MODEL:-qwen3:1.7b}}"
  cat > "$OUT" <<EOF
GRANT_RUNTIME_PROFILE=container_ollama
COMPOSE_PROFILES=cpu-embedding,local-model
MODEL_ROUTING_MODE=$ROUTING_MODE
LOCAL_LLM_PROVIDER=ollama
LOCAL_LLM_URL=http://ollama:11434/v1/chat/completions
LOCAL_LLM_API_MODEL=$LOCAL_MODEL
LOCAL_LLM_MAX_TOKENS=${LOCAL_LLM_MAX_TOKENS:-2048}
OLMO_URL=http://ollama:11434/v1/chat/completions
OLMO_API_MODEL=$LOCAL_MODEL
OLMO_MAX_TOKENS=${LOCAL_LLM_MAX_TOKENS:-2048}
EMBEDDING_URL=http://embedding-cpu:8010/v1/embeddings
EMBEDDING_API_MODEL=grant-embedding-cpu
CPU_EMBEDDING_API_MODEL=grant-embedding-cpu
CPU_EMBEDDING_MODEL=BAAI/bge-small-en-v1.5
CPU_EMBEDDING_THREADS=$THREADS
CPU_EMBEDDING_BATCH_SIZE=4
EMBEDDING_DOCUMENT_PREFIX="passage: "
EMBEDDING_QUERY_PREFIX="query: "
EMBEDDING_BATCH_SIZE=4
INDEX_EMBED_BATCH_RECORDS=4
OMP_NUM_THREADS=$THREADS
RAYON_NUM_THREADS=$THREADS
OPENBLAS_NUM_THREADS=1
RESEARCH_MAX_CONCURRENCY=2
CONTEXT_RETRIEVAL_K=8
CONTEXT_MAX_CHARS=8000
DOCUMENT_CHUNK_WORDS=240
DOCUMENT_CHUNK_OVERLAP_WORDS=32
CORE_MEMORY_LIMIT=900m
UI_MEMORY_LIMIT=512m
RENDERER_MEMORY_LIMIT=512m
INGESTION_MEMORY_LIMIT=512m
EMBEDDING_MEMORY_LIMIT=512m
CORE_CPU_LIMIT=$THREADS.0
UI_CPU_LIMIT=0.5
RENDERER_CPU_LIMIT=0.75
INGESTION_CPU_LIMIT=0.75
EMBEDDING_CPU_LIMIT=1.0
OLLAMA_MEMORY_LIMIT=$(( MEM_GB >= 16 ? 10 : 3 ))g
OLLAMA_CPU_LIMIT=$(( CORES >= 8 ? CORES-2 : (CORES >= 4 ? CORES-1 : 2) )).0
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
elif [[ "$PROFILE" == "container_ollama" ]]; then
  echo "  inference: containerized Ollama (${LOCAL_MODEL}) with ${ROUTING_MODE} routing"
  echo "  embeddings: FastEmbed/ONNX CPU container"
fi

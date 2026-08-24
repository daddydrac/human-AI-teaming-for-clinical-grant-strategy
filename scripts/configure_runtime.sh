#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/.runtime.env}"
OS="${GRANT_RUNTIME_OS_OVERRIDE:-$(uname -s 2>/dev/null || echo unknown)}"
ARCH="${GRANT_RUNTIME_ARCH_OVERRIDE:-$(uname -m 2>/dev/null || echo unknown)}"
CORES="$(sysctl -n hw.logicalcpu 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2)"
if [[ "$OS" == "Darwin" ]]; then
  MEM_BYTES="$(sysctl -n hw.memsize 2>/dev/null || echo 8589934592)"
elif [[ -r /proc/meminfo ]]; then
  MEM_BYTES="$(( $(awk '/^MemTotal:/ {print $2; exit}' /proc/meminfo) * 1024 ))"
else
  MEM_BYTES=8589934592
fi
MEM_GB=$(( MEM_BYTES / 1024 / 1024 / 1024 ))
OVERRIDE="${GRANT_RUNTIME_PROFILE:-auto}"
ROUTING_MODE="${MODEL_ROUTING_MODE:-local_only}"

case "$ROUTING_MODE" in
  local_only|hybrid|claude_only) ;;
  *) echo "Unsupported MODEL_ROUTING_MODE: $ROUTING_MODE" >&2; exit 2 ;;
esac

if [[ "$OVERRIDE" == "container_ollama" ]]; then
  if [[ "$OS" == "Darwin" && "$ARCH" == "arm64" ]]; then
    echo "Migrating legacy container_ollama to apple_ollama so Ollama can use Metal." >&2
    PROFILE="apple_ollama"
  elif [[ "$OS" == "Linux" && "$ARCH" == "x86_64" ]]; then
    echo "Migrating legacy container_ollama to linux_nvidia_ollama." >&2
    PROFILE="linux_nvidia_ollama"
  else
    PROFILE="docker_cpu"
  fi
elif [[ "$OVERRIDE" == "apple_mlx" ]]; then
  echo "Migrating legacy apple_mlx to the supported native apple_ollama profile." >&2
  PROFILE="apple_ollama"
elif [[ "$OVERRIDE" != "auto" ]]; then
  PROFILE="$OVERRIDE"
elif [[ "$OS" == "Darwin" && "$ARCH" == "arm64" ]]; then
  PROFILE="apple_ollama"
elif [[ "$OS" == "Linux" && "$ARCH" == "x86_64" ]] && command -v nvidia-smi >/dev/null 2>&1; then
  PROFILE="linux_nvidia_ollama"
else
  PROFILE="docker_cpu"
fi

case "$PROFILE" in
  apple_ollama)
    [[ "$OS" == "Darwin" && "$ARCH" == "arm64" ]] || {
      echo "apple_ollama requires an Apple Silicon Mac." >&2; exit 2;
    }
    ;;
  linux_nvidia_ollama)
    [[ "$OS" == "Linux" && "$ARCH" == "x86_64" ]] || {
      echo "linux_nvidia_ollama requires Linux x86_64." >&2; exit 2;
    }
    ;;
  docker_cpu) ;;
  *) echo "Unsupported GRANT_RUNTIME_PROFILE: $PROFILE" >&2; exit 2 ;;
esac

if [[ "$PROFILE" == "docker_cpu" && "$ROUTING_MODE" != "claude_only" ]]; then
  echo "docker_cpu has no local generator; set MODEL_ROUTING_MODE=claude_only." >&2
  exit 2
fi

THREADS=$(( CORES >= 8 ? 2 : 1 ))
[[ "$PROFILE" == "linux_nvidia_ollama" && "$CORES" -ge 8 ]] && THREADS=4
LOCAL_MODEL="${LOCAL_LLM_API_MODEL:-${OLLAMA_MODEL:-qwen3:1.7b}}"
COMPOSE_PROFILES="cpu-embedding"
LOCAL_URL="http://host.docker.internal:11434/v1/chat/completions"
if [[ "$PROFILE" == "linux_nvidia_ollama" ]]; then
  LOCAL_URL="http://ollama:11434/v1/chat/completions"
  [[ "$ROUTING_MODE" == "claude_only" ]] || COMPOSE_PROFILES="cpu-embedding,local-model"
fi

if [[ "$PROFILE" == "apple_ollama" ]]; then
  if [[ "$MEM_GB" -le 10 ]]; then
    CORE_MEMORY=900m; UI_MEMORY=512m; SERVICE_MEMORY=512m
    CORE_CPUS=1.5; LOCAL_MAX_TOKENS="${LOCAL_LLM_MAX_TOKENS:-2048}"; DEFAULT_CONTEXT_TOKENS=4096
    CONTEXT_CHARS=8000; RETRIEVAL_K=8; RESEARCH_CONCURRENCY=2
  else
    CORE_MEMORY=2g; UI_MEMORY=768m; SERVICE_MEMORY=1g
    CORE_CPUS=4.0; LOCAL_MAX_TOKENS="${LOCAL_LLM_MAX_TOKENS:-8192}"; DEFAULT_CONTEXT_TOKENS=16384
    CONTEXT_CHARS=32000; RETRIEVAL_K=18; RESEARCH_CONCURRENCY=4
  fi
elif [[ "$PROFILE" == "linux_nvidia_ollama" ]]; then
  CORE_MEMORY=3g; UI_MEMORY=1g; SERVICE_MEMORY=1g
  CORE_CPUS=$(( CORES >= 8 ? 6 : 3 )).0
  LOCAL_MAX_TOKENS="${LOCAL_LLM_MAX_TOKENS:-12000}"; DEFAULT_CONTEXT_TOKENS=16384
  CONTEXT_CHARS=48000; RETRIEVAL_K=24; RESEARCH_CONCURRENCY=8
else
  CORE_MEMORY=1536m; UI_MEMORY=768m; SERVICE_MEMORY=768m
  CORE_CPUS=2.0; LOCAL_MAX_TOKENS="${LOCAL_LLM_MAX_TOKENS:-4096}"; DEFAULT_CONTEXT_TOKENS=8192
  CONTEXT_CHARS=36000; RETRIEVAL_K=18; RESEARCH_CONCURRENCY=3
fi

cat > "$OUT" <<EOF
GRANT_RUNTIME_PROFILE=$PROFILE
COMPOSE_PROFILES=$COMPOSE_PROFILES
MODEL_ROUTING_MODE=$ROUTING_MODE
REQUIRE_CLAUDE_IN_HYBRID=$([[ "$ROUTING_MODE" == "hybrid" ]] && echo true || echo false)
LOCAL_LLM_PROVIDER=ollama
LOCAL_LLM_URL=$LOCAL_URL
LOCAL_LLM_API_MODEL=$LOCAL_MODEL
LOCAL_LLM_MAX_TOKENS=$LOCAL_MAX_TOKENS
LOCAL_LLM_CONTEXT_TOKENS=${LOCAL_LLM_CONTEXT_TOKENS:-${OLLAMA_CONTEXT_LENGTH:-$DEFAULT_CONTEXT_TOKENS}}
OLMO_URL=$LOCAL_URL
OLMO_API_MODEL=$LOCAL_MODEL
OLMO_MAX_TOKENS=$LOCAL_MAX_TOKENS
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
RESEARCH_MAX_CONCURRENCY=$RESEARCH_CONCURRENCY
CONTEXT_RETRIEVAL_K=$RETRIEVAL_K
CONTEXT_MAX_CHARS=$CONTEXT_CHARS
DOCUMENT_CHUNK_WORDS=240
DOCUMENT_CHUNK_OVERLAP_WORDS=32
CORE_MEMORY_LIMIT=$CORE_MEMORY
UI_MEMORY_LIMIT=$UI_MEMORY
RENDERER_MEMORY_LIMIT=$SERVICE_MEMORY
INGESTION_MEMORY_LIMIT=$SERVICE_MEMORY
EMBEDDING_MEMORY_LIMIT=$SERVICE_MEMORY
CORE_CPU_LIMIT=$CORE_CPUS
UI_CPU_LIMIT=1.0
RENDERER_CPU_LIMIT=1.0
INGESTION_CPU_LIMIT=1.0
EMBEDDING_CPU_LIMIT=$THREADS.0
OLLAMA_MEMORY_LIMIT=$(( MEM_GB >= 24 ? 16 : (MEM_GB >= 12 ? 8 : 3) ))g
OLLAMA_CPU_LIMIT=$(( CORES >= 8 ? CORES-2 : (CORES >= 4 ? CORES-1 : 2) )).0
EOF

cat <<EOF
Grant Writer runtime profile: $PROFILE
  operating system: $OS
  architecture: $ARCH
  logical CPUs: $CORES
  memory: ${MEM_GB} GB
  runtime overrides: $OUT
EOF
case "$PROFILE:$ROUTING_MODE" in
  apple_ollama:claude_only) echo "  inference: Claude only; native Ollama will not be started" ;;
  apple_ollama:*) echo "  inference: native macOS Ollama/Metal ($LOCAL_MODEL) with $ROUTING_MODE routing" ;;
  linux_nvidia_ollama:claude_only) echo "  inference: Claude only; the GPU model container will not be started" ;;
  linux_nvidia_ollama:*) echo "  inference: NVIDIA GPU Ollama container ($LOCAL_MODEL) with $ROUTING_MODE routing" ;;
  docker_cpu:*) echo "  inference: Claude API" ;;
esac
echo "  embeddings: FastEmbed/ONNX CPU container"

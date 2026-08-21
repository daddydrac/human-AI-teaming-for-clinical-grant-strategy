#!/usr/bin/env bash
set -euo pipefail

# Source repositories/revisions are provenance. API model names are stable local aliases.
LLM_REPO="${OLMO_MODEL_REPO:-${OLMO_MODEL:-mlx-community/Olmo-3-7B-Instruct-4bit}}"
EMBED_REPO="${EMBEDDING_MODEL_REPO:-${EMBEDDING_MODEL:-mlx-community/all-MiniLM-L6-v2-4bit}}"
LLM_REVISION="${OLMO_MODEL_REVISION:-}"
EMBED_REVISION="${EMBEDDING_MODEL_REVISION:-}"
LLM_API_NAME="${OLMO_API_MODEL:-grant-olmo}"
EMBED_API_NAME="${EMBEDDING_API_MODEL:-grant-embedding}"
PORT="${OLMO_PORT:-8000}"
VLLM_MLX_VERSION="${VLLM_MLX_VERSION:-0.4.1}"
RUNTIME_DIR="${GRANT_MLX_RUNTIME_DIR:-$HOME/Library/Application Support/GrantWriter/mlx-runtime}"
VENV="$RUNTIME_DIR/.venv"
VERSION_FILE="$RUNTIME_DIR/vllm-mlx.version"
MANIFEST="$RUNTIME_DIR/runtime-manifest.json"
MODEL_ROOT="$RUNTIME_DIR/models"
SERVE_ROOT="$RUNTIME_DIR/served"
RESOLUTION="$RUNTIME_DIR/model-resolution.json"
SCRIPT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RELEASE_LOCK="$SCRIPT_ROOT/config/mlx-runtime.lock"
RUNTIME_LOCK="$RUNTIME_DIR/mlx-runtime.resolved.lock"

valid_alias='^[A-Za-z0-9_.-]+$'
[[ "$LLM_API_NAME" =~ $valid_alias ]] || { echo "Invalid OLMO_API_MODEL alias: $LLM_API_NAME" >&2; exit 2; }
[[ "$EMBED_API_NAME" =~ $valid_alias ]] || { echo "Invalid EMBEDDING_API_MODEL alias: $EMBED_API_NAME" >&2; exit 2; }
[[ "$LLM_API_NAME" != "$EMBED_API_NAME" ]] || { echo "Generation and embedding API aliases must differ" >&2; exit 2; }

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
  echo "MLX inference requires an Apple Silicon Mac." >&2; exit 2
fi
if ! command -v uv >/dev/null; then
  echo "Install uv first: https://docs.astral.sh/uv/" >&2; exit 3
fi
mkdir -p "$RUNTIME_DIR" "$MODEL_ROOT" "$SERVE_ROOT"
INSTALLED="$(cat "$VERSION_FILE" 2>/dev/null || true)"
if [[ ! -x "$VENV/bin/vllm-mlx" || "$INSTALLED" != "$VLLM_MLX_VERSION" ]]; then
  rm -rf "$VENV"
  uv venv "$VENV" --python 3.12
  if [[ -s "$RELEASE_LOCK" ]]; then
    echo "Installing native MLX runtime from release lock: $RELEASE_LOCK"
    uv pip install --python "$VENV/bin/python" -r "$RELEASE_LOCK"
  else
    # First controlled resolution. scripts/freeze_mlx_dependencies.sh promotes the
    # resulting exact environment into config/mlx-runtime.lock for a release.
    uv pip install --python "$VENV/bin/python" "vllm-mlx==$VLLM_MLX_VERSION"
  fi
  uv pip freeze --strict --python "$VENV/bin/python" > "$RUNTIME_LOCK"
  printf '%s' "$VLLM_MLX_VERSION" > "$VERSION_FILE"
fi

# Resolve model repos to immutable Hugging Face commit SHAs and materialize those exact snapshots.
"$VENV/bin/python" - "$LLM_REPO" "$LLM_REVISION" "$EMBED_REPO" "$EMBED_REVISION" "$MODEL_ROOT" "$RESOLUTION" <<'PYRESOLVE'
import json, sys
from pathlib import Path
from huggingface_hub import HfApi, snapshot_download
llm_repo,llm_rev,embed_repo,embed_rev,root,resolution=sys.argv[1:]
api=HfApi()
def resolve(repo, requested, kind):
    info=api.model_info(repo, revision=requested or None)
    sha=info.sha
    local=Path(root)/kind/sha
    local.mkdir(parents=True,exist_ok=True)
    snapshot_download(repo_id=repo, revision=sha, local_dir=str(local))
    return {"repo":repo,"requested_revision":requested or None,"resolved_sha":sha,"local_path":str(local)}
out={"generation":resolve(llm_repo,llm_rev,"generation"),"embedding":resolve(embed_repo,embed_rev,"embedding")}
Path(resolution).write_text(json.dumps(out,indent=2))
PYRESOLVE

LLM_MODEL_PATH="$("$VENV/bin/python" -c 'import json,sys; print(json.load(open(sys.argv[1]))["generation"]["local_path"])' "$RESOLUTION")"
EMBED_MODEL_PATH="$("$VENV/bin/python" -c 'import json,sys; print(json.load(open(sys.argv[1]))["embedding"]["local_path"])' "$RESOLUTION")"

# vllm-mlx v0.4.1 uses the embedding model identifier supplied at startup to validate
# /v1/embeddings requests. Stable symlink aliases let the API client use deterministic names
# while the backing models remain immutable SHA-addressed snapshots.
rm -f "$SERVE_ROOT/$LLM_API_NAME" "$SERVE_ROOT/$EMBED_API_NAME"
ln -s "$LLM_MODEL_PATH" "$SERVE_ROOT/$LLM_API_NAME"
ln -s "$EMBED_MODEL_PATH" "$SERVE_ROOT/$EMBED_API_NAME"

"$VENV/bin/python" - "$MANIFEST" "$RESOLUTION" "$VLLM_MLX_VERSION" "$LLM_API_NAME" "$EMBED_API_NAME" <<'PYMANIFEST'
import importlib.metadata, json, platform, sys
from pathlib import Path
manifest_path,resolution_path,vllm_version,llm_api,embed_api=sys.argv[1:]
manifest={
  "vllm_mlx_version":vllm_version,
  "python":platform.python_version(),
  "machine":platform.machine(),
  "macos":platform.mac_ver()[0],
  "api_models":{"generation":llm_api,"embedding":embed_api},
  "models":json.load(open(resolution_path)),
  "installed_distributions":sorted(
      {"name":d.metadata.get("Name", d.name), "version":d.version}
      for d in importlib.metadata.distributions()
      if d.metadata.get("Name")
  , key=lambda x:(x["name"].lower(),x["version"])),
}
Path(manifest_path).write_text(json.dumps(manifest,indent=2))
PYMANIFEST

echo "Starting vllm-mlx $VLLM_MLX_VERSION with immutable local model snapshots."
echo "Generation: $LLM_REPO -> $LLM_MODEL_PATH (API: $LLM_API_NAME)"
echo "Embedding:  $EMBED_REPO -> $EMBED_MODEL_PATH (API: $EMBED_API_NAME)"
echo "Runtime manifest: $MANIFEST"
# v0.4.1 supports --embedding-model. Unsupported later embedding flags are intentionally absent.
cd "$SERVE_ROOT"
exec caffeinate -dimsu "$VENV/bin/vllm-mlx" serve "$LLM_API_NAME" \
  --served-model-name "$LLM_API_NAME" \
  --host 127.0.0.1 --port "$PORT" \
  --embedding-model "$EMBED_API_NAME"

#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RUNTIME_DIR="${GRANT_MLX_RUNTIME_DIR:-$HOME/Library/Application Support/GrantWriter/mlx-runtime}"
VENV="$RUNTIME_DIR/.venv"
OUT="$ROOT/config/mlx-runtime.lock"
if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
  echo "Generate the native MLX release lock on the target Apple Silicon Mac." >&2; exit 2
fi
command -v uv >/dev/null || { echo "uv is required" >&2; exit 3; }
[[ -x "$VENV/bin/python" ]] || { echo "Run scripts/start_mlx.sh once to create the validated native environment." >&2; exit 4; }
TMP="$(mktemp)"; trap 'rm -f "$TMP"' EXIT
uv pip freeze --strict --python "$VENV/bin/python" > "$TMP"
grep -q '^vllm-mlx==0\.4\.1$' "$TMP" || { echo "Refusing to freeze an unexpected vllm-mlx version." >&2; exit 5; }
# Freeze output is exact name==version requirements. It is platform-specific by design.
cp "$TMP" "$OUT"
echo "Wrote $OUT"
shasum -a 256 "$OUT"

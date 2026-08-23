#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
[[ "$(uname -s)" == "Darwin" ]] || { echo "ERROR: automatic desktop dependency installation currently supports macOS." >&2; exit 2; }

if ! command -v brew >/dev/null 2>&1; then
  echo "Installing Homebrew dependency manager..."
  NONINTERACTIVE=1 /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
  if [[ -x /opt/homebrew/bin/brew ]]; then
    eval "$(/opt/homebrew/bin/brew shellenv)"
  elif [[ -x /usr/local/bin/brew ]]; then
    eval "$(/usr/local/bin/brew shellenv)"
  fi
fi
command -v brew >/dev/null 2>&1 || { echo "ERROR: Homebrew installation completed but brew is not available in this shell." >&2; exit 3; }

install_formula(){ command -v "$1" >/dev/null 2>&1 || { echo "Installing $2..."; brew install "$2"; }; }

if ! command -v docker >/dev/null 2>&1; then
  echo "Installing Docker Desktop..."
  brew install --cask docker
fi
if ! docker info >/dev/null 2>&1; then
  echo "Starting Docker Desktop..."
  open -a Docker
  for _ in $(seq 1 120); do docker info >/dev/null 2>&1 && break; sleep 2; done
fi
docker info >/dev/null 2>&1 || { echo "ERROR: Docker Desktop did not become ready. Complete any first-launch prompt and rerun ./install.sh." >&2; exit 4; }

PROFILE="${GRANT_RUNTIME_PROFILE:-auto}"
if [[ "$PROFILE" == "auto" ]]; then
  if [[ "$(uname -m)" == "arm64" ]]; then PROFILE=container_ollama; else PROFILE=docker_cpu; fi
fi

case "$PROFILE" in
  apple_ollama|apple_mlx)
    echo "Native profile '$PROFILE' is deprecated; using container_ollama."
    PROFILE=container_ollama
    ;;
  container_ollama|docker_cpu) ;;
  *) echo "ERROR: unsupported GRANT_RUNTIME_PROFILE: $PROFILE" >&2; exit 5 ;;
esac

echo "Container host dependencies ready for runtime profile: $PROFILE"

#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OS="$(uname -s)"
ARCH="$(uname -m)"
PROFILE="${GRANT_RUNTIME_PROFILE:-auto}"
ROUTING_MODE="${MODEL_ROUTING_MODE:-local_only}"
ADMIN_CMD=()
if [[ "$(id -u)" -ne 0 ]] && command -v sudo >/dev/null 2>&1; then ADMIN_CMD=(sudo); fi

if [[ "$PROFILE" == "auto" || "$PROFILE" == "container_ollama" ]]; then
  if [[ "$OS" == "Darwin" && "$ARCH" == "arm64" ]]; then
    PROFILE=apple_ollama
  elif [[ "$OS" == "Linux" && "$ARCH" == "x86_64" ]]; then
    PROFILE=linux_nvidia_ollama
  else
    PROFILE=docker_cpu
  fi
elif [[ "$PROFILE" == "apple_mlx" ]]; then
  PROFILE=apple_ollama
fi

case "$OS" in
  Darwin)
    [[ "$ARCH" == "arm64" || "$PROFILE" == "docker_cpu" ]] || {
      echo "ERROR: native local inference is supported only on Apple Silicon." >&2; exit 2;
    }
    if ! command -v brew >/dev/null 2>&1; then
      echo "Installing Homebrew dependency manager..."
      NONINTERACTIVE=1 /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
      if [[ -x /opt/homebrew/bin/brew ]]; then
        eval "$(/opt/homebrew/bin/brew shellenv)"
      elif [[ -x /usr/local/bin/brew ]]; then
        eval "$(/usr/local/bin/brew shellenv)"
      fi
    fi
    command -v brew >/dev/null 2>&1 || {
      echo "ERROR: Homebrew installation completed but brew is not available in this shell." >&2; exit 3;
    }
    if ! command -v python3 >/dev/null 2>&1; then
      echo "Installing Python for host-side configuration checks..."
      brew install python
    fi
    if ! command -v docker >/dev/null 2>&1; then
      echo "Installing Docker Desktop..."
      brew install --cask docker
    fi
    if ! docker info >/dev/null 2>&1; then
      echo "Starting Docker Desktop..."
      open -a Docker
      for _ in $(seq 1 120); do docker info >/dev/null 2>&1 && break; sleep 2; done
    fi
    docker info >/dev/null 2>&1 || {
      echo "ERROR: Docker Desktop did not become ready. Complete any first-launch prompt and rerun ./install.sh." >&2; exit 4;
    }
    if [[ "$PROFILE" == "apple_ollama" && "$ROUTING_MODE" != "claude_only" ]] && ! command -v ollama >/dev/null 2>&1; then
      echo "Installing native Ollama for Apple Metal acceleration..."
      brew install ollama
    fi
    ;;
  Linux)
    [[ "$ARCH" == "x86_64" ]] || {
      echo "ERROR: the NVIDIA cloud/workstation profile requires Linux x86_64." >&2; exit 2;
    }
    if ! command -v curl >/dev/null 2>&1 || ! command -v python3 >/dev/null 2>&1; then
      [[ "$(id -u)" -eq 0 || ${#ADMIN_CMD[@]} -gt 0 ]] || { echo "ERROR: root or sudo is required to install host bootstrap tools." >&2; exit 3; }
      if command -v apt-get >/dev/null 2>&1; then
        "${ADMIN_CMD[@]}" apt-get update
        "${ADMIN_CMD[@]}" apt-get install -y ca-certificates curl python3
      elif command -v dnf >/dev/null 2>&1; then
        "${ADMIN_CMD[@]}" dnf install -y ca-certificates curl python3
      elif command -v yum >/dev/null 2>&1; then
        "${ADMIN_CMD[@]}" yum install -y ca-certificates curl python3
      else
        echo "ERROR: automatic host-tool installation supports apt, dnf, or yum." >&2; exit 3
      fi
    fi
    if ! command -v docker >/dev/null 2>&1; then
      [[ "$(id -u)" -eq 0 || ${#ADMIN_CMD[@]} -gt 0 ]] || { echo "ERROR: root or sudo is required to install Docker Engine." >&2; exit 3; }
      echo "Installing Docker Engine and the Compose plugin..."
      curl -fsSL https://get.docker.com -o /tmp/grantspace-get-docker.sh
      "${ADMIN_CMD[@]}" sh /tmp/grantspace-get-docker.sh
      if [[ "$(id -u)" -ne 0 ]]; then
        "${ADMIN_CMD[@]}" usermod -aG docker "${SUDO_USER:-${USER:-}}"
        echo "Docker was installed and your account was added to the docker group."
        echo "Start a new login shell, then rerun ./install.sh so group access is active."
        exit 10
      fi
    fi
    docker compose version >/dev/null 2>&1 || {
      echo "ERROR: Docker Compose v2 is required." >&2; exit 4;
    }
    docker info >/dev/null 2>&1 || {
      echo "ERROR: Docker is installed but unavailable to this account. Start Docker and activate docker-group membership." >&2; exit 4;
    }
    if [[ "$PROFILE" == "linux_nvidia_ollama" && "$ROUTING_MODE" != "claude_only" ]]; then
      command -v nvidia-smi >/dev/null 2>&1 || {
        echo "ERROR: no NVIDIA driver is visible. Use an NVIDIA-enabled VM image or install the provider driver, reboot, and rerun." >&2; exit 5;
      }
      if ! command -v nvidia-ctk >/dev/null 2>&1; then
        "$ROOT/scripts/install_nvidia_container_toolkit.sh"
      fi
      docker info --format '{{json .Runtimes}}' | grep -q 'nvidia' || {
        echo "Configuring Docker to use the NVIDIA Container Toolkit..."
        "${ADMIN_CMD[@]}" nvidia-ctk runtime configure --runtime=docker
        "${ADMIN_CMD[@]}" systemctl restart docker
      }
    fi
    ;;
  *) echo "ERROR: supported hosts are Apple Silicon macOS and x86-64 Linux." >&2; exit 2 ;;
esac

echo "Host dependencies ready for runtime profile: $PROFILE"

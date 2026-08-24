#!/usr/bin/env bash
set -euo pipefail

ADMIN_CMD=()
if [[ "$(id -u)" -ne 0 ]]; then
  command -v sudo >/dev/null 2>&1 || { echo "ERROR: root or sudo is required to install NVIDIA Container Toolkit." >&2; exit 2; }
  ADMIN_CMD=(sudo)
fi
command -v nvidia-smi >/dev/null 2>&1 || { echo "ERROR: install the NVIDIA driver before the container toolkit." >&2; exit 2; }

if command -v apt-get >/dev/null 2>&1; then
  command -v gpg >/dev/null 2>&1 || { "${ADMIN_CMD[@]}" apt-get update; "${ADMIN_CMD[@]}" apt-get install -y gpg; }
  curl -fsSL https://nvidia.github.io/libnvidia-container/gpgkey \
    | "${ADMIN_CMD[@]}" gpg --dearmor --yes -o /usr/share/keyrings/nvidia-container-toolkit-keyring.gpg
  curl -fsSL https://nvidia.github.io/libnvidia-container/stable/deb/nvidia-container-toolkit.list \
    | sed 's#deb https://#deb [signed-by=/usr/share/keyrings/nvidia-container-toolkit-keyring.gpg] https://#' \
    | "${ADMIN_CMD[@]}" tee /etc/apt/sources.list.d/nvidia-container-toolkit.list >/dev/null
  "${ADMIN_CMD[@]}" apt-get update
  "${ADMIN_CMD[@]}" apt-get install -y nvidia-container-toolkit
elif command -v dnf >/dev/null 2>&1; then
  curl -fsSL https://nvidia.github.io/libnvidia-container/stable/rpm/nvidia-container-toolkit.repo \
    | "${ADMIN_CMD[@]}" tee /etc/yum.repos.d/nvidia-container-toolkit.repo >/dev/null
  "${ADMIN_CMD[@]}" dnf install -y nvidia-container-toolkit
elif command -v yum >/dev/null 2>&1; then
  curl -fsSL https://nvidia.github.io/libnvidia-container/stable/rpm/nvidia-container-toolkit.repo \
    | "${ADMIN_CMD[@]}" tee /etc/yum.repos.d/nvidia-container-toolkit.repo >/dev/null
  "${ADMIN_CMD[@]}" yum install -y nvidia-container-toolkit
else
  echo "ERROR: automatic NVIDIA Container Toolkit installation supports apt, dnf, or yum hosts." >&2
  exit 3
fi

command -v nvidia-ctk >/dev/null 2>&1 || { echo "ERROR: NVIDIA Container Toolkit installation did not provide nvidia-ctk." >&2; exit 4; }

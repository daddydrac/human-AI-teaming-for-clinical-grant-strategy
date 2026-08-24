# Quickstart

Run one profile from the repository directory. The installer installs host
dependencies, generates the first-administrator token, prepares the model,
builds the application containers, and starts the GUI.

## Apple M2, 8 GB

```bash
cp env.m2Mac.8gb.txt .env
./install.sh
awk -F= '$1=="INITIAL_ADMIN_SETUP_TOKEN" {print $2}' .env
open http://localhost:7860/setup
```

This runs Qwen 3 1.7B in native Ollama with Apple Metal. The application and
embedding services remain in Docker.

## Apple M4, 24 GB

```bash
cp env.m4Mac.qwen3.txt .env
./install.sh
awk -F= '$1=="INITIAL_ADMIN_SETUP_TOKEN" {print $2}' .env
open http://localhost:7860/setup
```

This runs Qwen 3 8B in native Ollama with Apple Metal.

## Linux x86-64 with NVIDIA GPU

Start from an NVIDIA-enabled Linux image whose host driver is already working
(`nvidia-smi` must succeed), then run:

```bash
cp env.linux.nvidia.txt .env
./install.sh
awk -F= '$1=="INITIAL_ADMIN_SETUP_TOKEN" {print $2}' .env
```

Open `http://SERVER_IP:7860/setup`. The installer adds Docker and NVIDIA
Container Toolkit when missing. If it installs Docker for the first time, start
a new login shell and rerun `./install.sh` when prompted.

For a cloud VM, set these values in `.env` before startup:

```bash
APP_BIND_ADDRESS=0.0.0.0
APP_PUBLIC_URL=http://SERVER_IP:7860
```

## Model routing

Each hardware template defaults to local-only operation. To use local Qwen plus
Claude, edit `.env` before `./install.sh`:

```bash
MODEL_ROUTING_MODE=hybrid
ANTHROPIC_API_KEY=your_key
```

To use only Claude and skip local model startup/download:

```bash
MODEL_ROUTING_MODE=claude_only
ANTHROPIC_API_KEY=your_key
```

Accepted routing values are `local_only`, `hybrid`, and `claude_only`. Startup
rejects hybrid or Claude-only configuration when the Claude key is blank.

## First login

Paste the printed setup token into the setup page and enter the initial
administrator username, email, and temporary password. Sign in with that
username and temporary password; the GUI immediately requires a permanent
password.

## Later starts

```bash
./start.sh
```

Open `http://localhost:7860/login` on a local Mac, or the configured
`APP_PUBLIC_URL` for a Linux VM.

## Apply application-code changes

Python application code is bind-mounted. Restart only the changed service:

```bash
docker compose restart ui
```

Use `renderer`, `ingestion`, or `embedding-cpu` instead of `ui` when applicable.
`./start.sh` fingerprints Rust API inputs and rebuilds the core automatically.

## Stop without deleting grants or models

```bash
./stop.sh
```

On macOS, this stops native Ollama only if Grantspace started that exact process.
An externally managed Ollama service is left running.

## Start completely fresh

```bash
./reset-local-data.sh
./start.sh
```

Open `http://localhost:7860/setup`. The reset moves existing application data to
`backups/local-resets/`, prints a new setup token, and preserves model downloads.

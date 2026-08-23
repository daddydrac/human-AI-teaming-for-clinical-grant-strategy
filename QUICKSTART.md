# Quickstart

Run commands from the repository directory. `./install.sh` installs/starts
Docker Desktop when needed, downloads the selected model, builds missing
containers, starts the application, and opens the GUI. It uses fast startup
checks so you can validate behavior in the GUI without first running the full
release test suite.

## M2, 8 GB, local Qwen 3

```bash
cp env.m2Mac.8gb.txt .env
./install.sh
awk -F= '$1=="INITIAL_ADMIN_SETUP_TOKEN" {print $2}' .env
open http://localhost:7860/setup
```

Run each line separately. Paste the plain URL exactly as shown; do not paste
Markdown link syntax such as `[http://...](http://...)` into the shell.

In the GUI, paste the displayed setup token and enter the initial administrator
username, email, and temporary password. Sign in with that username and temporary
password; the GUI immediately requires a permanent password.

## M4, 24 GB, Ollama OLMo 3 + Claude

```bash
cp env.m4Mac.txt .env
open -W -a TextEdit .env
./install.sh
awk -F= '$1=="INITIAL_ADMIN_SETUP_TOKEN" {print $2}' .env
open http://localhost:7860/setup
```

Before closing TextEdit, set `ANTHROPIC_API_KEY` in `.env`. In the GUI, paste the
displayed setup token and enter the initial administrator username, email, and
temporary password. Sign in with that username and temporary password; the GUI
immediately requires a permanent password.

## M4, 24 GB, Ollama Qwen 3 + Claude

```bash
cp env.m4Mac.qwen3.txt .env
open -W -a TextEdit .env
./install.sh
awk -F= '$1=="INITIAL_ADMIN_SETUP_TOKEN" {print $2}' .env
open http://localhost:7860/setup
```

Before closing TextEdit, set `ANTHROPIC_API_KEY` in `.env`. Complete the same
first-administrator and mandatory first-password-change screens in the GUI.

## Later starts

```bash
./start.sh
open http://localhost:7860/login
```

## Test application-code changes

The Python application files are bind-mounted into their containers. After
editing UI, renderer, ingestion, or embedding code, restart only the affected
service; rebuilding is unnecessary when dependencies did not change:

```bash
docker compose restart ui
open http://localhost:7860/login
```

Use the corresponding service name for other Python services:

```bash
docker compose restart renderer
docker compose restart ingestion
docker compose restart embedding-cpu
```

Rust core changes and dependency-file changes must be rebuilt:

```bash
REBUILD=1 ./start.sh
```

The optional full build/test/release validation remains available for every
hardware profile:

```bash
RUN_FULL_VALIDATION=true START_AFTER_INSTALL=false ./install.sh
```

## Stop without deleting grants or models

```bash
./stop.sh
```

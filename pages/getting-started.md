---
title: Deploy and sign in
description: Deploy Grantspace on Apple Silicon or Linux with NVIDIA, then create the first administrator.
---

<div class="guide-layout">
<aside class="toc">
<strong>On this page</strong>
<a href="#before-you-start">Before you start</a>
<a href="#apple-m2">Apple M2</a>
<a href="#apple-m4">Apple M4</a>
<a href="#linux-nvidia">Linux + NVIDIA</a>
<a href="#first-administrator">First administrator</a>
<a href="#model-routing">Model routing</a>
<a href="#later-starts">Later starts</a>
</aside>
<article class="guide-content" markdown="1">

<div class="eyebrow">Deployment</div>
# Deploy and sign in

Grantspace packages the application, API, database, renderer, ingestion, and embedding services in Docker. On Apple Silicon, Ollama runs natively so Qwen can use Metal; on Linux NVIDIA systems, Ollama runs in a GPU-enabled container.

## Before you start

You need the Grantspace repository and permission to run Docker. The installer handles application dependencies and model preparation; a Linux GPU host must already return a working result from `nvidia-smi`.

<div class="callout"><strong>Your data survives normal stops.</strong> Use <code>./stop.sh</code> to stop services without deleting saved grants, accounts, or downloaded models.</div>

## Apple M2

Use the small Qwen profile on an 8 GB M2:

```bash
cp env.m2Mac.8gb.txt .env
./install.sh
awk -F= '$1=="INITIAL_ADMIN_SETUP_TOKEN" {print $2}' .env
open http://localhost:7860/setup
```

The native Ollama process serves Qwen 3 1.7B with Apple Metal. Docker reaches it through `host.docker.internal`.

## Apple M4

Use the larger Qwen profile on a 24 GB M4:

```bash
cp env.m4Mac.qwen3.txt .env
./install.sh
awk -F= '$1=="INITIAL_ADMIN_SETUP_TOKEN" {print $2}' .env
open http://localhost:7860/setup
```

This profile serves Qwen 3 8B through native Ollama and Apple Metal.

## Linux NVIDIA

Start on an x86-64 Linux host where `nvidia-smi` succeeds:

```bash
cp env.linux.nvidia.txt .env
./install.sh
awk -F= '$1=="INITIAL_ADMIN_SETUP_TOKEN" {print $2}' .env
```

For a VM, set the reachable address in `.env` before starting:

```bash
APP_BIND_ADDRESS=0.0.0.0
APP_PUBLIC_URL=https://grantspace.example.org
```

Open `APP_PUBLIC_URL/setup`. Put TLS and your normal network controls in front of internet-facing deployments.

## First administrator

Only the first person can create an account from the setup page. Enter the printed setup token, a username, an email address, a display name, and a temporary password.

<figure class="screenshot">
  <img src="{{ '/assets/images/first-admin.png' | relative_url }}" alt="First administrator setup form with setup token, username, email, display name, and temporary password">
  <figcaption>The setup token is generated for the deployment. The example data shown here is fictional.</figcaption>
</figure>

After setup:

1. Sign in with the username and temporary password.
2. Create a permanent password when prompted.
3. Open **Administration** to create accounts for other users.

The bootstrap administrator is the only system administrator. Later accounts are created inside the application and must also change their first password.

## Model routing

Set one deployment policy in `.env` before `./install.sh`:

| Mode | Behavior |
|---|---|
| `local_only` | Sends model work only to the configured local Qwen runtime. |
| `hybrid` | Uses local Qwen and routes configured high-value tasks to Claude. |
| `claude_only` | Uses Claude and skips local model startup and download. |

Hybrid and Claude-only modes require `ANTHROPIC_API_KEY`. Startup rejects those modes when the key is blank.

```bash
MODEL_ROUTING_MODE=hybrid
ANTHROPIC_API_KEY=your_key
```

## Later starts

```bash
./start.sh
```

Open `http://localhost:7860/login` on a local Mac or the configured `APP_PUBLIC_URL` on a shared host.

To start with a new database while retaining a recoverable copy of current local data:

```bash
./reset-local-data.sh
./start.sh
```

The reset moves the previous data into `backups/local-resets/` and generates a new first-administrator token.

</article>
</div>


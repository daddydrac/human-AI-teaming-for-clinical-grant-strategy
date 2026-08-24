---
title: Troubleshooting
description: Resolve common Grantspace startup, login, model, email, drafting, collaboration, and publishing problems.
---

<div class="guide-layout">
<aside class="toc">
<strong>On this page</strong>
<a href="#setup-page-does-not-appear">Setup page</a>
<a href="#login-expired">Login expired</a>
<a href="#model-is-unavailable">Model unavailable</a>
<a href="#drafting-is-slow">Drafting is slow</a>
<a href="#email-was-not-delivered">Email delivery</a>
<a href="#shared-change-is-missing">Shared changes</a>
<a href="#collect-diagnostics">Diagnostics</a>
</aside>
<article class="guide-content" markdown="1">

<div class="eyebrow">Help</div>
# Troubleshooting

## Setup page does not appear

If the deployment already contains an administrator, `/setup` redirects to login. To deliberately start over with a recoverable local-data backup:

```bash
./reset-local-data.sh
./start.sh
```

Then print the newly generated token and reopen setup:

```bash
awk -F= '$1=="INITIAL_ADMIN_SETUP_TOKEN" {print $2}' .env
open http://localhost:7860/setup
```

## Login expired

Sign in again at `/login`, then reopen the grant. Browser session cookies—not values pasted into browser storage—authorize application requests.

If this recurs immediately, inspect the UI and core logs for cookie, public URL, or reverse-proxy errors.

## Model is unavailable

Run startup again and read its provider diagnostics:

```bash
./start.sh
```

On macOS, confirm native Ollama responds and has the model named in `.env`. On Linux NVIDIA, confirm `nvidia-smi` works and the Ollama container is healthy. Hybrid and Claude-only deployments must have a nonblank Claude key.

## Drafting is slow

An M2 with 8 GB uses a small local model and can take substantially longer than an M4, NVIDIA GPU, or Claude route. Drafting proceeds through multiple bounded calls; the current operation should report a named stage or a concrete error.

Do not repeatedly click **Create shared grant** while a request is active. If the stage does not change and no model traffic appears in logs, collect diagnostics below.

## Email was not delivered

Check all SMTP values and restart. A missing `SMTP_HOST` disables delivery. A success result only confirms that the relay accepted the message.

For a development mailbox, point Grantspace at an SMTP capture service such as Mailpit and inspect its web inbox. For real delivery, use an authenticated organizational relay and a reachable `APP_PUBLIC_URL`.

## Shared change is missing

Choose **Refresh shared changes**. The application does not currently push character-level updates to other browsers. If your save is stale, load the latest version and reconcile rather than overwriting it.

## Collect diagnostics

```bash
docker compose ps
docker compose logs --tail=200 ui core renderer ingestion embedding-cpu
```

For macOS model problems, also capture:

```bash
curl -sS http://127.0.0.1:11434/api/tags
```

Remove passwords, API keys, setup tokens, invitation links, and proposal content before sharing logs.

</article>
</div>


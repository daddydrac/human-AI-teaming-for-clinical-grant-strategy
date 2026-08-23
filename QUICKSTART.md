# Quickstart

## M4 with Claude + OLMo 3

Add the Anthropic key in `env.m4Mac.txt`, then:

```bash
brew install uv
cp env.m4Mac.txt .env
./install.sh
./start.sh
```

## M4 with Claude + Qwen 3

Add the Anthropic key in `env.m4Mac.qwen3.txt`, then:

```bash
brew install uv
cp env.m4Mac.qwen3.txt .env
./install.sh
./start.sh
```

## M2 with 8 GB, local only

Install Ollama for macOS from <https://ollama.com/download/mac>, then:

```bash
cp env.m2Mac.8gb.txt .env
./install.sh
./start.sh
```

The first start downloads `qwen3:1.7b`. For higher-stakes sponsor analysis and
scientific review on the M2, switch the same template to `MODEL_ROUTING_MODE=hybrid`,
set `REQUIRE_CLAUDE_IN_HYBRID=true`, and provide `ANTHROPIC_API_KEY`.

## Shared enterprise server with OIDC

Register an OIDC client whose callback is
`https://YOUR_HOST:8443/oauth2/callback`. Copy `.env.example` to `.env` and set
the `OIDC_*` values, including an explicit email-domain allowlist, an immutable
subject claim (normally `sub`), a stable organization ID, the client-secret file,
and institution-issued TLS certificate/key files. Then run:

```bash
./scripts/start_oidc_gateway.sh
```

This profile publishes only the TLS gateway. Core, UI, renderer, ingestion, and
embedding ports remain private. The startup preflight validates OIDC discovery,
PKCE support, secret files, the certificate/key pair, and the resolved Compose
isolation policy before starting services. It generates the OAuth session secret
and the internal gateway-proof secret on first use when their configured paths do
not yet exist.

For a SAML-based institutional gateway, deploy that gateway in place of OAuth2
Proxy and implement the trusted-header contract in
`dev_docs/ADR_003_IDENTITY_AND_SHARED_DEPLOYMENT.md`.

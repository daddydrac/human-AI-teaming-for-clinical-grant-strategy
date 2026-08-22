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

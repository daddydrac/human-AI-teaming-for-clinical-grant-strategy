# Clinical Grant Workbench

Clinical Grant Workbench is a local, human-in-the-loop application for developing sponsor-ready grant proposals. It turns a funding opportunity and supporting project materials into structured requirements, evidence, clinical-study design, competitive intelligence, reviewed grant sections, and a final DOCX/PDF submission package. The app runs as a Gradio UI backed by containerized Rust and Python services; supported Apple Silicon Macs can use local MLX inference, while other Macs use Claude with local CPU embeddings.

## UI workflow

- **1 · Intake & Requirements:** Create a project, provide the funding opportunity by searchable PDF/DOCX/TXT/HTML upload, public URL, or pasted text, add supporting and branding files, review the parsed requirements, and approve them.
- **2 · Investigator Interview:** Generate questions for missing information, record answers with confidence and provenance, and continue until the project context is complete.
- **3 · Research & Evidence:** Run online evidence research, build or refresh the local hybrid knowledge index, and test retrieval results.
- **4 · Clinical Study Design:** Define the study, aims, arms, endpoints, timeline, resources, recruitment assumptions, and statistical parameters; run feasibility and sample-size checks.
- **5 · Competitive Applicant Intelligence:** Build a likely strong-applicant profile, inspect capability-matched organizations and public evidence, and generate differentiating strategy.
- **6 · Sponsor Compliance & Submission:** Compile deterministic sponsor rules, correct and approve the compliance profile, register required attachments, resolve human-confirmation items, and run rendered preflight checks.
- **7 · Write, Edit & Approve:** Compile context and draft each section, optionally escalate a section to Claude, edit the result, and explicitly approve the exact version intended for submission.
- **8 · Final Export:** Preview the approved grant, pass all readiness gates, and export a DOCX, PDF, or both with the sponsor-compliant submission package.
- **9 · System & Diagnostics:** Inspect non-secret runtime/build information and run the local HPC benchmark.

Public opportunity URLs are rendered in Chromium and converted to Markdown before storage; pasted text is stored without trimming. In both cases that stored per-document text is the authoritative citation buffer. Claude returns only rule meaning and source hints. Rust locates a passage and copies its exact UTF-8 byte slice into provenance fields, or records `SOURCE NOT LOCATED` and requires human review—model-generated excerpts are not accepted.

## Quick start

### Prerequisites

- macOS with at least 10 GB of free disk space
- Docker Desktop installed and running
- `uv` installed when using the native Apple MLX profile (automatically selected on Apple Silicon Macs with at least 16 GB RAM)
- An Anthropic API key on Intel or lower-memory Macs, where the automatic runtime profile uses Claude for generation

### Install and run

```bash
cp env.m4Mac.txt .env
```

Edit `.env` before starting:

- Set `ANTHROPIC_API_KEY`; the M4 hybrid profile requires both OLMo and Claude.
- Optionally set `BRAVE_SEARCH_API_KEY` for online evidence and patent/technology research.
- Optionally set `OPENALEX_API_KEY` for publication discovery.
- Adjust `ORGANIZATION_NAME`, `GRANT_SECTIONS`, and `GRANT_EXPORT_HOME` as needed.

Then bootstrap and start the application:

```bash
./install.sh
./start.sh
```

Open [http://localhost:7860](http://localhost:7860). The first startup may take longer while containers and local models are downloaded.

### Common commands

```bash
# Show service status and recent logs
docker compose ps
docker compose logs -f

# Rebuild images after source or dependency changes
REBUILD=1 ./start.sh

# Check the installation and service health
./scripts/doctor.sh
./scripts/smoke_test.sh

# Stop the application
./stop.sh
```

# Grant docs final output
Generated documents are written to `./exports` by default, as .docx, .pdf or both. Project data is retained in the Docker `grant-data` volume when the application is stopped.

---

# .env file instructions

This guide explains the `.env` configuration: what each variable controls, what value to use, which settings you normally need to change, and where to obtain external credentials.

---

## 1. Create the `.env` File

From the project root:

```bash
cp .env.example .env
```

Then open `.env` in your editor.

> **Important:** Never commit `.env` to Git. It contains API credentials and machine-specific configuration.

For most installations, you only need to manually configure:

```bash
ORGANIZATION_NAME="MD Anderson Cancer Center"

ANTHROPIC_API_KEY=
BRAVE_SEARCH_API_KEY=

OPENALEX_MAILTO=
OPENALEX_API_KEY=

GRANT_RUNTIME_PROFILE=auto
MODEL_ROUTING_MODE=hybrid

GRANT_EXPORT_HOME=./exports
```

Most other settings already have production defaults and should remain unchanged unless benchmarking or deployment requirements tell you otherwise.

---

# 2. Organization Configuration

## `ORGANIZATION_NAME`

```bash
ORGANIZATION_NAME="MD Anderson Cancer Center"
```

### What it does

Sets the institution name used in:

* grant metadata
* prompts
* document generation
* competitive-intelligence filtering
* default branding context

### Recommended value

For MD Anderson:

```bash
ORGANIZATION_NAME="MD Anderson Cancer Center"
```

If another institution deploys the system, replace it with that institution's official name.

---

## `GRANT_SECTIONS`

```bash
GRANT_SECTIONS="Specific Aims,Significance,Innovation,Approach,Human Subjects,Environment"
```

### What it does

Defines the default grant-writing sections before the funding opportunity is parsed.

### Recommended value

Leave unchanged:

```bash
GRANT_SECTIONS="Specific Aims,Significance,Innovation,Approach,Human Subjects,Environment"
```

The Sponsor Compliance engine can dynamically add additional required sections after parsing the actual funding opportunity.

For example, if the opportunity requires:

```text
Commercialization Plan
Data Management Plan
Community Engagement Plan
```

those can be added automatically.

---

## `GRANT_WRITER_HOME`

```bash
GRANT_WRITER_HOME=./workspace
```

### What it does

Defines the application workspace used by local tooling.

### Recommended value

```bash
GRANT_WRITER_HOME=./workspace
```

Production Docker deployments keep performance-sensitive SQLite, MMAP, Parquet, BM25 and CSR data inside Docker named volumes.

---

# 3. Anthropic / Claude

## `ANTHROPIC_API_KEY`

```bash
ANTHROPIC_API_KEY=
```

### What it does

Allows the system to call Claude for:

* complex scientific synthesis
* grant writing
* difficult reasoning
* competitive positioning
* research synthesis
* other high-value model tasks

### Is it required?

For an **Intel or low-memory Mac**, yes.

The weak-Mac architecture does not attempt to run OLMo 3 7B locally. It keeps the Rust/HPC/retrieval system local and sends generation tasks to Claude.

For an **Apple Silicon Mac**, it is strongly recommended.

The system can use:

```text
OLMo 3 locally through MLX
+
Claude selectively for higher-value tasks
```

### Where to get it

Go to:

`https://console.anthropic.com/`

Then:

1. Sign in or create an Anthropic account.
2. Open the API key section.
3. Create an API key.
4. Copy the key.
5. Store it securely.

Example:

```bash
ANTHROPIC_API_KEY=sk-ant-your-real-key-here
```

Never place this key in:

* Git
* screenshots
* documentation
* browser JavaScript
* generated grant documents

---

## `CLAUDE_MODEL`

```bash
CLAUDE_MODEL=claude-sonnet-4-5
```

### What it does

Selects the Claude model used by the current Phase 8 release.

### Recommended

Leave the tested release value:

```bash
CLAUDE_MODEL=claude-sonnet-4-5
```

Changing models can affect output quality and reproducibility.

Treat a model change as a deployment/release configuration change.

---

## `CLAUDE_MAX_TOKENS`

```bash
CLAUDE_MAX_TOKENS=6000
```

### What it does

Maximum number of tokens Claude can generate for one request.

### Recommended

```bash
CLAUDE_MAX_TOKENS=6000
```

The context compiler already limits what evidence and project information is sent to the model.

---

## `CLAUDE_TASK_KINDS`

```bash
CLAUDE_TASK_KINDS=competitive_positioning,complex_scientific_synthesis
```

### What it does

Identifies tasks that should preferentially use Claude when the system is running in hybrid mode.

### Recommended

```bash
CLAUDE_TASK_KINDS=competitive_positioning,complex_scientific_synthesis
```

Routine work can remain local on Apple Silicon while Claude handles more difficult reasoning.

---

# 4. Brave Search API

## `BRAVE_SEARCH_API_KEY`

```bash
BRAVE_SEARCH_API_KEY=
```

### What it does

Enables public web research for:

* grant research
* evidence discovery
* competitor research
* public technology discovery
* patent/IP discovery
* organizational capability research

### Where to get it

Go to:

`https://api-dashboard.search.brave.com/`

Then:

1. Create or sign into a Brave Search API account.
2. Select an appropriate API plan.
3. Create an API key/subscription token.
4. Copy the token.

Example:

```bash
BRAVE_SEARCH_API_KEY=your-real-brave-search-token
```

---

## `BRAVE_SEARCH_ENDPOINT`

```bash
BRAVE_SEARCH_ENDPOINT=https://api.search.brave.com/res/v1/web/search
```

### What it does

Defines the Brave Web Search API endpoint.

### Recommended

Do not change it:

```bash
BRAVE_SEARCH_ENDPOINT=https://api.search.brave.com/res/v1/web/search
```

---

# 5. OpenAlex

OpenAlex is used by the Competitive Applicant Intelligence Engine for public scholarly and publication intelligence.

## `OPENALEX_API_KEY`

```bash
OPENALEX_API_KEY=
```

### What it does

Authenticates requests to OpenAlex.

The competitive-intelligence engine can use OpenAlex to investigate:

* relevant publications
* institutional research activity
* investigator research activity
* scientific capability signals
* competing organizations

### Where to get it

Go to:

`https://openalex.org/settings/api`

Sign into OpenAlex and obtain your API credentials.

Then:

```bash
OPENALEX_API_KEY=your-openalex-api-key
```

---

## `OPENALEX_MAILTO`

```bash
OPENALEX_MAILTO=
```

### What it does

Identifies the application/operator making OpenAlex requests.

### Recommended

Use an approved institutional or service email address.

Example:

```bash
OPENALEX_MAILTO=grant-ai-service@example.org
```

For enterprise use, prefer a service/team account instead of an individual's personal email.

---

# 6. OLMo 3 on Apple MLX

These settings apply primarily to supported Apple-Silicon Macs.

Intel Macs automatically use the CPU/Claude deployment profile instead.

---

## `OLMO_MODEL_REPO`

```bash
OLMO_MODEL_REPO=mlx-community/Olmo-3-7B-Instruct-4bit
```

### What it does

Specifies the MLX-compatible OLMo model repository.

### Recommended

```bash
OLMO_MODEL_REPO=mlx-community/Olmo-3-7B-Instruct-4bit
```

---

## `OLMO_API_MODEL`

```bash
OLMO_API_MODEL=grant-olmo
```

### What it does

Provides a stable internal API alias for the local OLMo model.

The application calls:

```text
grant-olmo
```

rather than coupling the Rust backend directly to a Hugging Face repository name.

### Recommended

Leave unchanged:

```bash
OLMO_API_MODEL=grant-olmo
```

---

## `OLMO_MODEL_REVISION`

```bash
OLMO_MODEL_REVISION=
```

### What it does

Optionally pins OLMo to an exact immutable Hugging Face Git revision.

### Development

Leave blank:

```bash
OLMO_MODEL_REVISION=
```

The startup process resolves and records the actual repository SHA.

### Certified production release

Use the exact model revision that passed validation:

```bash
OLMO_MODEL_REVISION=<tested-hugging-face-commit-sha>
```

---

## `VLLM_MLX_VERSION`

```bash
VLLM_MLX_VERSION=0.4.1
```

### What it does

Pins the native Apple MLX model-serving runtime.

### Recommended

```bash
VLLM_MLX_VERSION=0.4.1
```

Only upgrade this after regression testing.

---

## `MODEL_HTTP_TIMEOUT_SECONDS`

```bash
MODEL_HTTP_TIMEOUT_SECONDS=300
```

### What it does

Maximum HTTP request time allowed for model generation.

### Recommended

```bash
MODEL_HTTP_TIMEOUT_SECONDS=300
```

---

## `OLMO_MAX_TOKENS`

```bash
OLMO_MAX_TOKENS=4096
```

### What it does

Maximum local OLMo output size.

### Recommended

```bash
OLMO_MAX_TOKENS=4096
```

---

# 7. Apple-Silicon Embedding Model

## `EMBEDDING_MODEL_REPO`

```bash
EMBEDDING_MODEL_REPO=mlx-community/all-MiniLM-L6-v2-4bit
```

### What it does

Defines the local MLX embedding model used for semantic retrieval.

### Recommended

```bash
EMBEDDING_MODEL_REPO=mlx-community/all-MiniLM-L6-v2-4bit
```

Do not change the embedding model casually because doing so changes the semantic vector space and requires index rebuilding/recalibration.

---

## `EMBEDDING_API_MODEL`

```bash
EMBEDDING_API_MODEL=grant-embedding
```

Stable API alias for the embedding service.

### Recommended

```bash
EMBEDDING_API_MODEL=grant-embedding
```

---

## `EMBEDDING_MODEL_REVISION`

```bash
EMBEDDING_MODEL_REVISION=
```

Optional exact Hugging Face model revision.

During development:

```bash
EMBEDDING_MODEL_REVISION=
```

For a certified production build:

```bash
EMBEDDING_MODEL_REVISION=<tested-commit-sha>
```

---

## `EMBEDDING_URL`

```bash
EMBEDDING_URL=http://host.docker.internal:8000/v1/embeddings
```

### What it does

Allows the Dockerized Rust backend to communicate with the native macOS MLX embedding server.

### Recommended

Leave unchanged.

The weak-Mac profile automatically switches to the Docker CPU embedding service.

---

## `EMBEDDING_BATCH_SIZE`

```bash
EMBEDDING_BATCH_SIZE=64
```

### What it does

Number of text items sent to the embedding service per request.

### Recommended

Leave the default.

Weak-Mac tuning automatically lowers the effective batch size.

---

## `EMBEDDING_HTTP_TIMEOUT_SECONDS`

```bash
EMBEDDING_HTTP_TIMEOUT_SECONDS=120
```

Maximum embedding request duration.

### Recommended

```bash
EMBEDDING_HTTP_TIMEOUT_SECONDS=120
```

---

## `EMBEDDING_HEALTH_TTL_SECONDS`

```bash
EMBEDDING_HEALTH_TTL_SECONDS=600
```

### What it does

Caches embedding service health for 10 minutes.

This prevents Docker health checks from repeatedly invoking the embedding model.

### Recommended

```bash
EMBEDDING_HEALTH_TTL_SECONDS=600
```

---

## `INDEX_EMBED_BATCH_RECORDS`

```bash
INDEX_EMBED_BATCH_RECORDS=64
```

### What it does

Number of records processed per embedding/index batch.

### Recommended

Leave unchanged.

Weak-Mac mode automatically reduces it.

---

# 8. Automatic Hardware Selection

## `GRANT_RUNTIME_PROFILE`

```bash
GRANT_RUNTIME_PROFILE=auto
```

### What it does

Tells `start.sh` to detect the Mac and choose the correct architecture automatically.

### Recommended

Always start with:

```bash
GRANT_RUNTIME_PROFILE=auto
```

Typical behavior:

```text
Apple Silicon + sufficient unified memory
        ↓
apple_mlx

Intel Mac / low-memory Mac
        ↓
docker_cpu
```

Manual overrides are available:

```bash
GRANT_RUNTIME_PROFILE=apple_mlx
```

or:

```bash
GRANT_RUNTIME_PROFILE=docker_cpu
```

Only override automatic selection for troubleshooting or controlled deployments.

---

## `MODEL_ROUTING_MODE`

```bash
MODEL_ROUTING_MODE=hybrid
```

### What it does

Controls how generation is routed between local OLMo and Claude.

### Recommended

```bash
MODEL_ROUTING_MODE=hybrid
```

Effective behavior:

```text
Apple Silicon
─────────────
OLMo locally
+
Claude escalation

Intel / weak Mac
────────────────
Claude generation
+
local HPC retrieval
```

---

# 9. Weak-Mac CPU Embeddings

These settings support older Intel and low-memory Macs.

## `CPU_EMBEDDING_MODEL`

```bash
CPU_EMBEDDING_MODEL=BAAI/bge-small-en-v1.5
```

### What it does

Small CPU-efficient embedding model used by the Docker FastEmbed/ONNX service.

### Recommended

```bash
CPU_EMBEDDING_MODEL=BAAI/bge-small-en-v1.5
```

---

## `CPU_EMBEDDING_API_MODEL`

```bash
CPU_EMBEDDING_API_MODEL=grant-embedding-cpu
```

Stable internal API name.

### Recommended

Leave unchanged.

---

## `CPU_EMBEDDING_THREADS`

```bash
CPU_EMBEDDING_THREADS=2
```

Number of CPU threads used for embeddings on weak Macs.

### Recommended

```bash
CPU_EMBEDDING_THREADS=2
```

The runtime tuner may override this depending on detected hardware.

---

## `CPU_EMBEDDING_BATCH_SIZE`

```bash
CPU_EMBEDDING_BATCH_SIZE=8
```

Conservative embedding batch size for weak Macs.

### Recommended

```bash
CPU_EMBEDDING_BATCH_SIZE=8
```

---

# 10. HPC Parallelism

## `OMP_NUM_THREADS`

```bash
OMP_NUM_THREADS=4
```

Controls OpenMP parallelism used by the native C++ HPC kernels.

---

## `RAYON_NUM_THREADS`

```bash
RAYON_NUM_THREADS=4
```

Controls Rust Rayon CPU parallelism.

---

## `OPENBLAS_NUM_THREADS`

```bash
OPENBLAS_NUM_THREADS=1
```

Controls OpenBLAS internal threading.

### Important

Keep BLAS at:

```bash
OPENBLAS_NUM_THREADS=1
```

The application intentionally uses OpenMP and Rayon for outer parallelism.

You do **not** want:

```text
OpenMP 8
× Rayon 8
× BLAS 8
```

creating dozens of competing threads.

That can make the application substantially slower.

The Mac runtime tuner adjusts the effective OpenMP/Rayon settings automatically.

---

# 11. Research Concurrency

## `RESEARCH_MAX_CONCURRENCY`

```bash
RESEARCH_MAX_CONCURRENCY=8
```

Maximum number of concurrent public research jobs.

Weak-Mac mode automatically reduces this.

---

## `RESEARCH_HTTP_TIMEOUT_SECONDS`

```bash
RESEARCH_HTTP_TIMEOUT_SECONDS=30
```

Maximum normal public-research HTTP request duration.

### Recommended

```bash
RESEARCH_HTTP_TIMEOUT_SECONDS=30
```

---

## `RESEARCH_MAX_BODY_BYTES`

```bash
RESEARCH_MAX_BODY_BYTES=8388608
```

Maximum permitted public-web response body.

`8,388,608 bytes = 8 MiB`

This prevents unexpectedly large downloads from consuming excessive memory.

### Recommended

Leave unchanged.

---

# 12. Document Chunking

## `DOCUMENT_CHUNK_WORDS`

```bash
DOCUMENT_CHUNK_WORDS=420
```

Approximate words per indexed document chunk.

---

## `DOCUMENT_CHUNK_OVERLAP_WORDS`

```bash
DOCUMENT_CHUNK_OVERLAP_WORDS=64
```

Number of overlapping words between neighboring chunks.

Overlap reduces the chance of losing meaning when relevant information crosses a chunk boundary.

---

# 13. Context Compilation

## `CONTEXT_RETRIEVAL_K`

```bash
CONTEXT_RETRIEVAL_K=24
```

Maximum number of high-value retrieval records normally supplied to the model.

---

## `CONTEXT_MAX_CHARS`

```bash
CONTEXT_MAX_CHARS=48000
```

Maximum retrieved-context character budget.

This is important because the system does **not** dump the entire grant repository into every model prompt.

Weak-Mac mode automatically reduces these values.

---

# 14. Hybrid Retrieval Weights

```bash
RETRIEVAL_WEIGHT_SEMANTIC=0.45
RETRIEVAL_WEIGHT_LEXICAL=0.25
RETRIEVAL_WEIGHT_EVIDENCE=0.20
RETRIEVAL_WEIGHT_FRESHNESS=0.10
```

These control the hybrid retrieval score.

The weights total:

```text
0.45 + 0.25 + 0.20 + 0.10 = 1.00
```

### Semantic — 45%

Embedding similarity.

### Lexical — 25%

BM25 keyword/text similarity.

### Evidence — 20%

Evidence quality/confidence.

### Freshness — 10%

How recent the evidence is.

### Recommended

Leave these values unchanged until retrieval-quality benchmarking provides evidence to change them.

---

## `RETRIEVAL_GRAPH_BOOST`

```bash
RETRIEVAL_GRAPH_BOOST=0.08
```

Additional score for information connected through the requirement/evidence CSR graph.

---

## `RETRIEVAL_CANDIDATE_MULTIPLIER`

```bash
RETRIEVAL_CANDIDATE_MULTIPLIER=4
```

Expands candidate retrieval before final reranking.

---

## `RETRIEVAL_OPENMP_THRESHOLD`

```bash
RETRIEVAL_OPENMP_THRESHOLD=4096
```

Minimum workload size where native OpenMP processing becomes worthwhile.

---

## `RETRIEVAL_FRESHNESS_HALF_LIFE_DAYS`

```bash
RETRIEVAL_FRESHNESS_HALF_LIFE_DAYS=365
```

Controls time decay for freshness scoring.

---

# 15. BM25

## `BM25_K1`

```bash
BM25_K1=1.2
```

Controls BM25 term-frequency saturation.

---

## `BM25_B`

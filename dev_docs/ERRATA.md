# Research Clinic Grant Agent — Phase 7 Sponsor Compliance + Submission Package

Local-first grant-development workbench with a Docker-managed Gradio UI and Rust/C++ HPC data plane. The runtime auto-selects the fastest safe model path for the Mac:

- **Apple Silicon with >=16 GB memory:** native macOS MLX/vLLM-MLX serves OLMo 3 7B Instruct and the local embedding model; Claude is used selectively for high-value tasks.
- **Intel Macs or low-memory Macs:** every local service runs in Docker, dense embeddings use a lightweight ONNX/FastEmbed CPU service, and generation routes to Claude so an 8 GB Mac is not forced to page a 7B model.

The grant workflow, evidence model, HITL approvals, retrieval index, and final renderer are identical in both modes.

## Human-approved authoring rule

The authoring workflow is intentionally simple and human-controlled:

```text
compiled grant context
        ↓
OLMo / Claude produces section text
        ↓
formatted section preview
        ↓
Human chooses one:
   ✓ approve exact draft
   ✎ edit → save → approve exact edit
        ↓
approved section ledger
        ↓
live approved-grant preview
        ↓
all required sections approved
        ↓
DOCX / PDF / BOTH
```

There is **no claim-by-claim approval gate** in the writing workflow. Research, evidence retrieval, and citation data can improve the draft context, but the unit of human control is the section version. The final document compiler consumes only exact human-approved section versions, in configured document order. Unapproved AI drafts and unsaved edits are excluded.

The Final Export tab exposes a live assembled preview from `/api/projects/{id}/approved-document`, so the investigator can see exactly which approved versions will become the final grant before choosing DOCX, PDF, or both.



## Phase 7 sponsor compliance + submission package compiler

Phase 7 turns the funding opportunity into a **versioned, human-approved deterministic submission profile** and validates the final proposal/package against it. Hard sponsor requirements control export readiness; model opinions do not.

### ChatGPT-style grant-opportunity intake

The Gradio intake accepts the opportunity in any of three interchangeable ways:

1. **Upload** a searchable PDF, DOCX, TXT, Markdown, HTML, CSV, JSON, or XML file. DOCX extraction preserves paragraphs, tables, headers, and footers; a PDF with no usable text layer fails visibly instead of silently losing requirements.
2. **URL** to a public opportunity. URL ingestion is performed by the Rust secure fetcher, which rejects loopback/private/link-local destinations and disables unsafe redirect behavior.
3. **Paste Text** directly into the large Grant Opportunity editor.

All three inputs are persisted as `funding_*` sources and normalized into the same opportunity corpus. The UI shows that normalized source beside the extracted rules so a human can verify and correct normalization before approval. Supporting project/institutional documents remain separate and cannot silently become sponsor rules.

### Deterministic sponsor rules

The model is used only to **extract and normalize** explicit instructions from the opportunity. Every extracted rule must carry a source excerpt that can be verified verbatim against the stored opportunity text. The Rust compliance engine then evaluates the normalized rules deterministically. Supported rule classes include:

- required narrative sections;
- maximum/minimum word counts;
- required attachments and required letter counts;
- allowed attachment extensions;
- minimum body-font size and margins;
- maximum rendered page count;
- explicit deadlines;
- submission-system metadata;
- maximum project period when the structured clinical timeline provides an authoritative duration;
- budget/manual rules that require explicit human confirmation when the current deterministic project model does not contain an authoritative value.

Rules are versioned and SHA-256 hashed. A human can correct the structured rules in Gradio, but source excerpts must still exist in the funding opportunity. The corrected profile must then be explicitly approved. Sponsor-required sections discovered by the profile are synchronized into the writing plan automatically.

### Rendered preflight and fail-closed export

The compliance engine evaluates the exact human-approved sections, approved design profile, registered attachments, and current clinical timeline. The renderer measures the same shared Document AST used for final PDF/DOCX generation, so page-count checks operate on the actual styled proposal rather than an LLM estimate. The measurement fingerprint includes approved prose, the design-profile SHA, and clinical-study SHA, so changing text, margins/fonts, or the study timeline automatically invalidates stale measurements.

A hard mandatory sponsor rule that is failed or still requires authoritative confirmation keeps `readiness.ready=false`. The user sees the specific rule, source excerpt, observed value, and resolution state in **Sponsor Compliance & Submission**. Rules that cannot be machine-proven may be resolved explicitly as satisfied, not applicable, or waived with a human rationale.

### Registered submission artifacts and deterministic package ZIP

Attachments are copied into the project submission workspace, SHA-256 verified by the Rust backend, and registered under stable slots such as `letters_of_support`, `biosketches`, or `data_management_plan`. Final package generation includes **only artifacts recorded in the immutable export snapshot**; merely placing an extra file in the workspace cannot make it into the package. Checksums are verified again during packaging.

After all ordinary workflow gates and hard sponsor rules pass, the user still chooses **DOCX, PDF, or BOTH**. The backend freezes the exact approved section versions, design profile, clinical study, current competitive intelligence, compliance profile/assessment, and registered artifacts into one immutable export snapshot. The renderer produces the requested proposal file(s) and a submission package ZIP containing:

```text
proposal/
  <approved DOCX/PDF>
attachments/
  <slot>/<registered file>
submission_manifest.json
```

### Phase 7 API surface

- `GET/POST /api/projects/{id}/compliance`
- `POST /api/projects/{id}/compliance/compile`
- `POST /api/projects/{id}/compliance/approve`
- `POST /api/projects/{id}/compliance/resolve`
- `POST /api/projects/{id}/compliance/measurements`
- `GET /api/projects/{id}/compliance/assessment`
- `GET/POST /api/projects/{id}/submission-artifacts`
- `GET /api/projects/{id}/opportunity-source`
- renderer `POST /measure`
- renderer `POST /package`

The Phase 6 competitive self-healing cadence is now **4 hours** end-to-end by default:

```bash
COMPETITIVE_REFRESH_TTL_SECONDS=14400
COMPETITIVE_BACKGROUND_REFRESH_ENABLED=true
COMPETITIVE_BACKGROUND_REFRESH_SECONDS=14400
COMPETITIVE_UI_POLL_SECONDS=14400
COMPETITIVE_UPDATE_LABEL="Competitive Edge Auto-Update"
```

The weak-Mac profile is unchanged: compliance evaluation is Rust/SQLite/renderer work and adds no resident 7B model or heavyweight service.

## Phase 5 structured clinical-study model

Phase 5 adds one versioned, authoritative clinical-study object per grant. It is persisted with a SHA-256 digest and immutable history so every later draft/export can be traced to the exact study assumptions used. The model includes the clinical problem, knowledge gap, central hypothesis, population, study design, study arms, recruitment assumptions, deterministic statistics inputs, Specific Aims, endpoints, timeline dependencies, and required resources.

The backend now exposes:

- `GET/POST /api/projects/{id}/clinical-study`
- `GET /api/projects/{id}/clinical-assessment`
- `POST /api/projects/{id}/clinical/sample-size`
- `POST /api/projects/{id}/clinical/scenarios`

The clinical model is included in the project retrieval fingerprint, in the MMAP/Parquet retrieval corpus, in every compiled drafting context, and in the immutable final export snapshot. Section drafting/saving/approval is rejected until a structured clinical model exists.

Deterministic Rust checks calculate recruitment throughput and accrual duration, calculate initial sample sizes for two proportions, one proportion, two independent means, and equal-allocation log-rank designs, validate endpoint-type/analysis-family compatibility, validate Aim→Endpoint references, verify timeline dependencies, identify missing required resources, and compare selected authoritative numeric facts against already approved prose. These checks never rewrite approved text.

The Gradio UI includes a **Clinical Study Design** workspace with structured fields plus editable Aim, Study Arm, Endpoint, Timeline, and Resource tables. A Rayon-parallel scenario sweep compares combinations of site counts, consent rates, and biomarker prevalence without using an LLM for arithmetic. The authoring workflow remains section-level only: AI writes text, the human edits or approves that exact section version, and only approved versions are aggregated into the final DOCX/PDF.


## Phase 6 competitive applicant intelligence

Phase 6 adds a versioned, public-evidence **Competitive Applicant Intelligence Engine**. It does not claim to know who will apply. Instead, it derives the observable capability profile of a strong plausible applicant from the approved grant requirements, investigator interview, evidence corpus, and authoritative Phase 5 clinical-study design; discovers public organizations whose visible work overlaps that profile; and turns those public signals into evidence-bounded proposal positioning.

The engine uses a two-pass workflow:

1. **Discovery:** NIH RePORTER prior awards, ClinicalTrials.gov studies, and OpenAlex publications identify capability-matched organizations. The deploying organization and configured aliases are excluded.
2. **Enrichment:** the strongest candidates are enriched with bounded public patent/IP and technology/licensing/partnership search signals. IP web-search results are treated as discovery signals, not proof of ownership or exclusive rights unless the public source itself establishes that fact.
3. **HPC relevance scoring:** the existing embedding service generates normalized vectors, OpenMP normalizes the dense matrix, and BLAS SGEMV scores every public asset against the grant-specific capability profile. Deterministic configured weights aggregate prior grants, publications, clinical trials, public IP signals, disclosed technology, capability breadth, and dimension coverage into a ranked candidate profile.
4. **Positioning synthesis:** OLMo or Claude receives only the bounded public evidence packet plus the project's own authoritative context and returns structured differentiators, gaps to close, candidate notes, claims to avoid, and section-specific positioning guidance. Every competitor-specific strategic statement is constrained to supplied candidate/asset keys.

The backend exposes:

- `GET /api/projects/{id}/competitive/profile`
- `POST /api/projects/{id}/competitive/profile/generate`
- `GET /api/projects/{id}/competitive`
- `POST /api/projects/{id}/competitive/run`

Competitive profiles and runs are versioned and hashed. Each run records the input fingerprint, profile version, competitive configuration SHA-256, provider status, ranked candidates, all persisted public assets, strategy model, strategy SHA-256, and completion state. If source documents, requirements, investigator answers, evidence, the clinical design, competitive configuration, or the public-intelligence TTL changes freshness state, the backend **self-heals automatically**: it rebuilds the profile when needed, re-queries the public providers, rescoring the current evidence before any downstream drafting/readiness/export operation proceeds.

### ⚡ Competitive Edge Auto-Update

Phase 6 continuously protects an in-progress grant from becoming competitively stale while the Docker stack is running. `COMPETITIVE_REFRESH_TTL_SECONDS` controls how old public intelligence may become before a new public scan is due. The Gradio client polls cheap status with `COMPETITIVE_UI_POLL_SECONDS`, while an independent Rust background loop (`COMPETITIVE_BACKGROUND_REFRESH_ENABLED`, `COMPETITIVE_BACKGROUND_REFRESH_SECONDS`) also checks eligible projects even if the browser is closed. Expensive public research still runs only when the TTL/input/config freshness checks say a refresh is needed.

When a refresh finds **new or materially changed public competitor evidence**—for example a newly surfaced award, publication, clinical trial, patent/IP signal, technology capability, new capability-matched organization, or a material score shift—the engine recomputes the evidence-bounded positioning strategy and proposes revised text for affected existing grant sections. A strategy model merely changing its wording without an observable public-data delta does **not** trigger automatic grant rewrites. If a public provider is degraded, missing/downward signals from that refresh are suppressed so transient API failures cannot make the grant chase false competitor disappearances.

The investigator is notified in Gradio with a configurable `COMPETITIVE_UPDATE_LABEL` (default **Competitive Edge Auto-Update**). The affected sections are listed, and opening one renders an in-page word diff: yellow highlighting marks additions/revisions and red strikethrough marks proposed removals. If the investigator edits the proposal, the preview clearly states that the highlighted diff now includes both the automated competitive proposal and subsequent human edits.

**Human-approved prose is never silently overwritten.** The previously approved version remains the authoritative export version. Automatic refresh creates a new unapproved version linked to the competitive update event. The user can edit it, approve it unchanged, or deliberately keep/reapprove earlier language. Only explicit human approval changes the version that is eligible for final DOCX/PDF aggregation. Export remains fail-closed while competitive text reconciliation or human review is pending.

The Gradio UI also has a dedicated **Competitive Applicant Intelligence** workspace that shows the generated strong-applicant capability profile, ranked capability-matched organizations, provider health, top public assets, evidence-backed differentiators, gaps to close, and section-specific positioning guidance. The writing context compiler injects only fresh competitive intelligence and labels all organizations as potential/capability-matched rather than confirmed applicants.

Public provider and scoring behavior is externalized in `config/competitive_intelligence.json`, including result caps, enrichment concurrency, rate limits, provider toggles, relevance thresholds, asset-type weights, and public-IP search domains.

## Implemented production path

The current code executes this real workflow:

1. Create a project and ingest a funding opportunity from PDF/DOCX/text/HTML or URL.
2. Ingest supporting institutional/project documents and design references.
3. Decompose the funding source into typed atomic requirements using OLMo; parse and persist strict JSON rather than keeping a prose blob.
4. Require explicit human approval of the parsed requirements.
5. Generate a dynamic investigator interview from unresolved gaps only. Questions are typed as text/integer/number/percentage/boolean/date/choice and map back to requirement IDs.
6. Persist each answer with confidence and provenance classification: verified fact, investigator estimate, assumption, or unknown.
7. Build a targeted external research plan from unresolved evidence needs.
8. Execute real Brave Search API queries when `BRAVE_SEARCH_API_KEY` is configured.
9. Fetch public source pages concurrently, reject localhost/private/link-local destinations, hash the normalized content, and persist source lineage.
10. Ask OLMo to classify each fetched source as supported, partially supported, contradicted, or irrelevant for the evidence need.
11. Mark a citation verified only if the proposed supporting excerpt exists exactly in the fetched page text. Non-exact excerpts remain candidate evidence.
12. Build and human-review a structured clinical-study model containing population, design, Specific Aims, endpoints, recruitment, statistics, timeline, and resources.
13. Run deterministic recruitment, sample-size, endpoint-analysis, timeline, resource, and cross-section consistency checks in Rust.
14. Generate a versioned likely-strong-applicant capability profile from the current grant and clinical design.
15. Discover capability-matched public organizations through NIH RePORTER, ClinicalTrials.gov, and OpenAlex; optionally enrich the strongest candidates with bounded public patent/IP and technology/licensing/partnership search signals.
16. Score public assets using the existing embedding → OpenMP normalization → BLAS SGEMV path, aggregate candidate capability scores deterministically, and synthesize evidence-bounded differentiation guidance.
17. Before drafting, automatically self-heal stale public competitive intelligence, reconcile any material new competitor evidence into highlighted section-update proposals, then compile grant requirements, the authoritative clinical model/assessment, current competitive intelligence, interview answers, evidence, citations, selected source material, and optional human notes on the Rust backend.
18. Route normal drafting/reasoning to local OLMo and explicitly escalated/high-value synthesis to Claude if configured.
19. Render the generated section as a page-style HTML/CSS preview in Gradio.
20. Allow pencil/edit, save each changed section as a new human version, or approve the generated draft unchanged. Approval always targets one exact version ID.
21. Aggregate only approved versions into a live full-grant preview in canonical section order.
22. Once all required sections, deterministic clinical consistency gates, and fresh competitive-intelligence gates pass, ask whether to create DOCX, PDF, or both; render from one immutable approved snapshot and write the artifacts to the local Mac export directory.

## Deployment architecture

### Apple Silicon performance profile

```text
macOS / Apple Silicon
  ├─ native vllm-mlx :8000
  │    ├─ OLMo 3 7B Instruct / Metal / unified memory
  │    └─ local embedding model / Metal
  └─ Docker Desktop
       ├─ ui       :7860  Gradio
       ├─ core     :8080  Rust + C++ HPC + SQLite WAL
       └─ renderer :8090  deterministic DOCX/PDF
```

### Weak/Intel Mac performance profile

```text
Docker Desktop
  ├─ embedding-cpu :8010  FastEmbed + ONNX Runtime
  ├─ core          :8080  Rust + MMAP + BM25 + CSR + OpenMP/BLAS/Rayon
  ├─ renderer      :8090  deterministic DOCX/PDF
  └─ ui            :7860  Gradio
         │
         └──── compact task context ────> Claude API
```

`start.sh` detects architecture, logical CPUs, and memory and writes `.runtime.env`. Intel Macs never attempt to launch MLX because MLX requires Apple Silicon. On an 8 GB Intel profile, CPU fan-out, embedding batch size, research concurrency, and context size are reduced automatically.

Hot SQLite/MMAP/BM25/Parquet/CSR project data lives in Docker named volumes so macOS bind-mount latency is not on the retrieval path. Only final artifacts are bind-mounted to `GRANT_EXPORT_HOME` (default `./exports`) so users can open DOCX/PDF files directly from Finder.

## HPC implementation

### MMAP

`core/src/vector_store.rs` stores normalized dense vectors as a fixed binary header plus contiguous float32 rows and maps the file with `memmap2`. Query scoring reads the mapped matrix directly rather than deserializing it into Python objects.

### OpenMP + SIMD

`hpc/hpc_kernels.cpp` uses OpenMP parallel loops and SIMD reductions for row normalization and fused numeric scoring. These kernels are compiled at `-O3 -ffast-math -march=native` inside the arm64 Docker build.

### BLAS

Exact dense embedding scoring uses `cblas_sgemv`. Embeddings are normalized once so cosine scoring becomes matrix-vector dot products. OpenBLAS is configured as a single-threaded inner kernel while Rayon/OpenMP own outer parallelism, avoiding nested oversubscription.

### Rayon

Rust Rayon performs parallel top-k reduction and is the preferred general-purpose CPU fan-out layer for chunk/evidence processing and numeric post-processing.

### Tokio

Tokio owns I/O concurrency. Research source fetches use bounded `buffer_unordered` concurrency rather than tying up CPU workers waiting on network I/O.

### Arrow + Parquet

Arrow is the intended in-memory columnar interchange layer and Parquet is the durable analytical layer for high-volume structured evidence data. Hot vector/index data remains in MMAP-friendly binary structures rather than being accessed randomly from compressed Parquet pages.

### SQLite WAL concurrency

Workflow/version/approval state uses SQLite WAL. The initial single process-wide connection mutex has been removed; operations open independent WAL connections with busy timeouts so concurrent readers are not serialized behind one global lock.

## Research security

The research fetcher rejects loopback, private, link-local, local-domain, and other non-public destinations after DNS resolution. HTTP redirects are disabled in the research client so a public URL cannot redirect the fetcher into a local/private destination. Search/fetch failures are isolated per source and do not discard successful evidence from the same run.

## Configuration

```bash
cp .env.example .env
```

For the complete public research + Phase 6 enrichment path, configure:

```bash
BRAVE_SEARCH_API_KEY=...
OPENALEX_API_KEY=...
```

`BRAVE_SEARCH_API_KEY` enables grant research plus public patent/IP and technology enrichment. `OPENALEX_API_KEY` enables publication discovery. NIH RePORTER and ClinicalTrials.gov discovery do not use those two keys. Provider failures are isolated and reported in the competitive run rather than being silently treated as evidence.

Optional cloud escalation:

```bash
ANTHROPIC_API_KEY=...
CLAUDE_MODEL=...
```

All model/provider names, endpoints, timeouts, research concurrency, and CPU thread counts are environment-configurable.

## Start

```bash
cp .env.example .env
```

Set `ANTHROPIC_API_KEY` if you want Claude escalation. It is mandatory on the weak/Intel Docker profile because local 7B inference is intentionally disabled there for speed and memory safety.

Then run one command:

```bash
./start.sh
```

`start.sh` performs hardware detection, runs preflight checks, starts native MLX automatically when the Apple-Silicon profile is selected, starts the correct Docker Compose profile, waits for readiness, and opens:

```text
http://localhost:7860
```

Use:

```bash
./stop.sh
```

Final files are written under:

```text
./exports/<project-id>/final/
```

For diagnostics:

```bash
./scripts/tune_mac.sh
cat .runtime.env
```

## Validation

Static validation available in any suitable build environment:

```bash
./scripts/validate.sh
```

After services and the MLX endpoint are running:

```bash
./scripts/smoke_test.sh
```

The final acceptance test is `docker compose build` and the smoke test on the target Apple Silicon Mac because this execution environment does not provide Docker or a Rust toolchain.

## Export invariant

The final renderer never consumes the latest draft implicitly. The backend records an explicit approved version per section, `/api/projects/{id}/approved-document` assembles those versions in configured document order for preview, and export freezes the same approved content into an immutable snapshot. Model-generated drafts, human edits, and approvals remain separate events. A newer AI draft or unsaved edit cannot silently enter the DOCX/PDF.

## Phase 3: compiled hybrid retrieval runtime

The runtime now builds a project-local immutable retrieval snapshot from approved requirements, uploaded document chunks, investigator answers, validated evidence, and approved sections. The index is rebuilt automatically when its SQLite-derived fingerprint changes.

Each project index contains:

- `vectors.f32` — normalized dense embeddings stored as a memory-mapped row-major matrix.
- `bm25.lexicon`, `bm25.postings`, `bm25.lengths` — memory-mapped lexical index with BM25 scoring.
- `requirement.offsets`, `requirement.edges` — CSR requirement-to-record adjacency for graph expansion.
- `records.blob`, `records.offsets` — memory-mapped immutable retrieval records.
- `retrieval.parquet` — durable Arrow/Parquet analytical representation of the same indexed records.
- `manifest.json` — source fingerprint, embedding model, dimensions, row count, and creation timestamp.

Semantic exact search is implemented with normalized embeddings and BLAS SGEMV. Weighted fusion uses OpenMP/SIMD C++ kernels. Large top-k selections use the OpenMP native kernel while small selections use Rayon to avoid parallel-runtime overhead. The context compiler uses this hybrid retrieval result instead of dumping the entire project into the model context.

On Apple Silicon, the native runtime is `vllm-mlx==0.4.1`, serving both the OLMo chat model and an MLX embedding model from the same Metal-backed macOS process. On Intel/low-memory Macs, the `cpu-embedding` Docker profile serves `BAAI/bge-small-en-v1.5` through an OpenAI-compatible `/v1/embeddings` API using FastEmbed/ONNX Runtime. Model dimensions are discovered from responses and are never hard-coded.



## Phase 6 evidence and privacy boundary

Competitive organizations are **potential capability-overlap candidates inferred from public information, not confirmed applicants**. The system must not infer confidential applicant intent, non-public trial results, unpublished IP, or private partnerships. Public search hits are retained with provider, external ID/URL, title, summary, metadata, relevance score, and run provenance so users can inspect what drove the ranking. Strategy prompts instruct the model to turn public observations into positive proposal positioning rather than naming competitors in grant prose unless a human explicitly chooses to do so.

---

# Phase 8 · Production Hardening and Release Operations

Phase 8 closes the engineering plan with production/release controls around the completed grant workflow.

## Install / bootstrap on a Mac

```bash
./install.sh
```

The bootstrap script detects the Mac runtime profile, verifies Docker Desktop, validates the source tree, and prepares local export/backup/benchmark/release directories. It does not write provider secrets. Add required keys to `.env`, then start normally:

```bash
./start.sh
```

## Hardened Docker runtime

All application ports remain loopback-only. Services run with dropped Linux capabilities, `no-new-privileges`, read-only root filesystems, bounded PID/CPU/memory resources, temporary writable `/tmp` filesystems, restart policies, and size-rotated Docker logs. Persistent grant state remains in Docker named volumes while final artifacts stay on the Mac export bind mount.

## System diagnostics and benchmarks

Run a fast host/runtime diagnostic at any time:

```bash
./scripts/doctor.sh
```

Gradio now includes **9 · System & Diagnostics**. It reports only non-secret runtime/build information and can execute the local MMAP/OpenMP/BLAS benchmark.

For a persisted machine-readable benchmark:

```bash
./scripts/benchmark.sh
```

Reports are written to `./benchmarks/` and include repeated HPC timings plus Docker resource snapshots when available.

## Backup / restore

Create a checksum-protected, filesystem-consistent backup of the persistent grant workspace. The script briefly stops application writers so SQLite WAL, MMAP, Parquet, and project files are captured at one point in time:

```bash
./scripts/backup.sh
```

Restore is intentionally destructive and requires explicit confirmation:

```bash
CONFIRM_RESTORE=RESTORE ./scripts/restore.sh ./backups/grant-data-<timestamp>.tar.gz
```

## Release security / reproducibility

Phase 8 provides:

```text
scripts/security_scan.sh
scripts/audit_dependencies.sh
scripts/generate_sbom.py
scripts/generate_release_manifest.py
scripts/sign_release.sh
scripts/build_release.sh
scripts/release_acceptance.sh
```

A reproducible release requires `core/Cargo.lock`. Generate/freeze it on a networked release Mac with:

```bash
./scripts/freeze_rust_dependencies.sh
```

Then the release acceptance gate performs validation, security scan, locked Rust tests, Docker builds, full smoke test, benchmark, and backup before release packaging.

`build_release.sh` emits a CycloneDX dependency inventory, release manifest, ZIP checksum, and—when a configured Minisign or GPG key is available—a detached cryptographic signature. Set `REQUIRE_RELEASE_SIGNATURE=true` in release CI to fail closed when signing is unavailable.

## Final target-Mac release gate

```bash
./scripts/release_acceptance.sh
./scripts/build_release.sh
```

Phase 8 does not weaken any prior grant-writing gate: human-approved exact section versions, competitive self-healing, clinical consistency, sponsor compliance, immutable export snapshots, and registered-artifact checks remain authoritative.

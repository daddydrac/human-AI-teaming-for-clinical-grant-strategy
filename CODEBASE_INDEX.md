# Clinical Grant Workbench — Codebase Index

## System shape

The product is a nine-tab Gradio workbench backed by a Rust API, a Playwright HTML-ingestion service, a Python document renderer, and OpenAI-compatible embedding/generation services. Docker keeps application state in `grant-data`; final DOCX/PDF files are bind-mounted to `exports/`. On this M4 profile, native MLX serves OLMo and embeddings, while Claude plans research, validates retrieved evidence, classifies compliance rule meaning, and performs selected high-value synthesis. Rust alone locates and copies exact compliance source excerpts.

The backend workflow state is: `intake → documents → requirements → interview → research → science → strategy → writing → review → export`. Every final document is built from an immutable approved-section snapshot. Sponsor compliance and readiness gates run before export.

## UI-to-backend workflow

| UI step | User outcome | Backend support |
|---|---|---|
| 1. Intake & Requirements | Create a project; upload, fetch, or paste an opportunity; approve extracted requirements | Browser-rendered URL-to-Markdown ingestion, byte-preserved paste ingestion, chunking, requirement/compliance compilation, SQLite persistence, workflow gates |
| 2. Investigator Interview | Resolve missing facts with confidence and provenance | Claude question generation, typed answer storage, unresolved-question gate |
| 3. Research & Evidence | Find external evidence and test retrieval | Claude research plan, bounded Brave/web fetching, Claude evidence validation, embeddings, lexical/vector/graph retrieval |
| 4. Clinical Study Design | Define aims, arms, endpoints, statistics, recruitment, timeline, and resources | Typed validation, sample-size/recruitment calculations, scenario sweeps, cross-section consistency checks |
| 5. Competitive Intelligence | Compare against capability-matched organizations using public data | NIH RePORTER, ClinicalTrials.gov, OpenAlex, bounded web enrichment, scoring, refresh and proposed-update workflow |
| 6. Sponsor Compliance | Approve deterministic rules, register forms/attachments, run rendered preflight | Rule engine, checksummed artifact registry, per-section word/page measurements, hard-failure readiness gate |
| 7. Write/Edit/Approve | Draft locally or escalate, edit, and approve exact versions | Context compiler, OLMo default drafting, Claude high-value routing, versioned sections and explicit approval |
| 8. Final Export | Choose DOCX, PDF, or BOTH and receive a submission ZIP | Immutable snapshot, Markdown-aware renderer, DOCX/PDF generation, atomic ZIP with SHA-256 manifest |
| 9. Diagnostics | Inspect safe runtime metadata and benchmark compute | Health/system endpoints and C++/OpenMP/OpenBLAS benchmark |

## File catalog

### Runtime and deployment

- `env.m4Mac.txt` — real-world M4/24 GB hybrid defaults; copied to `.env`.
- `.env.example` — generic deployment template.
- `docker-compose.yml` — core, renderer, UI, and optional CPU embedding topology, security, resources, volumes, and environment wiring.
- `Dockerfile.core` — pinned Rust 1.91 release build and minimal runtime image.
- `Dockerfile.renderer` — renderer/WeasyPrint image.
- `Dockerfile.ui` — Gradio UI image.
- `Dockerfile.embedding-cpu` — low-memory CPU embedding fallback.
- `Dockerfile.ingestion` — pinned Playwright/Chromium HTML-to-Markdown ingestion image.
- `.dockerignore` — excludes secrets, runtime data, and build output from Docker contexts.
- `.gitignore` — excludes credentials, generated data, caches, and artifacts.
- `install.sh` — validates macOS prerequisites, selects the M4 template, configures runtime, and validates the stack.
- `start.sh` — starts native MLX when selected, then starts and health-checks Compose services.
- `stop.sh` — stops the stack and local MLX process.
- `QUICKSTART.md` — minimal M4 startup commands.
- `README.md` — operator and configuration reference.

### Rust core API

- `core/Cargo.toml` — exact direct Rust dependency pins.
- `core/Cargo.lock` — reproducible transitive dependency resolution.
- `core/rust-toolchain.toml` — pinned Rust toolchain configuration.
- `core/build.rs` — compiles and links the C++ HPC kernels.
- `core/src/main.rs` — HTTP routes, application orchestration, workflow gates, background refresh, drafting, and export snapshots.
- `core/src/storage.rs` — SQLite schema, transactions, versions, approvals, readiness, snapshots, compliance, and artifact metadata.
- `core/src/workflow.rs` — ordered workflow-stage model and gate helpers.
- `core/src/domain.rs` — shared request/response and model-output data structures.
- `core/src/models.rs` — hybrid OLMo/Claude routing and provider clients.
- `core/src/research.rs` — safe public search/fetch client and destination validation.
- `core/src/chunker.rs` — normalized document chunking.
- `core/src/context_compiler.rs` — evidence-grounded prompt/context assembly.
- `core/src/embedding.rs` — OpenAI-compatible embedding client and batching.
- `core/src/retrieval.rs` — hybrid retrieval orchestration and ranking.
- `core/src/lexical.rs` — BM25-style lexical index.
- `core/src/vector_store.rs` — normalized memory-mapped vector matrices.
- `core/src/record_store.rs` — memory-mapped retrieval record storage.
- `core/src/csr.rs` — sparse requirement/evidence relationship representation.
- `core/src/parquet_store.rs` — auditable Parquet retrieval exports.
- `core/src/hpc.rs` — safe Rust FFI wrappers for native kernels.
- `core/src/json_extract.rs` — strict JSON extraction from model responses.
- `core/src/clinical.rs` — typed study validation, statistics, recruitment, scenarios, timeline, and consistency analysis.
- `core/src/compliance.rs` — deterministic sponsor-rule validation and evaluation.
- `core/src/source_locator.rs` — parallel deterministic passage matching, normalized offset projection, exact source-buffer copying, and provenance validation.
- `core/src/competitive.rs` — public competitive-data collection, normalization, scoring, and strategy generation.
- `core/src/competitive_updates.rs` — material-change detection and protected section-update proposals.

### UI, rendering, and embeddings

- `ui/app.py` — all nine Gradio tabs and backend/renderer API bindings.
- `ui/requirements.txt` — pinned UI Python dependencies.
- `renderer/app.py` — design profiles, previews, measurements, DOCX/PDF rendering, diffs, and atomic submission packaging.
- `renderer/requirements.txt` — pinned document-rendering dependencies.
- `embedding_cpu/app.py` — OpenAI-compatible FastEmbed fallback service.
- `embedding_cpu/requirements.txt` — pinned CPU embedding dependencies.
- `ingestion/app.py` — browser-rendered public URL extraction into an authoritative Markdown source buffer.
- `ingestion/requirements.txt` — pinned Playwright, FastAPI, and Markdown conversion dependencies.
- `hpc/hpc_kernels.cpp` — OpenMP normalization/fusion/top-K and OpenBLAS matrix scoring.

### Product configuration

- `config/default_sections.json` — initial narrative-section plan.
- `config/default_design.json` — default document style profile.
- `config/sponsor_formats.json` — sponsor-specific formatting overrides.
- `config/competitive_intelligence.json` — public providers, scoring, limits, and refresh policy.
- `config/mlx-runtime.in` — input constraints for freezing the native MLX environment.

### Operations and release scripts

- `scripts/configure_runtime.sh` — machine-aware Apple MLX versus Docker CPU selection and effective resource settings.
- `scripts/start_mlx.sh` — resolves immutable Hugging Face revisions and serves local generation plus embeddings.
- `scripts/tune_mac.sh` — Mac resource-tuning helper.
- `scripts/preflight.sh` — credentials, Docker, MLX, and export-path checks.
- `scripts/validate.sh` — static, integration, build, and release validation entry point.
- `scripts/smoke_test.sh` — running-service health and basic API checks.
- `scripts/doctor.sh` — installation diagnostics.
- `scripts/benchmark.sh` — repeatable performance benchmark runner.
- `scripts/backup.sh` / `scripts/restore.sh` — consistent application-data backup and guarded restore.
- `scripts/audit_dependencies.sh` / `scripts/security_scan.sh` — dependency and container/source security checks.
- `scripts/freeze_rust_dependencies.sh` / `scripts/freeze_mlx_dependencies.sh` — reproducibility lock generation.
- `scripts/generate_sbom.py` / `scripts/generate_release_manifest.py` — CycloneDX inventory and hashed source manifest generation.
- `scripts/build_release.sh` / `scripts/sign_release.sh` / `scripts/release_acceptance.sh` — package, optionally sign, and verify releases.

### Development evidence

- `dev_docs/ERRATA.md` — known corrections and caveats.
- `dev_docs/PHASE6_VALIDATION.md` — competitive-intelligence validation record.
- `dev_docs/PHASE7_VALIDATION.md` — compliance/intake/export validation record.
- `dev_docs/PHASE8_VALIDATION.md` — production-hardening validation record.
- `dev_docs/PHASE8_SOURCE_MANIFEST.json` — prior phase source hashes.
- `dev_docs/PHASE8_SOURCE_SBOM.cdx.json` — prior phase CycloneDX SBOM.

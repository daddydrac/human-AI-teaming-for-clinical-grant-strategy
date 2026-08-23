# Clinical Grant Workbench — Codebase Index

## System shape

The product is a composable Gradio grant workspace backed by a Rust API, a Playwright HTML-ingestion service, a Python document renderer, and provider-neutral embedding/generation services. Every project persists five mandatory core steps plus its selected optional modules; navigation and gates are derived from that server-side configuration. Docker keeps application state in `grant-data`; final DOCX/PDF files are bind-mounted to `exports/`.

The legacy stage value remains a read-only compatibility projection. The authoritative evaluator derives each enabled step as `not_started`, `available`, `in_progress`, `awaiting_review`, `blocked`, or `complete`; disabled modules cannot add blockers. Every final document is built from exact approved artifact and section versions recorded in an immutable snapshot.

## UI-to-backend workflow

| UI step | User outcome | Backend support |
|---|---|---|
| 1. Solicitation analysis | Create/import a project; ingest an opportunity; approve normalized facts and rubric | Browser-rendered ingestion, exact source anchors, structured profile, server-derived editor contract, SQLite persistence, composable gates |
| 2. Research-plan framework | Map sponsor requirements and rubric to an owned proposal outline | Versioned structured nodes, project-scoped requirement/member catalogs, coverage validation, transactional `project_sections` synchronization |
| 3. Key aims | Approve objectives, thesis, typed aims, and evidence support | Structured aim set, fact/estimate/assumption contract, approved-framework linkage, project evidence validation |
| 4. Literature & evidence | Run and approve solicitation- and aim-derived research | Reproducible run manifest, dispositions, transactionally stored evidence, project-scoped source/citation validation |
| Optional: Investigator interview | Resolve missing facts with confidence and provenance | Model-assisted question generation, typed answer storage, unresolved-question gate |
| Optional: Clinical design | Define study design, statistics, recruitment, timeline, and resources | Typed validation, sample-size/recruitment calculations, scenario sweeps, cross-section consistency checks |
| Optional: Competitive intelligence | Compare against capability-matched organizations using public data | NIH RePORTER, ClinicalTrials.gov, OpenAlex, bounded web enrichment, scoring, refresh and proposed-update workflow |
| Optional: Sponsor compliance | Approve deterministic rules, register forms/attachments, run rendered preflight | Rule engine, checksummed artifact registry, per-section word/page measurements, configured readiness gate |
| 5. Draft/review/approve/export | Draft locally or cloud-route permitted tasks, reconcile edits, approve exact versions, and export | Context compiler, per-project routing, immutable lineage, three-way merge, exact approval snapshots, DOCX/PDF/ZIP rendering |

## File catalog

### Runtime and deployment

- `env.m4Mac.txt` — real-world M4/24 GB hybrid defaults; copied to `.env`.
- `.env.example` — generic deployment template.
- `docker-compose.yml` — core, renderer, UI, and optional CPU embedding topology, security, resources, volumes, and environment wiring.
- `docker-compose.oidc.yml` — enterprise override that removes backend host ports and adds pinned OIDC and TLS gateway services.
- `gateway/oauth2-proxy-alpha.yml` — structured OIDC claim mapping that keeps immutable subject and email distinct and injects the private gateway proof.
- `gateway/nginx.conf.template` — TLS/auth-request gateway, browser-header overwrite policy, and authenticated proof forwarding.
- `scripts/preflight_oidc_gateway.sh` — OIDC discovery, PKCE, secret, certificate, and Compose-isolation validation.
- `scripts/start_oidc_gateway.sh` / `scripts/stop_oidc_gateway.sh` — validated shared-enterprise lifecycle commands.
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
- `core/src/storage.rs` — SQLite schema, authoritative structured-editor catalogs, cross-project reference enforcement, atomic section/artifact transactions, approvals, readiness, snapshots, and compliance.
- `core/src/versioning.rs` — bounded deterministic line-level three-way merge with explicit conflict preservation; it never silently chooses between overlapping human edits.
- `core/src/workflow.rs` — versioned workflow registry, composable gate evaluator, module-combination validation, and legacy-stage compatibility projection.
- `core/src/domain.rs` — shared request/response and model-output data structures.
- `core/src/models.rs` — per-project hybrid routing; Rust-derived, versioned JSON Schema contracts; vLLM/MLX, native Ollama, and Claude tool-schema adapters; common fail-closed response validation.
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

- `ui/app.py` — composable Gradio workflow with structured solicitation/framework/aims/literature forms backed by server-derived reference catalogs, authenticated account pages, and the shared Team Workspace.
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
- `scripts/preflight_oidc_gateway.sh` — issuer discovery, immutable-claim, secret, TLS, and enterprise Compose isolation checks.
- `scripts/start_oidc_gateway.sh` / `scripts/stop_oidc_gateway.sh` — shared OIDC deployment lifecycle.
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
- `dev_docs/ADR_003_IDENTITY_AND_SHARED_DEPLOYMENT.md` — accepted internal-account and enterprise identity boundary.
- `dev_docs/PHASE6_VALIDATION.md` — competitive-intelligence validation record.
- `dev_docs/PHASE7_VALIDATION.md` — compliance/intake/export validation record.
- `dev_docs/PHASE8_VALIDATION.md` — production-hardening validation record.
- `dev_docs/PHASE8_SOURCE_MANIFEST.json` — prior phase source hashes.
- `dev_docs/PHASE8_SOURCE_SBOM.cdx.json` — prior phase CycloneDX SBOM.

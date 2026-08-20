# Phase 7 Validation Record

Phase 7 implements the Sponsor Compliance + Submission Package Compiler on top of the completed Phase 6 Competitive Applicant Intelligence Engine.

## Code-complete scope

- Grant opportunity intake by upload, secure public URL fetch, or pasted opportunity text.
- Normalization of all opportunity input modes into one authoritative source corpus.
- Source-grounded sponsor rule compilation with human correction/approval.
- Deterministic compliance rules for required sections, word/page constraints, attachments, file extensions, font/margins, deadlines, required letters, project duration, and manual authoritative requirements.
- Sponsor-required sections synchronize into the writing plan.
- Shared renderer measurement for actual PDF page count and section/total word counts.
- Measurement invalidation after authoritative prose, design, or clinical-study changes.
- Registered submission artifacts with SHA-256 validation.
- Immutable export snapshot containing the approved design, clinical model, competitive intelligence, sponsor compliance profile/assessment, approved section versions, and registered artifacts.
- Final DOCX/PDF plus submission-package ZIP assembled from registered immutable artifacts only.
- Phase 6 Competitive Edge Auto-Update remains intact and defaults to a four-hour refresh cadence.

## Validation executed in this environment

- `scripts/validate.sh`: PASS.
- Python syntax compilation for UI, renderer, and CPU embedding service: PASS.
- Gradio application construction/import: PASS.
- Phase 6 auto-update banner and highlighted diff renderer checks: PASS.
- Phase 7 compliance profile/UI/renderer static acceptance checks: PASS.
- DOCX/PDF rendering through the shared Document AST: PASS.
- Actual PDF page/word measurement: PASS.
- Registered-only attachment packaging with SHA-256 verification: PASS.
- C++17/OpenMP compilation: PASS.
- OpenMP row normalization runtime execution: PASS.
- BLAS SGEMV runtime execution: PASS.
- HPC top-k runtime execution: PASS.
- Cargo manifest, Docker Compose YAML, and JSON configuration parsing: PASS.
- Shell syntax/runtime-profile generation checks: PASS.
- Credential-pattern scan: no embedded production credentials found.
- Runtime/cache artifacts are removed before packaging.

## Validation deferred to the target Mac

This execution environment does not provide Cargo, Docker Desktop, Apple Silicon/Metal, or live external provider credentials. The following therefore remain explicit target-Mac acceptance gates rather than claimed passes:

1. `cargo test --release --locked` inside `core/`.
2. `docker compose config` and the `cpu-embedding` profile.
3. `docker compose build` for all services.
4. Full `scripts/smoke_test.sh` against the running Docker stack.
5. Intel/low-memory FastEmbed runtime acceptance.
6. Apple-Silicon MLX/OLMo runtime acceptance.
7. Live NIH RePORTER, ClinicalTrials.gov, OpenAlex, Brave Search, and Claude provider integration when configured.

## Target-Mac acceptance sequence

```bash
cp .env.example .env
# Add required provider credentials.
./start.sh
./scripts/validate.sh
./scripts/smoke_test.sh
```

Phase 7 is considered code-complete once the target-Mac acceptance sequence passes on the supported deployment profile.

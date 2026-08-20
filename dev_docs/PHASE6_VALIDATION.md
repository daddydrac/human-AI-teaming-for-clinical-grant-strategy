# Phase 6 Validation Record

## Scope

Phase 6 implements the Competitive Applicant Intelligence Engine and the self-healing **Competitive Edge Auto-Update** workflow on top of the Phase 5 clinical-study architecture.

The required behavior is:

1. Build a capability-matched potential-applicant profile from the grant and authoritative clinical study.
2. Discover and score public competitor signals from configured public providers.
3. Persist versioned intelligence, candidate rankings, public assets, provider state, strategy, fingerprints, and hashes.
4. Detect genuinely new/material public competitive evidence without treating transient provider degradation as competitor disappearance.
5. Automatically refresh stale intelligence from backend entry points and a background refresh loop.
6. If fresh public evidence materially changes positioning, create new **unapproved** section proposals rather than replacing human-approved prose.
7. Show exact additions/removals as highlighted diffs in Gradio under **Competitive Edge Auto-Update**.
8. Preserve the old approved section until the human explicitly approves the replacement version.
9. Keep final export fail-closed while competitive reconciliation or human review remains pending.

## Validation completed in this build environment

- Python source compilation: PASS
- Gradio module import/construction: PASS
- Renderer module import: PASS
- Competitive Edge Auto-Update global banner: PASS
- Affected-section listing in UI: PASS
- Highlighted addition/removal diff generation: PASS
- Shared renderer DOCX generation: PASS
- Shared renderer PDF generation: PASS
- Docker Compose YAML parse and Phase 6 environment wiring: PASS
- Shell syntax for startup/validation/smoke scripts: PASS
- `.env.example` can be sourced safely: PASS
- C++17/OpenMP source compilation: PASS
- Native HPC runtime harness (OpenMP normalization/top-k + BLAS SGEMV): PASS
- Static MMAP/BM25/CSR/Parquet/Arrow checks: PASS
- Weak Intel-Mac Docker profile configuration checks: PASS
- Apple-Silicon MLX profile wiring checks: PASS (static only)
- No packaged `.env`, runtime DB, private key, or obvious API-token artifact detected: PASS

## Rust tests added

The Rust source includes unit tests for the most important Phase 6 invariants, including:

- public-data change detection;
- strategy wording drift alone does not trigger automatic proposal rewrites;
- degraded public providers do not create destructive competitor-removal signals;
- competitive proposals do not overwrite human-approved section versions;
- a newer material competitive refresh supersedes obsolete pending proposals.

## Target-Mac validation still required

This execution environment does not provide Cargo, Docker, Apple Metal/MLX, or live external provider credentials. Therefore the following must be executed on the target Mac before production acceptance:

```bash
cp .env.example .env
# Configure ANTHROPIC_API_KEY and any enabled public-provider keys/settings.
./start.sh
./scripts/validate.sh
./scripts/smoke_test.sh
```

The target-Mac acceptance run must confirm:

- Rust dependency resolution and `cargo test`;
- Docker image build and service health;
- weak Intel/8-GB profile with CPU embeddings + Claude;
- Apple-Silicon profile with native MLX when used;
- live public-provider competitive discovery;
- scheduled/background refresh;
- highlighted proposal creation after a material public-data update;
- exact-version human approval protection;
- final immutable DOCX/PDF export.

## Release rule

Phase 6 is not considered production-certified until the target-Mac Cargo/Docker/live-provider smoke test passes. The repository is nevertheless internally wired so failures remain fail-closed: approved prose is preserved and final export is blocked while unresolved competitive reconciliation remains pending.

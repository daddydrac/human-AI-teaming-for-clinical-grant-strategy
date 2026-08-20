# Phase 8 Validation Record

Phase 8 completes the engineering plan by adding production hardening, diagnostics, hardware-aware benchmarking, consistent backup/restore, dependency-audit hooks, SBOM/release manifests, hardened Docker configuration, CI wiring, release checksum/signing support, and fail-closed release acceptance on top of the completed Phase 7 grant workflow.

## Validation environment

Finalization was performed in a Linux x86_64 execution environment with Python 3.13 and GCC 14. Docker, Cargo/Rust, macOS, Apple Metal, and MLX are not installed in this environment. Those checks are therefore retained as explicit target-Mac gates rather than reported as passed.

## Executed and passed locally

- Python source compilation for Gradio UI, renderer, CPU embedding server, SBOM generator, and release-manifest generator.
- Phase 6 Competitive Edge Auto-Update regression checks, including highlighted add/remove diff rendering and protected human-approved text messaging.
- Phase 7 upload/URL/pasted-opportunity compliance workflow static and renderer checks.
- Gradio UI module construction and Phase 8 System & Diagnostics functions.
- C++17/OpenMP syntax validation.
- Native C++ execution harness for OpenMP row normalization, BLAS SGEMV scoring, weighted fusion, and top-k selection.
- Shared renderer produced and reopened a DOCX, produced a readable PDF, and created a submission-package ZIP containing the rendered grant and manifest.
- Weak-Mac `docker_cpu` profile generated the intended 8 GB-class settings: 2 OpenMP threads, 2 Rayon workers, BLAS single-threaded, 8-item embedding batches, 12 retrieved records, and a 24K-character context budget.
- Restore safety test rejected a malicious `../escape.txt` tar member before Docker or destructive filesystem operations were reached.
- CycloneDX 1.5 SBOM generation, including Rust/Python dependencies, container base references, and local model/runtime provenance entries.
- Release manifest generation with per-file SHA-256 hashes, configuration hashes, container-base references, and release-control metadata.
- Checksum signing fallback generated and verified a SHA-256 release checksum when no cryptographic signing key was configured.
- Security scanner passed on the sanitized Phase 8 tree and correctly failed on an injected Anthropic-style credential.
- Shell syntax validation for startup, installation, benchmark, backup/restore, audit, release, and signing scripts.
- Docker Compose YAML and hardened settings were parsed statically: loopback-only ports, read-only roots, dropped capabilities, `no-new-privileges`, PID/CPU/memory ceilings, restart policies, tmpfs, and rotated logs.

## Production defects corrected during Phase 8 finalization

1. Backup previously tarred live SQLite/WAL state. It now briefly stops application writers and creates a filesystem-consistent point-in-time archive plus checksum and backup manifest.
2. Restore previously trusted arbitrary tar members. It now rejects absolute paths, `..` traversal, devices, symlinks, and hard links before deleting existing workspace data.
3. Release security scanning could fail simply because normal local `.env`, Python cache, or Cargo build artifacts existed. Release packaging now creates a sanitized candidate tree and scans that exact candidate before ZIP creation.
4. Weak Macs could incur an unnecessary Rust lock-generation container during normal startup. Normal startup no longer does this; reproducible release tooling remains responsible for generating and enforcing `Cargo.lock`.
5. Docker rebuild performance now uses BuildKit dependency caches for Cargo registries/git and Python pip downloads.
6. Dependency auditing now has explicit `cargo-audit` and `pip-audit` hooks and can be made mandatory in release CI with `REQUIRE_AUDIT_TOOLS=true`.
7. Release acceptance now builds both the normal and CPU-embedding Docker profiles before smoke/benchmark/backup acceptance.

## Required target-Mac acceptance

A reproducible enterprise release requires `core/Cargo.lock`. On a networked release Mac:

```bash
cp .env.example .env
# configure required provider credentials

./install.sh
./scripts/freeze_rust_dependencies.sh
./start.sh
./scripts/smoke_test.sh
./scripts/benchmark.sh
./scripts/release_acceptance.sh
./scripts/build_release.sh
```

The target-Mac gate must prove:

- `cargo test --release --locked` passes.
- Docker Compose builds both default and `cpu-embedding` profiles.
- Intel/low-memory mode operates with FastEmbed/ONNX + Claude.
- Apple-Silicon mode starts native MLX/Metal and resolves immutable OLMo/embedding revisions.
- The full UI workflow completes from grant-opportunity intake through final sponsor-compliant DOCX/PDF/submission package.
- Backup then restore produces a healthy application and preserved projects.
- Release SBOM/manifest/checksum are generated; when `REQUIRE_RELEASE_SIGNATURE=true`, Minisign or GPG signing must succeed.

## Release status

**Phase 8 source code: code-complete.** The artifact is ready for the target-Mac release gate above. No Docker/Rust/Metal result is claimed from this Linux-only finalization environment.

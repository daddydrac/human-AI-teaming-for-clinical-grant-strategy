# Clincial Grant Workbench

Clincial Grant Workbench is a local, human-in-the-loop application for developing sponsor-ready grant proposals. It turns a funding opportunity and supporting project materials into structured requirements, evidence, clinical-study design, competitive intelligence, reviewed grant sections, and a final DOCX/PDF submission package. The app runs as a Gradio UI backed by containerized Rust and Python services; supported Apple Silicon Macs can use local MLX inference, while other Macs use Claude with local CPU embeddings.

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

## Quick start

### Prerequisites

- macOS with at least 10 GB of free disk space
- Docker Desktop installed and running
- `uv` installed when using the native Apple MLX profile (automatically selected on Apple Silicon Macs with at least 16 GB RAM)
- An Anthropic API key on Intel or lower-memory Macs, where the automatic runtime profile uses Claude for generation

### Install and run

```bash
cp .env.example .env
```

Edit `.env` before starting:

- Set `ANTHROPIC_API_KEY` when required by the Docker CPU profile, or when you want Claude escalation in hybrid mode.
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

Generated documents are written to `./exports` by default. Project data is retained in the Docker `grant-data` volume when the application is stopped.

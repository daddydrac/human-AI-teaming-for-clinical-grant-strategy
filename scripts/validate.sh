#!/usr/bin/env bash
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
python3 - "$ROOT" <<'PYSYNTAX'
from pathlib import Path
import sys
root=Path(sys.argv[1])
for rel in ('ui/app.py','renderer/app.py','embedding_cpu/app.py','scripts/generate_sbom.py','scripts/generate_release_manifest.py'):
    p=root/rel
    compile(p.read_text(),str(p),'exec')
print('Python syntax validation passed.')
PYSYNTAX
( set -a; source "$ROOT/.env.example"; set +a; test -n "$ORGANIZATION_NAME"; test -n "$GRANT_SECTIONS"; test -n "$COMPETITIVE_UPDATE_LABEL" )
bash -n "$ROOT/start.sh" "$ROOT/stop.sh" "$ROOT/scripts/configure_runtime.sh" "$ROOT/scripts/tune_mac.sh" "$ROOT/scripts/start_mlx.sh" "$ROOT/scripts/smoke_test.sh" "$ROOT/scripts/preflight.sh"
g++ -std=c++17 -O3 -fopenmp -fsyntax-only "$ROOT/hpc/hpc_kernels.cpp"

grep -q 'memmap2' "$ROOT/core/Cargo.toml"
grep -q '#pragma omp parallel' "$ROOT/hpc/hpc_kernels.cpp"
grep -q 'cblas_sgemv' "$ROOT/hpc/hpc_kernels.cpp"
grep -q 'hpc_topk_indices' "$ROOT/hpc/hpc_kernels.cpp"
grep -q 'rayon' "$ROOT/core/Cargo.toml"
grep -q 'write_retrieval_parquet' "$ROOT/core/src/parquet_store.rs"
grep -q 'MmapMatrixWriter' "$ROOT/core/src/vector_store.rs"
grep -q 'LexicalIndex' "$ROOT/core/src/lexical.rs"
grep -q 'RequirementCsr' "$ROOT/core/src/csr.rs"
grep -q 'MmapRecordStore' "$ROOT/core/src/record_store.rs"
grep -q 'embed_documents' "$ROOT/core/src/embedding.rs"
grep -q 'embed_query' "$ROOT/core/src/embedding.rs"
grep -q 'MODEL_ROUTING_MODE' "$ROOT/core/src/models.rs"
grep -q 'claude_only' "$ROOT/core/src/models.rs"
grep -q 'fastembed' "$ROOT/embedding_cpu/requirements.txt"
grep -q 'profiles: \["cpu-embedding"\]' "$ROOT/docker-compose.yml"
grep -q 'grant-data:/workspace' "$ROOT/docker-compose.yml"
grep -q 'GRANT_EXPORT_HOME' "$ROOT/docker-compose.yml"
grep -q 'context_compiler::compile' "$ROOT/core/src/main.rs"
grep -q 'validate_public_destination' "$ROOT/core/src/research.rs"
grep -q 'site:{d}.* OR ' "$ROOT/core/src/research.rs"
grep -q 'supporting_excerpt' "$ROOT/core/src/main.rs"
grep -q 'interview_questions' "$ROOT/core/src/storage.rs"
grep -q 'approved-sections' "$ROOT/core/src/main.rs"
grep -q 'approved-document' "$ROOT/core/src/main.rs"
grep -q 'clinical-study' "$ROOT/core/src/main.rs"
grep -q 'clinical_assessment_json' "$ROOT/core/src/storage.rs"
grep -q 'assess_recruitment' "$ROOT/core/src/clinical.rs"
grep -q 'sample_size' "$ROOT/core/src/clinical.rs"
grep -q 'scenario_sweep' "$ROOT/core/src/clinical.rs"
grep -q 'par_iter' "$ROOT/core/src/clinical.rs"
grep -q 'StudyArm' "$ROOT/core/src/clinical.rs"
grep -q 'endpoint_analysis_mismatch' "$ROOT/core/src/clinical.rs"
grep -q 'timeline_dependency_overlap' "$ROOT/core/src/clinical.rs"
grep -q 'cross_section_consistency' "$ROOT/core/src/clinical.rs"
grep -q 'clinical_context' "$ROOT/core/src/context_compiler.rs"
grep -q 'Clinical Study Design' "$ROOT/ui/app.py"
grep -q 'Competitive Applicant Intelligence' "$ROOT/ui/app.py"
grep -q 'COMPETITIVE_BACKGROUND_REFRESH_ENABLED' "$ROOT/core/src/main.rs"
grep -q 'COMPETITIVE_BACKGROUND_REFRESH_SECONDS' "$ROOT/core/src/main.rs"
grep -q 'competitive_pending_section_updates_json' "$ROOT/core/src/storage.rs"
grep -q 'public_data_changed' "$ROOT/core/src/competitive_updates.rs"
grep -q 'provider_degraded' "$ROOT/core/src/competitive_updates.rs"
grep -q 'preview-diff' "$ROOT/renderer/app.py"
grep -q 'agentic-add' "$ROOT/renderer/app.py"
grep -q 'COMPETITIVE_UPDATE_LABEL' "$ROOT/ui/app.py"
grep -q 'Competitive Edge Auto-Update' "$ROOT/.env.example"
grep -q '/competitive/profile/generate' "$ROOT/core/src/main.rs"
grep -q '/competitive/run' "$ROOT/core/src/main.rs"
grep -q 'pub struct CompetitiveEngine' "$ROOT/core/src/competitive.rs"
grep -q 'nih_reporter_projects' "$ROOT/core/src/competitive.rs"
grep -q 'clinical_trials_studies' "$ROOT/core/src/competitive.rs"
grep -q 'OPENALEX_API_KEY' "$ROOT/core/src/competitive.rs"
grep -q 'web_enrichment_concurrency' "$ROOT/core/src/competitive.rs"
grep -q 'hpc::normalize_rows' "$ROOT/core/src/competitive.rs"
grep -q 'hpc::scores' "$ROOT/core/src/competitive.rs"
grep -q 'competitive_positioning' "$ROOT/core/src/competitive.rs"
grep -q 'competitive_ready' "$ROOT/core/src/storage.rs"
grep -q 'competitive_context' "$ROOT/core/src/context_compiler.rs"
grep -q 'competitive_intelligence' "$ROOT/core/src/storage.rs"
grep -q 'COPY config /app/config' "$ROOT/Dockerfile.core"
grep -q 'COMPETITIVE_CONFIG_PATH' "$ROOT/docker-compose.yml"
grep -q 'OPENALEX_API_KEY' "$ROOT/docker-compose.yml"
! grep -q 'final_red_team' "$ROOT/core/src"/*.rs
python3 - "$ROOT/config/competitive_intelligence.json" <<'PYCFG'
import json,sys
x=json.load(open(sys.argv[1]))
assert x['schema_version']>=1
assert x['endpoints']['nih_reporter_projects'].startswith('https://')
assert x['endpoints']['clinical_trials_studies'].startswith('https://')
assert x['endpoints']['openalex_works'].startswith('https://')
assert x['limits']['web_enrichment_concurrency']>=1
assert x['limits']['max_candidates']>=1
assert x['scoring']['minimum_asset_relevance']>=0
assert {'grant','publication','clinical_trial','patent_ip','technology'} <= set(x['scoring']['asset_type_weights'])
assert {'nih_reporter','clinical_trials','openalex','ip_web','technology_web'} <= set(x['scoring']['provider_reliability'])
assert x['updates']['auto_revise_sections'] is True
assert x['updates']['max_sections_per_refresh'] >= 1
assert x['updates']['candidate_score_delta'] >= 0
assert isinstance(x['updates']['section_refresh_high_value'], bool)
PYCFG

python3 - "$ROOT" <<'PYUPDATE'
import importlib.util,sys
from pathlib import Path
root=Path(sys.argv[1])
# Renderer must provide an actual highlighted in-page diff, not only a banner.
spec=importlib.util.spec_from_file_location('grant_renderer',root/'renderer'/'app.py')
mod=importlib.util.module_from_spec(spec);spec.loader.exec_module(mod)
diff=mod.word_diff_html('Original grant language','Updated superior grant language')
assert 'agentic-add' in diff and 'agentic-remove' in diff,diff
# Gradio messaging must identify affected sections and clearly protect approved prose.
spec=importlib.util.spec_from_file_location('grant_ui',root/'ui'/'app.py')
ui=importlib.util.module_from_spec(spec);spec.loader.exec_module(ui)
banner=ui.global_competitive_update_banner({'pending':2,'processing_pending':0,'pending_sections':[{'title':'Specific Aims'},{'title':'Approach'}],'events':[{'summary':'New public technology and IP evidence found.','material':True}]})
assert 'Specific Aims' in banner and 'Approach' in banner and 'not silently overwritten' in banner,banner
section=ui.competitive_update_banner({'status':'pending','summary':'New competitor data','refresh_reason':['public_intelligence_refresh_due'],'proposed_version':5},6)
assert 'including edits you made' in section and 'protected' in section,section
print('Phase 6 auto-update UI/diff validation passed.')
PYUPDATE
grep -q 'preview_approved_grant' "$ROOT/ui/app.py"
! grep -q 'claim_lineage' "$ROOT/core/src/main.rs"

# Phase 7 sponsor compliance + multi-mode opportunity intake
for needle in   'mod compliance'   '/compliance/compile'   '/compliance/assessment'   '/opportunity-source'   '/submission-artifacts'; do grep -q "$needle" "$ROOT/core/src/main.rs"; done
grep -q 'sponsor_compliance_ready' "$ROOT/core/src/storage.rs"
grep -q 'pub struct ComplianceProfile' "$ROOT/core/src/compliance.rs"
grep -q 'required_section' "$ROOT/core/src/compliance.rs"
grep -q 'max_pages' "$ROOT/core/src/compliance.rs"
grep -q 'compliance_context' "$ROOT/core/src/context_compiler.rs"
grep -q 'funding_paste' "$ROOT/ui/app.py"
grep -q 'Paste the grant opportunity' "$ROOT/ui/app.py"
grep -q 'Sponsor Compliance & Submission' "$ROOT/ui/app.py"
grep -q '@app.post("/measure")' "$ROOT/renderer/app.py"
grep -q '@app.post("/package")' "$ROOT/renderer/app.py"
grep -q '^COMPETITIVE_REFRESH_TTL_SECONDS=14400$' "$ROOT/.env.example"
grep -q '^COMPETITIVE_BACKGROUND_REFRESH_SECONDS=14400$' "$ROOT/.env.example"
grep -q '^COMPETITIVE_UI_POLL_SECONDS=14400$' "$ROOT/.env.example"
python3 - "$ROOT" <<'PYPH7'
import importlib.util,sys
from pathlib import Path
root=Path(sys.argv[1])
spec=importlib.util.spec_from_file_location('grant_ui_phase7',root/'ui'/'app.py')
ui=importlib.util.module_from_spec(spec);spec.loader.exec_module(ui)
profile=ui.build_compliance_profile('Sponsor','Mechanism','Portal','2030-01-01',[[
    'C-001','section','required_section','proposal','specific_aims','hard',True,None,None,'','Specific Aims required','p.1',''
]])
assert profile['rules'][0]['rule_type']=='required_section' and profile['rules'][0]['mandatory'] is True
assert ui.COMPETITIVE_UI_POLL_SECONDS==14400
spec=importlib.util.spec_from_file_location('grant_renderer_phase7',root/'renderer'/'app.py')
r=importlib.util.module_from_spec(spec);spec.loader.exec_module(r)
assert hasattr(r,'measure') and hasattr(r,'package')
print('Phase 7 compliance/intake/renderer validation passed.')
PYPH7

TMP="$(mktemp)"; trap 'rm -f "$TMP"' EXIT
GRANT_RUNTIME_PROFILE=docker_cpu "$ROOT/scripts/configure_runtime.sh" "$TMP" >/dev/null
grep -q '^COMPOSE_PROFILES=cpu-embedding$' "$TMP"
grep -q '^MODEL_ROUTING_MODE=claude_only$' "$TMP"
grep -q '^EMBEDDING_URL=http://embedding-cpu:8010/v1/embeddings$' "$TMP"
grep -q '^OMP_NUM_THREADS=' "$TMP"

if command -v cargo >/dev/null 2>&1 && [[ -f "$ROOT/core/Cargo.lock" ]]; then
  (cd "$ROOT/core" && cargo test --release --locked)
elif command -v cargo >/dev/null 2>&1; then
  echo "cargo is available but core/Cargo.lock is absent; locked Rust tests are deferred until the release lock is generated."
else
  echo "cargo unavailable in this environment; Rust compilation deferred to Docker/target Mac."
fi
if command -v docker >/dev/null 2>&1; then
  (cd "$ROOT" && docker compose config >/dev/null && docker compose --profile cpu-embedding config >/dev/null && docker compose build)
else
  echo "docker unavailable in this environment; container build deferred to target Mac."
fi
echo "Validation completed."

# Phase 8 production hardening / release engineering
grep -q '/api/system/info' "$ROOT/core/src/main.rs"
grep -q 'mmap_create_ms' "$ROOT/core/src/main.rs"
grep -q 'System & Diagnostics' "$ROOT/ui/app.py"
grep -q 'read_only: true' "$ROOT/docker-compose.yml"
grep -q 'no-new-privileges:true' "$ROOT/docker-compose.yml"
grep -q 'cap_drop:' "$ROOT/docker-compose.yml"
grep -q 'restart: unless-stopped' "$ROOT/docker-compose.yml"
grep -q 'max-size: "10m"' "$ROOT/docker-compose.yml"
for file in install.sh scripts/security_scan.sh scripts/benchmark.sh scripts/backup.sh scripts/restore.sh scripts/audit_dependencies.sh scripts/doctor.sh scripts/build_release.sh scripts/sign_release.sh scripts/release_acceptance.sh; do
  test -x "$ROOT/$file"
  bash -n "$ROOT/$file"
done

grep -q 'application writers stopped during archive' "$ROOT/scripts/backup.sh"
grep -q 'unsafe archive path' "$ROOT/scripts/restore.sh"
grep -q 'cargo-audit' "$ROOT/scripts/audit_dependencies.sh"
grep -q 'Grant Writer doctor' "$ROOT/scripts/doctor.sh"
grep -q 'type=cache,target=/usr/local/cargo/registry' "$ROOT/Dockerfile.core"
grep -q 'type=cache,target=/root/.cache/pip' "$ROOT/Dockerfile.ui"
python3 - "$ROOT" <<'PYPH8'
import importlib.util,json,pathlib,sys,tomllib,yaml,tempfile
root=pathlib.Path(sys.argv[1])
with open(root/'core'/'Cargo.toml','rb') as f:cargo=tomllib.load(f)
assert cargo['package']['version']=='0.8.0'
compose=yaml.safe_load((root/'docker-compose.yml').read_text())
for name in ('core','ui','renderer','embedding-cpu'):
    svc=compose['services'][name]
    assert svc['read_only'] is True
    assert svc['cap_drop']==['ALL']
    assert svc['security_opt']==['no-new-privileges:true']
    assert str(svc['ports'][0]).startswith('127.0.0.1:')
# UI construction and non-secret diagnostics functions.
spec=importlib.util.spec_from_file_location('phase8_ui',root/'ui'/'app.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
assert hasattr(m,'system_diagnostics') and hasattr(m,'run_hpc_diagnostics') and m.GRANT_BUILD_VERSION=='0.8.0'
# SBOM/manifest generators must run without external services.
with tempfile.TemporaryDirectory() as td:
    td=pathlib.Path(td)
    import subprocess
    subprocess.check_call([sys.executable,str(root/'scripts'/'generate_sbom.py'),str(td/'sbom.json')])
    subprocess.check_call([sys.executable,str(root/'scripts'/'generate_release_manifest.py'),str(td/'manifest.json')])
    sb=json.load(open(td/'sbom.json')); mf=json.load(open(td/'manifest.json'))
    assert sb['bomFormat']=='CycloneDX' and sb['components']
    assert mf['version']=='0.8.0' and mf['source_files'] and mf['contains_secrets'] is False
print('Phase 8 production/release static validation passed.')
PYPH8
echo "Phase 8 validation completed."

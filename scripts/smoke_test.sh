#!/usr/bin/env bash
set -euo pipefail
CORE="${CORE_URL:-http://localhost:8080}"
RENDERER="${RENDERER_URL:-http://localhost:8090}"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

json_post(){ curl -fsS -X POST "$1" -H 'content-type: application/json' -d "$2"; }
status_post(){ curl -sS -o "$TMP/body" -w '%{http_code}' -X POST "$1" -H 'content-type: application/json' -d "$2"; }

echo "[1/29] liveness + MLX/embedding readiness"
curl -fsS "$CORE/health" | python3 -m json.tool >/dev/null
curl -fsS "$CORE/health/ready" | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["status"]=="ready" and x["embedding"]["dimensions"]>0 and x["model"]["model"]'
curl -fsS "$RENDERER/health" | python3 -m json.tool >/dev/null

echo "[2/29] create project with explicit ordered sections"
PROJECT_ID="$(json_post "$CORE/api/projects" '{"title":"Phase 7 Sponsor Compliance Smoke Test","sponsor":"Integration Test","mechanism":"validation","sections":["Specific Aims","Approach"]}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"

echo "[3/29] export gate rejects incomplete project"
CODE="$(status_post "$CORE/api/projects/$PROJECT_ID/export-snapshot" '{}')"; test "$CODE" = "409"

echo "[4/29] build and persist authoritative design profile"
python3 - "$RENDERER" "$CORE" "$PROJECT_ID" "$TMP/profile.json" <<'PY'
import json,sys,urllib.request
renderer,core,pid,path=sys.argv[1:]
def request(url,payload):
    data=json.dumps(payload).encode(); req=urllib.request.Request(url,data=data,headers={'content-type':'application/json'},method='POST')
    with urllib.request.urlopen(req,timeout=60) as r:return json.load(r)
profile=request(renderer+'/design-profile',{'project_id':pid,'sponsor':'Integration Test','organization_name':'Integration Test Organization','asset_paths':[]})
assert profile['body_size_pt']>0 and profile['page_width_in']>0
saved=request(core+f'/api/projects/{pid}/design-profile',{'profile':profile})
assert len(saved['sha256'])==64
json.dump(profile,open(path,'w'))
PY

echo "[5/29] ingest funding source + chunks"
json_post "$CORE/api/projects/$PROJECT_ID/documents" '{"name":"smoke.txt","kind":"funding_opportunity","text":"Applicants must include Specific Aims and Approach sections. The application must use at least 11 point body font and margins of at least 0.5 inches. The full proposal must not exceed 50 pages. Applicants must describe the scientific objective, provide supporting evidence, explain patient recruitment feasibility, identify measurable outcomes, provide a statistically defensible analysis plan, and describe the implementation approach."}' | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["chunks"]>=1 and x["document_id"]>0 and x["added"]'

echo "[6/29] analyze atomic requirements"
curl -fsS -X POST "$CORE/api/projects/$PROJECT_ID/analyze-requirements" | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["count"]>0'

echo "[7/29] compile opportunity into sponsor compliance rules and verify normalized source"
curl -fsS -X POST "$CORE/api/projects/$PROJECT_ID/compliance/compile" > "$TMP/compliance-compiled.json"
python3 - "$TMP/compliance-compiled.json" <<'PYCOMP7'
import json,sys
x=json.load(open(sys.argv[1])); assert x.get('profile',{}).get('rules'),x
assert len(x.get('sha256') or '')==64
PYCOMP7
curl -fsS "$CORE/api/projects/$PROJECT_ID/opportunity-source" | python3 -c 'import json,sys; x=json.load(sys.stdin); assert "Specific Aims" in x["text"] and len(x["fingerprint"])==64'

# Save a deterministic human-reviewed profile so the remaining smoke test does not
# depend on model interpretation of formatting semantics.
json_post "$CORE/api/projects/$PROJECT_ID/compliance" '{"profile":{"sponsor":"Integration Test","mechanism":"validation","submission_system":"Test Portal","deadline_iso":null,"rules":[
  {"rule_id":"C-001","category":"section","rule_type":"required_section","scope":"proposal","target":"specific_aims","severity":"hard","mandatory":true,"numeric_value":null,"text_value":null,"list_value":[],"source_excerpt":"Applicants must include Specific Aims and Approach sections.","source_locator":"smoke source","notes":"Human-reviewed normalization"},
  {"rule_id":"C-002","category":"section","rule_type":"required_section","scope":"proposal","target":"approach","severity":"hard","mandatory":true,"numeric_value":null,"text_value":null,"list_value":[],"source_excerpt":"Applicants must include Specific Aims and Approach sections.","source_locator":"smoke source","notes":"Human-reviewed normalization"},
  {"rule_id":"C-003","category":"format","rule_type":"min_font_size_pt","scope":"proposal","target":"document","severity":"hard","mandatory":true,"numeric_value":11,"text_value":null,"list_value":[],"source_excerpt":"The application must use at least 11 point body font","source_locator":"smoke source","notes":"Human-reviewed normalization"},
  {"rule_id":"C-004","category":"format","rule_type":"min_margin_in","scope":"proposal","target":"document","severity":"hard","mandatory":true,"numeric_value":0.5,"text_value":null,"list_value":[],"source_excerpt":"margins of at least 0.5 inches","source_locator":"smoke source","notes":"Human-reviewed normalization"},
  {"rule_id":"C-005","category":"format","rule_type":"max_pages","scope":"proposal","target":"full_document","severity":"hard","mandatory":true,"numeric_value":50,"text_value":null,"list_value":[],"source_excerpt":"The full proposal must not exceed 50 pages.","source_locator":"smoke source","notes":"Human-reviewed normalization"}
]}}' | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["version"]>=2 and not x["approved"]'
curl -fsS -X POST "$CORE/api/projects/$PROJECT_ID/compliance/approve" | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["approved"] and x["fresh"]'

echo "[8/29] backend gates reject interview and drafting before requirement approval"
CODE="$(status_post "$CORE/api/projects/$PROJECT_ID/interview/generate" '{}')"; test "$CODE" = "409"
CODE="$(status_post "$CORE/api/projects/$PROJECT_ID/draft-section" '{"section_key":"specific_aims","title":"Specific Aims","additional_context":"","high_value":false}')"; test "$CODE" = "409"

echo "[9/29] approve requirements and generate typed interview"
curl -fsS -X POST "$CORE/api/projects/$PROJECT_ID/requirements/approve" | python3 -c 'import json,sys; assert json.load(sys.stdin)["approved"]>0'
curl -fsS -X POST "$CORE/api/projects/$PROJECT_ID/interview/generate" > "$TMP/interview.json"

echo "[10/29] answer every open investigator question using stdlib-only client"
python3 - "$CORE" "$PROJECT_ID" "$TMP/interview.json" <<'PY'
import json,sys,urllib.request
core,pid,path=sys.argv[1:]
def post(path,payload):
    req=urllib.request.Request(core+path,data=json.dumps(payload).encode(),headers={'content-type':'application/json'},method='POST')
    with urllib.request.urlopen(req,timeout=60) as r:return json.load(r)
def get(path):
    with urllib.request.urlopen(core+path,timeout=60) as r:return json.load(r)
questions=json.load(open(path)).get('questions',[])
for q in questions:
    if q.get('status')!='open':continue
    t=q.get('answer_type','text')
    if t=='integer':value=100
    elif t=='number':value=1.5
    elif t=='percentage':value=50.0
    elif t=='boolean':value=True
    elif t=='date':value='2027-01-01'
    elif t=='choice':value=(q.get('choices') or ['Not specified'])[0]
    else:value='Validated smoke-test investigator response; evidence remains subject to production review.'
    post(f'/api/projects/{pid}/interview/answer',{'question_id':q['id'],'value':value,'confidence':'medium','classification':'investigator_estimate','notes':'Automated integration-test answer','answered_by':'integration-test'})
remaining=get(f'/api/projects/{pid}/interview')
assert not any(q.get('status')=='open' for q in remaining),remaining
PY

echo "[11/29] save versioned authoritative clinical study and run deterministic feasibility checks"
json_post "$CORE/api/projects/$PROJECT_ID/clinical-study" '{
  "clinical_problem":"Pancreatic adenocarcinoma remains a lethal cancer requiring better biomarker-guided treatment selection",
  "knowledge_gap":"Publicly validated biomarkers that prospectively improve treatment selection remain limited",
  "central_hypothesis":"A circulating tumor DNA biomarker-guided treatment strategy improves objective response.",
  "population":{"disease":"pancreatic adenocarcinoma","stage":"advanced","biomarker_criteria":"circulating tumor DNA biomarker-positive","inclusion_criteria":["eligible adult"],"exclusion_criteria":["contraindication"]},
  "design":{"design_type":"prospective randomized trial","phase":"II","randomization":"1:1","allocation_ratio":"1:1","blinding":"open label","follow_up_months":12,"sites":2},
  "recruitment":{"available_patients_per_site_month":20,"eligibility_rate_pct":80,"biomarker_positive_rate_pct":50,"consent_rate_pct":80,"target_enrollment":120,"accrual_months":12,"sites":2},
  "statistics":{"test_type":"two_proportions","alpha":0.05,"power":0.8,"attrition_pct":10,"control_rate":0.25,"treatment_rate":0.55,"null_rate":null,"alternative_rate":null,"mean_delta":null,"std_dev":null,"hazard_ratio":null,"event_probability":null},
  "aims":[{"aim_id":"AIM-1","title":"Evaluate the intervention","hypothesis":"The biomarker-guided intervention improves objective response","expected_endpoint_type":"binary","endpoint_ids":["EP-1"],"expected_result":"Improved response","risk":"Accrual","alternative_strategy":"Expand sites"}],
  "arms":[{"arm_id":"ARM-1","name":"Control","intervention":"Standard care","comparator":true},{"arm_id":"ARM-2","name":"Intervention","intervention":"Biomarker-guided treatment strategy","comparator":false}],
  "endpoints":[{"endpoint_id":"EP-1","name":"Response","endpoint_type":"binary","primary":true,"analysis_family":"two_proportions"}],
  "timeline":[{"task_id":"T-1","name":"Study activation","start_month":0,"duration_months":2,"dependencies":[]},{"task_id":"T-2","name":"Accrual","start_month":2,"duration_months":10,"dependencies":["T-1"]}],
  "resources":[{"resource_id":"RES-1","name":"Clinical recruitment infrastructure","required":true,"available":true,"notes":"Integration-test resource"}]
}' | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["saved"]["version"]==1 and len(x["saved"]["sha256"])==64; a=x["assessment"]; assert not a["errors"] and a["recruitment"]["feasible_within_planned_window"] is True'

echo "[12/29] sample-size endpoint and persisted assessment are deterministic"
json_post "$CORE/api/projects/$PROJECT_ID/clinical/sample-size" '{"test_type":"two_proportions","alpha":0.05,"power":0.8,"attrition_pct":10,"control_rate":0.25,"treatment_rate":0.55}' | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["complete"] and x["adjusted_total_n"]>=x["raw_total_n"]>0'
curl -fsS "$CORE/api/projects/$PROJECT_ID/clinical-assessment" | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["exists"] and x["cross_section_consistency"]["count"]==0 and not x["errors"]'
json_post "$CORE/api/projects/$PROJECT_ID/clinical/scenarios" '{"sites":[1,2,3],"consent_rates_pct":[60,80],"biomarker_positive_rates_pct":[40,60]}' | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["combinations"]==12 and len(x["rows"])==12 and all("estimated_accrual_months" in r for r in x["rows"])'

echo "[13/29] compile MMAP/Parquet/BM25/CSR index including clinical study source of truth"
curl -fsS -X POST "$CORE/api/projects/$PROJECT_ID/index/rebuild" | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["rows"]>0 and x["dimensions"]>0 and len(x["fingerprint"])==64'

echo "[14/29] hybrid retrieval exposes real score channels"
json_post "$CORE/api/projects/$PROJECT_ID/retrieve" '{"query":"patient recruitment feasibility and measurable outcomes","k":5}' | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x and all(k in x[0] for k in ("semantic","lexical","evidence","freshness","record")); assert 0.0 <= x[0]["freshness"] <= 1.0'

echo "[15/29] writing is blocked until public competitive applicant intelligence is fresh"
CODE="$(status_post "$CORE/api/projects/$PROJECT_ID/draft-section" '{"section_key":"specific_aims","title":"Specific Aims","additional_context":"Integration validation","high_value":false}')"; test "$CODE" = "409"

echo "[16/29] generate versioned likely strong-applicant capability profile"
curl -fsS -X POST "$CORE/api/projects/$PROJECT_ID/competitive/profile/generate" > "$TMP/competitive-profile.json"
python3 - "$TMP/competitive-profile.json" <<'PYCOMP'
import json,sys
x=json.load(open(sys.argv[1]))
assert x['version']>=1 and len(x['sha256'])==64
p=x['profile']; assert p['capability_dimensions'] and p['search_queries']
assert abs(sum(float(d['weight']) for d in p['capability_dimensions'])-1.0)<1e-4
PYCOMP

echo "[17/29] discover public capability-matched organizations and synthesize evidence-bounded positioning"
curl -fsS -X POST "$CORE/api/projects/$PROJECT_ID/competitive/run" -H 'content-type: application/json' -d '{}' > "$TMP/competitive.json"
python3 - "$TMP/competitive.json" <<'PYCOMP'
import json,sys
x=json.load(open(sys.argv[1]))
assert x['exists'] and x['fresh'] and x['status']=='complete',x
assert len(x.get('candidates') or [])>0,x
assert len(x.get('assets') or [])>0,x
assert (x.get('strategy') or {}).get('market_summary'),x
assert all(c.get('name') for c in x['candidates'])
assert all(a.get('provider') and a.get('asset_type') and a.get('title') for a in x['assets'])
PYCOMP
curl -fsS "$CORE/api/projects/$PROJECT_ID" | python3 -c 'import json,sys; assert json.load(sys.stdin)["stage"]=="strategy"'

echo "[18/29] initial intelligence refresh creates no phantom text updates before sections exist"
curl -fsS "$CORE/api/projects/$PROJECT_ID/competitive/updates" | python3 -c 'import json,sys; x=json.load(sys.stdin); assert "pending_sections" in x and x["pending"]==0 and x["processing_pending"]==0 and x["events"],x'

echo "[19/29] competitive intelligence enters the HPC retrieval index"
curl -fsS -X POST "$CORE/api/projects/$PROJECT_ID/index/rebuild" | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["rows"]>0 and len(x["fingerprint"])==64'
json_post "$CORE/api/projects/$PROJECT_ID/retrieve" '{"query":"competitive differentiation public grants patents clinical trials technology positioning","k":20}' | python3 -c 'import json,sys; x=json.load(sys.stdin); kinds={h["record"]["kind"] for h in x}; assert "competitive_strategy" in kinds or "competitive_candidate" in kinds,(kinds,x[:3])'

echo "[20/29] draft, human-edit, and exact-version approve configured sections"
RESP="$(json_post "$CORE/api/projects/$PROJECT_ID/draft-section" '{"section_key":"specific_aims","title":"Specific Aims","additional_context":"Integration validation","high_value":false}')"
DRAFT_VERSION="$(printf '%s' "$RESP" | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["text"].strip(); print(x["version"])')"
HUMAN_BODY='Human-approved smoke-test Specific Aims text. This exact edited wording must be the version aggregated into the final document.'
EDIT_RESP="$(json_post "$CORE/api/projects/$PROJECT_ID/sections/specific_aims" "$(python3 -c 'import json,sys; print(json.dumps({"title":"Specific Aims","body":sys.argv[1],"html":None,"base_version_id":int(sys.argv[2])}))' "$HUMAN_BODY" "$DRAFT_VERSION")")"
HUMAN_VERSION="$(printf '%s' "$EDIT_RESP" | python3 -c 'import json,sys; print(json.load(sys.stdin)["version"])')"
test "$HUMAN_VERSION" -gt "$DRAFT_VERSION"
json_post "$CORE/api/projects/$PROJECT_ID/sections/specific_aims/approve" "{\"version_id\":$HUMAN_VERSION}" | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["approved_version"]>0'

RESP="$(json_post "$CORE/api/projects/$PROJECT_ID/draft-section" '{"section_key":"approach","title":"Approach","additional_context":"Integration validation","high_value":false}')"
DRAFT_APPROACH_VERSION="$(printf '%s' "$RESP" | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["text"].strip(); print(x["version"])')"
APPROACH_BODY='Human-approved smoke-test Approach text describing the structured study design without changing authoritative enrollment or site assumptions.'
APPROACH_EDIT="$(json_post "$CORE/api/projects/$PROJECT_ID/sections/approach" "$(python3 -c 'import json,sys; print(json.dumps({"title":"Approach","body":sys.argv[1],"html":None,"base_version_id":int(sys.argv[2])}))' "$APPROACH_BODY" "$DRAFT_APPROACH_VERSION")")"
APPROACH_VERSION="$(printf '%s' "$APPROACH_EDIT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["version"])')"
test "$APPROACH_VERSION" -gt "$DRAFT_APPROACH_VERSION"
json_post "$CORE/api/projects/$PROJECT_ID/sections/approach/approve" "{\"version_id\":$APPROACH_VERSION}" | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["approved_version"]>0'

curl -fsS "$CORE/api/projects/$PROJECT_ID/approved-document" > "$TMP/approved-document.json"
python3 - "$TMP/approved-document.json" "$HUMAN_BODY" <<'PY2'
import json,sys
x=json.load(open(sys.argv[1])); body=sys.argv[2]
assert x['counts']['approved']==2,x
assert [s['title'] for s in x['sections']]==['Specific Aims','Approach'],x['sections']
assert x['sections'][0]['body']==body,x['sections'][0]
PY2

echo "[21/29] invalid approval is rejected without corrupting review stage"
CODE="$(status_post "$CORE/api/projects/$PROJECT_ID/sections/specific_aims/approve" '{"version_id":999999999}')"; test "$CODE" = "400"
curl -fsS "$CORE/api/projects/$PROJECT_ID" | python3 -c 'import json,sys; assert json.load(sys.stdin)["stage"]=="review"'

echo "[22/29] re-draft first section after review, reapprove exact new version, preserve logical order"
RESP="$(json_post "$CORE/api/projects/$PROJECT_ID/draft-section" '{"section_key":"specific_aims","title":"Specific Aims","additional_context":"Second approved revision used to validate stable document ordering.","high_value":false}')"
NEW_VERSION="$(printf '%s' "$RESP" | python3 -c 'import json,sys; print(json.load(sys.stdin)["version"])')"
curl -fsS "$CORE/api/projects/$PROJECT_ID" | python3 -c 'import json,sys; assert json.load(sys.stdin)["stage"]=="writing"'
json_post "$CORE/api/projects/$PROJECT_ID/sections/specific_aims/approve" "{\"version_id\":$NEW_VERSION}" | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["stage"]=="review"'

echo "[23/29] post-writing research is rejected until discovery is intentionally reopened"
CODE="$(status_post "$CORE/api/projects/$PROJECT_ID/research/run" '{"max_queries":1,"results_per_query":1}')"; test "$CODE" = "409"

echo "[24/29] rendered compliance preflight records page/word measurements"
python3 - "$RENDERER" "$CORE" "$PROJECT_ID" <<'PYMEASURE'
import json,sys,urllib.request
renderer,core,pid=sys.argv[1:]
def get(url):
    with urllib.request.urlopen(url,timeout=120) as r:return json.load(r)
def post(url,payload):
    req=urllib.request.Request(url,data=json.dumps(payload).encode(),headers={'content-type':'application/json'},method='POST')
    with urllib.request.urlopen(req,timeout=240) as r:return json.load(r)
d=get(core+f'/api/projects/{pid}/approved-document');meta=d['project'];sections=d['sections']
m=post(renderer+'/measure',{'project_id':pid,'title':meta['title'],'sponsor':meta.get('sponsor'),'sections':[{'section_key':x['section_key'],'title':x['title'],'body':x['body'],'version':x['version']} for x in sections],'include_document_title':True,'design_profile':d['design_profile']})
assert m['page_count']>0 and m['word_count']>0
assessment=post(core+f'/api/projects/{pid}/compliance/measurements',{'measurements':m})
assert assessment['profile_approved'] and assessment['profile_fresh'] and assessment['hard_failures']==0 and assessment['ready'],assessment
PYMEASURE

echo "[25/29] submission readiness requires deterministic sponsor compliance plus existing gates"
curl -fsS "$CORE/api/projects/$PROJECT_ID/readiness" | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x["ready"] and x["design_profile_present"] and x["required_sections_approved"] and x["clinical_study_present"] and x["clinical_consistent"] and x["competitive_intelligence_fresh"] and x["sponsor_compliance_ready"] and x["sponsor_compliance_hard_failures"]==0,x'

echo "[26/29] immutable export snapshot preserves section order and exact design profile"
curl -fsS -X POST "$CORE/api/projects/$PROJECT_ID/export-snapshot" > "$TMP/snapshot.json"
python3 - "$TMP/snapshot.json" <<'PY'
import json,sys
x=json.load(open(sys.argv[1]));titles=[s['title'] for s in x['sections']]
assert titles==['Specific Aims','Approach'],titles
assert x['snapshot_id']>0 and len(x['sha256'])==64
assert isinstance(x.get('design_profile'),dict) and len(x.get('design_profile_sha256') or '')==64
assert x.get('clinical_study',{}).get('exists') is True and x['clinical_study']['version']==1
assert x.get('competitive_intelligence',{}).get('fresh') is True and x['competitive_intelligence'].get('candidates')
assert x.get('sponsor_compliance_profile',{}).get('approved') is True and x.get('sponsor_compliance_assessment',{}).get('ready') is True
assert x['sections'][0]['version']>x['sections'][1]['version'], 'test expects revised first section to have a newer DB version while remaining first in document order'
PY

echo "[27/29] shared renderer AST produces DOCX + PDF from the same immutable snapshot"
python3 - "$RENDERER" "$PROJECT_ID" "$TMP/snapshot.json" <<'PY'
import json,sys,urllib.request
renderer,pid,path=sys.argv[1:];s=json.load(open(path));meta=s['project'];sections=s['sections']
def post(endpoint,payload):
    req=urllib.request.Request(renderer+endpoint,data=json.dumps(payload).encode(),headers={'content-type':'application/json'},method='POST')
    with urllib.request.urlopen(req,timeout=180) as r:return json.load(r)
base={'project_id':pid,'snapshot_id':s['snapshot_id'],'title':meta['title'],'sponsor':meta.get('sponsor'),'sections':[{'section_key':x['section_key'],'title':x['title'],'body':x['body'],'version':x['version']} for x in sections],'include_document_title':True,'design_profile':s['design_profile']}
preview=post('/preview',{**base,'format':None});ast=preview['ast'];assert ast['blocks'];assert preview['design_profile']==s['design_profile']
paths=[]
for fmt in ('docx','pdf'):
    out=post('/render',{**base,'format':fmt});assert out['ast_version']==ast['version'] and out['snapshot_id']==s['snapshot_id'] and out['path'].endswith('.'+fmt);paths.append(out['path'])
package=post('/package',{'project_id':pid,'snapshot_id':s['snapshot_id'],'title':meta['title'],'generated_paths':paths,'manifest':{'sponsor_compliance_assessment':s.get('sponsor_compliance_assessment')}})
assert package['path'].endswith('_submission_package.zip')
PY

echo "[28/29] project listing exposes persisted project for resume"
curl -fsS "$CORE/api/projects" | python3 - "$PROJECT_ID" <<'PY'
import json,sys
pid=sys.argv[1];x=json.load(sys.stdin);assert any(p['id']==pid and p['stage']=='export' for p in x),x
PY

echo "[29/29] new authoritative source invalidates prior approvals and reopens discovery fail-closed"
json_post "$CORE/api/projects/$PROJECT_ID/documents" '{"name":"late_amendment.txt","kind":"funding_amendment","text":"Amendment: applicants must additionally describe a validated external recruitment site and revised statistical analysis."}' | python3 -c 'import json,sys; assert json.load(sys.stdin)["added"]'
curl -fsS "$CORE/api/projects/$PROJECT_ID/readiness" | python3 -c 'import json,sys; x=json.load(sys.stdin); assert not x["ready"] and x["stage"]=="documents" and not x["requirements_approved"] and not x["required_sections_approved"],x'
curl -fsS "$CORE/api/projects/$PROJECT_ID/sections" | python3 -c 'import json,sys; x=json.load(sys.stdin); assert all(s.get("approved_version") is None for s in x),x'

echo "Phase 7 sponsor compliance and submission package smoke test passed for project $PROJECT_ID"

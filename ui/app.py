import hashlib, html, json, math, os, shutil, requests, gradio as gr
from datetime import date
from pathlib import Path
from typing import Iterable

from bs4 import BeautifulSoup
from docx import Document as DocxDocument
from docx.oxml.table import CT_Tbl
from docx.oxml.text.paragraph import CT_P
from docx.table import Table
from docx.text.paragraph import Paragraph
from pypdf import PdfReader

CORE=os.getenv("CORE_URL","http://core:8080")
RENDERER=os.getenv("RENDERER_URL","http://renderer:8090")
WORKSPACE=Path(os.getenv("GRANT_WORKSPACE","/workspace"))
ORGANIZATION_NAME=os.getenv("ORGANIZATION_NAME","Organization")
CONFIG_ROOT=Path(os.getenv("UI_CONFIG_ROOT",str(Path(__file__).resolve().parents[1]/"config")))
COMPETITIVE_UI_POLL_SECONDS=max(60,min(86400,int(os.getenv("COMPETITIVE_UI_POLL_SECONDS","14400"))))
COMPETITIVE_UPDATE_LABEL=os.getenv("COMPETITIVE_UPDATE_LABEL","Competitive Edge Auto-Update").strip() or "Competitive Edge Auto-Update"
GRANT_BUILD_VERSION=os.getenv("GRANT_BUILD_VERSION","0.8.0")


def load_default_sections():
    raw=os.getenv("GRANT_SECTIONS","").strip()
    if raw:return [x.strip() for x in raw.split(",") if x.strip()]
    try:return [str(x).strip() for x in json.loads((CONFIG_ROOT/"default_sections.json").read_text()) if str(x).strip()]
    except Exception:return []
DEFAULT_SECTIONS=load_default_sections()

CSS="""
.gradio-container{max-width:1720px!important}
.page-frame{background:#e7e7e7;padding:24px;border-radius:14px;overflow:auto}
.page-frame iframe{width:100%;min-height:11.2in;border:0;background:#e7e7e7}
.status{font-size:12px;padding:8px 12px;border-radius:8px;background:#f5f5f5}
.question-card{padding:18px;border:1px solid #ddd;border-radius:10px;background:white}
"""

def api(method,path,**kwargs):
    r=requests.request(method,f"{CORE}{path}",timeout=kwargs.pop("timeout",300),**kwargs)
    if not r.ok:
        try:detail=r.json().get("error",r.text)
        except Exception:detail=r.text
        raise gr.Error(detail)
    return r.json()

def renderer_api(path,payload,timeout=180):
    r=requests.post(f"{RENDERER}{path}",json=payload,timeout=timeout)
    if not r.ok:
        try:detail=r.json().get("detail",r.text)
        except Exception:detail=r.text
        raise gr.Error(detail)
    return r.json()

def file_path(value):
    if value is None:return None
    return str(getattr(value,"name",value))

def iter_docx_blocks(doc):
    for child in doc.element.body.iterchildren():
        if isinstance(child,CT_P):yield Paragraph(child,doc)
        elif isinstance(child,CT_Tbl):yield Table(child,doc)

def extract_docx(path):
    doc=DocxDocument(path);parts=[]
    for block in iter_docx_blocks(doc):
        if isinstance(block,Paragraph):
            if block.text.strip():parts.append(block.text.strip())
        else:
            for row in block.rows:
                cells=[c.text.strip() for c in row.cells]
                if any(cells):parts.append(" | ".join(cells))
    for label,collection in (("HEADER",[s.header for s in doc.sections]),("FOOTER",[s.footer for s in doc.sections])):
        seen=set()
        for item in collection:
            container=[]
            container.extend(p.text.strip() for p in item.paragraphs if p.text.strip())
            for table in item.tables:
                for row in table.rows:
                    cells=[c.text.strip() for c in row.cells]
                    if any(cells):container.append(" | ".join(cells))
            text="\n".join(container)
            if text and text not in seen:parts.append(f"[{label}]\n{text}");seen.add(text)
    return "\n\n".join(parts)

def extract_file(value):
    p=Path(file_path(value));ext=p.suffix.lower()
    if ext==".pdf":
        # Form-feed is an immutable page boundary in the stored source buffer.
        # Rust uses it to derive source_page while copying exact source spans.
        reader=PdfReader(str(p));text="\n\f\n".join((page.extract_text() or "").strip() for page in reader.pages).strip()
        if len(text)<40:raise gr.Error(f"{p.name} has no usable text layer. Please upload a searchable/OCR-processed PDF rather than allowing silent requirement loss.")
        return text
    if ext==".docx":
        text=extract_docx(str(p)).strip()
        if not text:raise gr.Error(f"{p.name} contains no readable paragraphs/tables/headers/footers.")
        return text
    if ext in {".html",".htm"}:return BeautifulSoup(p.read_text(errors="ignore"),"html.parser").get_text("\n",strip=True)
    if ext in {".txt",".md",".csv",".json",".xml"}:return p.read_text(errors="ignore")
    raise gr.Error(f"Unsupported text-ingestion file type: {ext or 'unknown'}. Images belong in Branding/Layout Inspiration, not grant evidence ingestion.")

def push_doc(project,name,kind,text):
    if not text.strip():return False
    return bool(api("POST",f"/api/projects/{project}/documents",json={"name":name,"kind":kind,"text":text}).get("added",False))

def copy_brand_assets(project,assets):
    dest=WORKSPACE/"projects"/project/"branding";dest.mkdir(parents=True,exist_ok=True);out=[]
    for item in assets or []:
        src=Path(file_path(item)); digest=hashlib.sha256(src.read_bytes()).hexdigest()[:12]; target=dest/src.name
        if target.exists() and hashlib.sha256(target.read_bytes()).hexdigest()[:12]!=digest:
            target=dest/f"{src.stem}_{digest}{src.suffix.lower()}"
        if not target.exists():shutil.copy2(src,target)
        out.append(str(target))
    return out

def build_design_profile(project,sponsor,assets):
    return renderer_api("/design-profile",{"project_id":project,"sponsor":sponsor or None,"organization_name":ORGANIZATION_NAME,"asset_paths":assets})

def competitive_update_reason_label(update):
    reasons=set(str(x) for x in ((update or {}).get("refresh_reason") or []))
    if "public_intelligence_refresh_due" in reasons:
        return "Fresh public competitor intelligence was discovered during the scheduled refresh"
    if "competitive_config_changed" in reasons:
        return "Competitive-scoring/search configuration changed and public intelligence was recomputed"
    if "project_inputs_changed" in reasons:
        return "The grant or clinical study changed, so competitor intelligence was rebuilt against the new design"
    if "manual_force" in reasons:
        return "A fresh public competitor scan found a material positioning change"
    return "Fresh public competitive intelligence changed the positioning context"

def section_preview(project,project_title,section_title,body,section_key=None,version=None,competitive_update=None):
    if not project:return '<div class="page-frame"><div style="background:white;padding:32px">Create or open a project to preview a section.</div></div>'
    if competitive_update and competitive_update.get("status")=="pending" and competitive_update.get("base_body") is not None:
        includes_human_edits=bool(version and competitive_update.get("proposed_version") and int(version)!=int(competitive_update.get("proposed_version")))
        d=renderer_api("/preview-diff",{"project_id":project,"title":project_title or "Grant","organization_name":ORGANIZATION_NAME,"section":{"section_key":section_key,"title":section_title or "Section","body":body or "","version":version},"baseline_body":competitive_update.get("base_body") or "","update_summary":competitive_update.get("summary") or "","update_reason":competitive_update_reason_label(competitive_update),"includes_human_edits":includes_human_edits})
    else:
        d=renderer_api("/preview",{"project_id":project,"title":project_title or "Grant","organization_name":ORGANIZATION_NAME,"sections":[{"section_key":section_key,"title":section_title or "Section","body":body or "","version":version}],"include_document_title":False})
    doc=html.escape(d["html"],quote=True)
    return f'<div class="page-frame"><iframe sandbox="" srcdoc="{doc}"></iframe></div>'

def competitive_update_banner(update,current_version=None):
    if not update or update.get("status")!="pending":return ""
    summary=update.get("summary") or "New public competitive information changed the positioning strategy."
    reason=competitive_update_reason_label(update)
    edited_after=bool(current_version and update.get("proposed_version") and int(current_version)!=int(update.get("proposed_version")))
    highlight=("Highlighted text shows all differences from the pre-update version, including edits you made after the automatic proposal."
               if edited_after else
               "Highlighted text shows the proposed changes caused by the refreshed competitive intelligence.")
    return (f"### ⚡ {COMPETITIVE_UPDATE_LABEL}\n"
            f"**{reason}.** {summary}\n\n"
            f"This section was refreshed automatically. **{highlight}** "
            "Review it, edit it if needed, then approve the exact version you want in the final grant. Your previously approved version remains protected until you approve again.")

def global_competitive_update_banner(data):
    if not data:return ""
    pending=int(data.get("pending") or 0)
    processing=int(data.get("processing_pending") or 0)
    events=data.get("events") or []
    latest=events[0] if events else {}
    if pending:
        pending_sections=data.get("pending_sections") or []
        names=[str(x.get("title") or x.get("section_key") or "Section") for x in pending_sections[:8]]
        section_line=("\n\n**Affected sections:** "+", ".join(names)+(f" +{len(pending_sections)-8} more" if len(pending_sections)>8 else "")) if names else ""
        return (f"## ⚡ {COMPETITIVE_UPDATE_LABEL}\n"
                f"Fresh public competitor data produced **{pending} highlighted grant section update(s) waiting for your review.** "
                "Human-approved text was not silently overwritten. Open an affected section to see exactly what changed, then edit or approve it."
                f"{section_line}\n\n{latest.get('summary','')}")
    if processing:
        return (f"## ⚡ {COMPETITIVE_UPDATE_LABEL}\n"
                "A fresh competitive-intelligence update is being reconciled with the grant. The agent will retry any incomplete section refresh automatically; final export remains protected until reconciliation finishes.\n\n"
                f"{latest.get('summary','')}")
    if latest.get("material"):
        return f"**Competitive intelligence is current.** Latest auto-update: {latest.get('summary','')}"
    return ""


def poll_competitive_updates(project):
    if not project:return ""
    try:
        data=api("GET",f"/api/projects/{project}/competitive/updates",timeout=2400)
        return global_competitive_update_banner(data)
    except Exception as exc:
        # Background public-intelligence refresh must not destroy an in-progress edit.
        # Surface the retry state in the banner; the backend remains fail-closed for export.
        return f"## ⚡ {COMPETITIVE_UPDATE_LABEL}\nAutomatic competitor refresh will retry. Current detail: `{html.escape(str(exc))}`"

def full_document_preview(project,payload):
    if not project or not payload.get("sections"):
        return '<div class="page-frame"><div style="background:white;padding:32px">No human-approved sections have been added to the final document yet.</div></div>'
    meta=payload.get("project") or {}
    d=renderer_api("/preview",{
        "project_id":project,
        "title":meta.get("title") or "Grant Application",
        "sponsor":meta.get("sponsor"),
        "organization_name":ORGANIZATION_NAME,
        "sections":[{"section_key":x.get("section_key"),"title":x.get("title") or "Section","body":x.get("body") or "","version":x.get("version")} for x in payload.get("sections",[])],
        "include_document_title":True,
        "design_profile":payload.get("design_profile"),
    })
    doc=html.escape(d["html"],quote=True)
    return f'<div class="page-frame"><iframe sandbox="" srcdoc="{doc}"></iframe></div>'

def requirement_rows(reqs):return [[r.get("id"),r.get("category"),r.get("mandatory"),r.get("requirement"),", ".join(r.get("evidence_needed") or []),r.get("status"),r.get("approved")] for r in reqs]
def evidence_rows(items):return [[e.get("id"),e.get("requirement_id"),e.get("source_type"),e.get("claim"),e.get("status"),round(float(e.get("confidence",0)),2),e.get("url") or ""] for e in items]
def slug(s):
    out=[];last=False
    for ch in (s or ""):
        if ch.isalnum():out.append(ch.lower());last=False
        elif out and not last:out.append("_");last=True
    return "".join(out).strip("_")

def assert_section_identity(section,key):
    expected=slug(section)
    if not expected or key!=expected:
        raise gr.Error("Section state changed while an edit/approval was in progress. Reload the selected section before saving or approving.")


def refresh_projects():
    items=api("GET","/api/projects");choices=[(f"{p['title']} · {p['stage']} · {p['id'][:8]}",p["id"]) for p in items]
    return gr.update(choices=choices,value=(choices[0][1] if choices else None))

def create_project(title,sponsor,mechanism,source,source_url,source_text,supporting,brand):
    if not title.strip():raise gr.Error("Working title is required.")
    if not source and not (source_url or "").strip() and not (source_text or "").strip():raise gr.Error("Upload, link, or paste a funding opportunity.")
    d=api("POST","/api/projects",json={"title":title.strip(),"sponsor":sponsor or None,"mechanism":mechanism or None,"sections":DEFAULT_SECTIONS})
    pid=d["id"];count=0
    if source:
        p=Path(file_path(source));count+=int(push_doc(pid,p.name,"funding_opportunity",extract_file(source)))
    if (source_url or "").strip():
        f=api("POST",f"/api/projects/{pid}/documents/fetch-url",json={"url":source_url.strip(),"kind":"funding_url"},timeout=120);count+=int(f.get("added",False))
    if (source_text or "").strip():
        # Preserve the pasted characters. Trimming here would make later byte
        # offsets refer to a transformed string rather than the user's source.
        count+=int(push_doc(pid,"Pasted funding opportunity","funding_paste",source_text))
    for f in supporting or []:
        p=Path(file_path(f));count+=int(push_doc(pid,p.name,"supporting",extract_file(f)))
    assets=copy_brand_assets(pid,brand);profile=build_design_profile(pid,sponsor,assets)
    api("POST",f"/api/projects/{pid}/design-profile",json={"profile":profile})
    analysis=api("POST",f"/api/projects/{pid}/analyze-requirements",timeout=600)
    comp=api("POST",f"/api/projects/{pid}/compliance/compile",timeout=900)
    sections=api("GET",f"/api/projects/{pid}/sections");section_choices=[x["title"] for x in sections]
    rule_count=len((comp.get("profile") or {}).get("rules") or [])
    return pid,f"Project `{pid}` created. {count} unique source(s), {analysis['count']} atomic grant requirements, and {rule_count} deterministic sponsor/submission rules were compiled from the opportunity. Review and approve both before final submission.","",requirement_rows(analysis["requirements"]),gr.update(choices=section_choices,value=(section_choices[0] if section_choices else None))

def load_project(pid):
    if not pid:raise gr.Error("Choose a project.")
    p=api("GET",f"/api/projects/{pid}");reqs=api("GET",f"/api/projects/{pid}/requirements");sections=api("GET",f"/api/projects/{pid}/sections");choices=[x["title"] for x in sections]
    selected=choices[0] if choices else None
    state=load_section(pid,p["title"],selected) if selected else (None,"",section_preview(pid,p["title"],"Section",""),"No sections configured.","",slug("Section"),None,"")
    notice=global_competitive_update_banner(p.get("competitive_updates") or {})
    return pid,p["title"],p.get("sponsor") or "",p.get("mechanism") or "",f"Opened `{pid}` at workflow stage **{p['stage']}**.",notice,requirement_rows(reqs),gr.update(choices=choices,value=selected),*state

def approve_requirements(project):
    if not project:raise gr.Error("Create or open a project first.")
    d=api("POST",f"/api/projects/{project}/requirements/approve");return f"✓ Human approved {d['approved']} parsed requirements. Workflow stage: `{d['stage']}`."

def next_open(questions):return next((q for q in questions if q.get("status")=="open"),None)
def render_question(q):
    if not q:return '<div class="question-card"><b>Interview complete.</b> No unresolved investigator questions remain.</div>'
    choices=q.get("choices") or [];choice_html=("<br><b>Allowed choices:</b> "+", ".join(html.escape(str(x)) for x in choices)) if choices else "";unit=(f" <b>Unit:</b> {html.escape(str(q['unit']))}" if q.get("unit") else "")
    return f'<div class="question-card"><b>{html.escape(q.get("requirement_id",""))}</b><h3>{html.escape(q.get("question",""))}</h3><p>{html.escape(q.get("why_needed") or "")}</p><small>Type: {html.escape(q.get("answer_type","text"))}{unit}</small>{choice_html}</div>'

def generate_interview(project):
    if not project:raise gr.Error("Create or open a project first.")
    d=api("POST",f"/api/projects/{project}/interview/generate",timeout=600);q=next_open(d.get("questions",[]));return d.get("questions",[]),q,render_question(q),"",f"Generated {d['count']} unresolved question(s) using `{d['model']}`.",d.get("questions",[])

def parse_typed_answer(q,raw):
    t=q.get("answer_type","text");raw=(raw or "").strip()
    if not raw:raise gr.Error("An answer is required. Use classification 'unknown' when the fact is not known.")
    if t=="integer":return int(raw)
    if t in ("number","percentage"):
        v=float(raw)
        if t=="percentage" and not 0<=v<=100:raise gr.Error("Percentage answers must be between 0 and 100.")
        return v
    if t=="boolean":
        v=raw.lower()
        if v in ("true","yes","1"):return True
        if v in ("false","no","0"):return False
        raise gr.Error("Enter true/false or yes/no.")
    if t=="date":
        try:return date.fromisoformat(raw).isoformat()
        except ValueError:raise gr.Error("Use ISO date format YYYY-MM-DD.")
    if t=="choice":
        choices=q.get("choices") or []
        if choices and raw not in choices:raise gr.Error("Answer must exactly match an allowed choice.")
    return raw

def submit_answer(project,questions,current_q,raw,confidence,classification,notes,answered_by):
    if not current_q:return questions,None,render_question(None),"","Interview is already complete."
    value=parse_typed_answer(current_q,raw);d=api("POST",f"/api/projects/{project}/interview/answer",json={"question_id":current_q["id"],"value":value,"confidence":confidence,"classification":classification,"notes":notes or None,"answered_by":answered_by or None})
    refreshed=api("GET",f"/api/projects/{project}/interview");q=next_open(refreshed);return refreshed,q,render_question(q),"",(f"Saved answer. {d['open_questions']} question(s) remain." if q else "✓ Interview complete; research/writing gates are now open.")

def run_research(project,max_queries,results_per):
    d=api("POST",f"/api/projects/{project}/research/run",json={"max_queries":int(max_queries),"results_per_query":int(results_per)},timeout=1200);status=f"Saved {d['sources_saved']} evidence source(s).";status+=(f" {len(d['failures'])} isolated failures." if d.get("failures") else "");return evidence_rows(d.get("evidence",[])),status

def rebuild_index(project):
    d=api("POST",f"/api/projects/{project}/index/rebuild",timeout=1800);return f"✓ Knowledge index: {d['rows']} records × {d['dimensions']} dimensions with `{d['embedding_model']}`.",d

def index_status(project):
    d=api("GET",f"/api/projects/{project}/index/status");return (("✓ Index is current." if d.get("fresh") else "⚠ Index is missing/stale; it will rebuild before retrieval."),d)

def test_retrieval(project,query,k):
    if not (query or "").strip():raise gr.Error("Enter a retrieval query.")
    d=api("POST",f"/api/projects/{project}/retrieve",json={"query":query,"k":int(k)},timeout=1800)
    return [[x.get("score"),x.get("semantic"),x.get("lexical"),x.get("evidence"),x.get("freshness"),x.get("graph_boost"),x.get("record",{}).get("kind"),x.get("record",{}).get("source_ref"),x.get("record",{}).get("text","")[:280]] for x in d]


def _cell(v):
    if v is None:return ""
    if isinstance(v,float) and math.isnan(v):return ""
    s=str(v).strip()
    return "" if s.lower()=="nan" else s

def _opt_float(v):
    s=_cell(v)
    if not s:return None
    x=float(s)
    if not math.isfinite(x):raise gr.Error("Numeric inputs must be finite values.")
    return x

def _opt_int(v):
    x=_opt_float(v)
    if x is None:return None
    if not x.is_integer():raise gr.Error("Integer inputs cannot contain a fractional value.")
    return int(x)

def _lines(v):return [x.strip() for x in (v or "").splitlines() if x.strip()]
def _csv(v):return [x.strip() for x in _cell(v).split(",") if x.strip()]
def _truth(v):
    if isinstance(v,bool):return v
    return str(v or "").strip().lower() in {"true","yes","1","y"}

def _records(table,headers):
    if table is None:return []
    if hasattr(table,"to_dict"):
        try:
            rows=table.to_dict(orient="records")
            return [{h:r.get(h) for h in headers} for r in rows]
        except Exception:pass
    rows=table if isinstance(table,list) else []
    out=[]
    for row in rows:
        if isinstance(row,dict):out.append({h:row.get(h) for h in headers})
        elif isinstance(row,(list,tuple)):out.append({h:(row[i] if i<len(row) else None) for i,h in enumerate(headers)})
    return out

AIM_HEADERS=["Aim ID","Title","Hypothesis","Expected endpoint type","Endpoint IDs","Expected result","Risk","Alternative strategy"]
ARM_HEADERS=["Arm ID","Name","Intervention","Comparator"]
ENDPOINT_HEADERS=["Endpoint ID","Name","Type","Primary","Analysis family"]
TIMELINE_HEADERS=["Task ID","Name","Start month","Duration months","Dependencies"]
RESOURCE_HEADERS=["Resource ID","Name","Required","Available","Notes"]

def build_clinical_study(clinical_problem,knowledge_gap,central_hypothesis,disease,disease_stage,biomarker,inclusion,exclusion,
                         design_type,study_phase,randomization,allocation_ratio,blinding,follow_up_months,design_sites,
                         available_patients,eligibility_pct,biomarker_pct,consent_pct,target_enrollment,accrual_months,recruitment_sites,
                         test_type,alpha,power,attrition_pct,control_rate,treatment_rate,null_rate,alternative_rate,mean_delta,std_dev,hazard_ratio,event_probability,
                         aims_table,arms_table,endpoints_table,timeline_table,resources_table):
    aims=[]
    for r in _records(aims_table,AIM_HEADERS):
        aid=_cell(r.get("Aim ID"))
        if not aid:continue
        aims.append({"aim_id":aid,"title":_cell(r.get("Title")),"hypothesis":_cell(r.get("Hypothesis")),"expected_endpoint_type":_cell(r.get("Expected endpoint type")),"endpoint_ids":_csv(r.get("Endpoint IDs")),"expected_result":_cell(r.get("Expected result")),"risk":_cell(r.get("Risk")),"alternative_strategy":_cell(r.get("Alternative strategy"))})
    arms=[]
    for r in _records(arms_table,ARM_HEADERS):
        aid=_cell(r.get("Arm ID"));name=_cell(r.get("Name"))
        if not aid and not name:continue
        arms.append({"arm_id":aid,"name":name,"intervention":_cell(r.get("Intervention")),"comparator":_truth(r.get("Comparator"))})
    endpoints=[]
    for r in _records(endpoints_table,ENDPOINT_HEADERS):
        eid=_cell(r.get("Endpoint ID"));name=_cell(r.get("Name"))
        if not eid and not name:continue
        endpoints.append({"endpoint_id":eid,"name":name,"endpoint_type":_cell(r.get("Type")),"primary":_truth(r.get("Primary")),"analysis_family":_cell(r.get("Analysis family"))})
    timeline=[]
    for r in _records(timeline_table,TIMELINE_HEADERS):
        tid=_cell(r.get("Task ID"));name=_cell(r.get("Name"))
        if not tid and not name:continue
        timeline.append({"task_id":tid,"name":name,"start_month":float(_cell(r.get("Start month")) or 0),"duration_months":float(_cell(r.get("Duration months")) or 0),"dependencies":_csv(r.get("Dependencies"))})
    resources=[]
    for r in _records(resources_table,RESOURCE_HEADERS):
        rid=_cell(r.get("Resource ID"));name=_cell(r.get("Name"))
        if not rid and not name:continue
        resources.append({"resource_id":rid,"name":name,"required":_truth(r.get("Required")),"available":_truth(r.get("Available")),"notes":_cell(r.get("Notes"))})
    return {
      "clinical_problem":clinical_problem or "","knowledge_gap":knowledge_gap or "","central_hypothesis":central_hypothesis or "",
      "population":{"disease":disease or "","stage":disease_stage or "","biomarker_criteria":biomarker or "","inclusion_criteria":_lines(inclusion),"exclusion_criteria":_lines(exclusion)},
      "design":{"design_type":design_type or "","phase":study_phase or "","randomization":randomization or "","allocation_ratio":allocation_ratio or "","blinding":blinding or "","follow_up_months":_opt_float(follow_up_months),"sites":_opt_int(design_sites)},
      "recruitment":{"available_patients_per_site_month":_opt_float(available_patients),"eligibility_rate_pct":_opt_float(eligibility_pct),"biomarker_positive_rate_pct":_opt_float(biomarker_pct),"consent_rate_pct":_opt_float(consent_pct),"target_enrollment":_opt_int(target_enrollment),"accrual_months":_opt_float(accrual_months),"sites":_opt_int(recruitment_sites)},
      "statistics":{"test_type":test_type or "","alpha":_opt_float(alpha),"power":_opt_float(power),"attrition_pct":_opt_float(attrition_pct),"control_rate":_opt_float(control_rate),"treatment_rate":_opt_float(treatment_rate),"null_rate":_opt_float(null_rate),"alternative_rate":_opt_float(alternative_rate),"mean_delta":_opt_float(mean_delta),"std_dev":_opt_float(std_dev),"hazard_ratio":_opt_float(hazard_ratio),"event_probability":_opt_float(event_probability)},
      "aims":aims,"arms":arms,"endpoints":endpoints,"timeline":timeline,"resources":resources
    }

def clinical_summary(a):
    if not a:return "No clinical assessment available."
    rec=a.get("recruitment") or {};bits=[]
    if rec.get("complete"):
        bits.append(f"**Recruitment:** expected {rec.get('expected_enrollments_per_month',0):.2f}/month; required {rec.get('required_enrollments_per_month') if rec.get('required_enrollments_per_month') is not None else 'not configured'}/month; estimated accrual {rec.get('estimated_accrual_months') if rec.get('estimated_accrual_months') is not None else 'not configured'} months.")
        if rec.get("feasible_within_planned_window") is False:bits.append("⚠ Recruitment assumptions do not meet the planned accrual window.")
    stats=a.get("statistics") or {}
    if stats.get("complete"):bits.append(f"**Sample size:** raw N={stats.get('raw_total_n')}, attrition-adjusted N={stats.get('adjusted_total_n')} ({stats.get('method')}).")
    errs=a.get("errors") or [];warn=a.get("warnings") or [];conf=(a.get("cross_section_consistency") or {}).get("count",0)
    bits.append(f"**Deterministic checks:** {len(errs)} error(s), {len(warn)} warning(s), {conf} approved-section consistency conflict(s).")
    return "\n\n".join(bits)

def _csv_numbers(v,integer=False):
    out=[]
    for x in _csv(v):out.append(int(float(x)) if integer else float(x))
    return out

def run_feasibility_scenarios(project,sites_values,consent_values,biomarker_values):
    if not project:raise gr.Error("Create or open a project first.")
    d=api("POST",f"/api/projects/{project}/clinical/scenarios",json={"sites":_csv_numbers(sites_values,True),"consent_rates_pct":_csv_numbers(consent_values),"biomarker_positive_rates_pct":_csv_numbers(biomarker_values)})
    rows=[[x.get("sites"),x.get("consent_rate_pct"),x.get("biomarker_positive_rate_pct"),x.get("expected_enrollments_per_month"),x.get("required_enrollments_per_month"),x.get("estimated_accrual_months"),x.get("feasible"),x.get("shortfall_per_month")] for x in d.get("rows",[])]
    return rows,f"Evaluated **{d.get('combinations',0)}** recruitment scenarios in the Rust/Rayon clinical engine."

def save_clinical_study(project,*values):
    if not project:raise gr.Error("Create or open a project first.")
    study=build_clinical_study(*values)
    d=api("POST",f"/api/projects/{project}/clinical-study",json=study,timeout=300)
    a=d.get("assessment") or {}
    return a,clinical_summary(a)+f"\n\nSaved clinical study version **{d['saved']['version']}** (`{d['saved']['sha256'][:16]}…`)."

def load_clinical_study(project):
    if not project:raise gr.Error("Create or open a project first.")
    d=api("GET",f"/api/projects/{project}/clinical-study")
    if not d.get("exists"):
        return ("","","","","","","","","","","","","",None,None,None,None,None,None,None,None,None,None,"",None,None,None,None,None,None,None,None,None,None,[],[],[],[],[],{},"No saved clinical study exists yet.")
    st=d["study"];p=st.get("population") or {};des=st.get("design") or {};r=st.get("recruitment") or {};sp=st.get("statistics") or {}
    aims=[[x.get("aim_id"),x.get("title"),x.get("hypothesis"),x.get("expected_endpoint_type"),", ".join(x.get("endpoint_ids") or []),x.get("expected_result"),x.get("risk"),x.get("alternative_strategy")] for x in st.get("aims") or []]
    arms=[[x.get("arm_id"),x.get("name"),x.get("intervention"),x.get("comparator")] for x in st.get("arms") or []]
    eps=[[x.get("endpoint_id"),x.get("name"),x.get("endpoint_type"),x.get("primary"),x.get("analysis_family")] for x in st.get("endpoints") or []]
    timeline=[[x.get("task_id"),x.get("name"),x.get("start_month"),x.get("duration_months"),", ".join(x.get("dependencies") or [])] for x in st.get("timeline") or []]
    resources=[[x.get("resource_id"),x.get("name"),x.get("required"),x.get("available"),x.get("notes")] for x in st.get("resources") or []]
    a=api("GET",f"/api/projects/{project}/clinical-assessment")
    return (st.get("clinical_problem",""),st.get("knowledge_gap",""),st.get("central_hypothesis",""),p.get("disease",""),p.get("stage",""),p.get("biomarker_criteria",""),"\n".join(p.get("inclusion_criteria") or []),"\n".join(p.get("exclusion_criteria") or []),des.get("design_type",""),des.get("phase",""),des.get("randomization",""),des.get("allocation_ratio",""),des.get("blinding",""),des.get("follow_up_months"),des.get("sites"),r.get("available_patients_per_site_month"),r.get("eligibility_rate_pct"),r.get("biomarker_positive_rate_pct"),r.get("consent_rate_pct"),r.get("target_enrollment"),r.get("accrual_months"),r.get("sites"),sp.get("test_type",""),sp.get("alpha"),sp.get("power"),sp.get("attrition_pct"),sp.get("control_rate"),sp.get("treatment_rate"),sp.get("null_rate"),sp.get("alternative_rate"),sp.get("mean_delta"),sp.get("std_dev"),sp.get("hazard_ratio"),sp.get("event_probability"),aims,arms,eps,timeline,resources,a,clinical_summary(a)+f"\n\nLoaded clinical study version **{d['version']}**.")

def calculate_sample_size(project,test_type,alpha,power,attrition_pct,control_rate,treatment_rate,null_rate,alternative_rate,mean_delta,std_dev,hazard_ratio,event_probability):
    plan={"test_type":test_type or "","alpha":_opt_float(alpha),"power":_opt_float(power),"attrition_pct":_opt_float(attrition_pct),"control_rate":_opt_float(control_rate),"treatment_rate":_opt_float(treatment_rate),"null_rate":_opt_float(null_rate),"alternative_rate":_opt_float(alternative_rate),"mean_delta":_opt_float(mean_delta),"std_dev":_opt_float(std_dev),"hazard_ratio":_opt_float(hazard_ratio),"event_probability":_opt_float(event_probability)}
    d=api("POST",f"/api/projects/{project}/clinical/sample-size",json=plan)
    return d,(f"Calculated **N={d.get('adjusted_total_n')}** including attrition using {d.get('method')}." if d.get("complete") else "Statistics inputs are incomplete.")


def competitive_candidate_rows(data):
    rows=[]
    for c in data.get("candidates") or []:
        rows.append([
            c.get("rank"),c.get("name"),round(float(c.get("overall_score",0)),3),
            round(float(c.get("grant_score",0)),3),round(float(c.get("publication_score",0)),3),
            round(float(c.get("clinical_trial_score",0)),3),round(float(c.get("patent_ip_score",0)),3),
            round(float(c.get("technology_score",0)),3),round(float(c.get("breadth_score",0)),3),c.get("asset_count",0)
        ])
    return rows

def competitive_asset_rows(data,limit=250):
    names={c.get("candidate_key"):c.get("name") for c in data.get("candidates") or []}
    rows=[]
    for a in (data.get("assets") or [])[:limit]:
        rows.append([names.get(a.get("candidate_key"),a.get("candidate_key")),a.get("asset_type"),round(float(a.get("relevance",0)),3),a.get("title"),a.get("year"),a.get("provider"),a.get("url") or ""])
    return rows

def competitive_strategy_markdown(data):
    if not data.get("exists"):
        return "No competitive applicant intelligence has been run yet."
    strategy=data.get("strategy") or {}
    lines=["### Evidence-backed competitive positioning","> **Important:** organizations below are capability-overlap candidates inferred from public evidence, **not confirmed applicants**."]
    if strategy.get("market_summary"):lines.extend(["",strategy["market_summary"]])
    if strategy.get("positioning_principles"):
        lines.extend(["","#### Positioning principles"]+[f"- {x}" for x in strategy.get("positioning_principles") or []])
    if strategy.get("differentiators"):
        lines.extend(["","#### Differentiators to emphasize"])
        for x in strategy["differentiators"]:
            refs=", ".join(x.get("asset_keys") or [])
            lines.append(f"- **{x.get('theme','Differentiator')}** — {x.get('our_advantage','')}"+(f"  Public evidence: `{refs}`" if refs else ""))
    if strategy.get("gaps_to_close"):
        lines.extend(["","#### Competitive gaps to close"])
        for x in strategy["gaps_to_close"]:
            refs=", ".join(x.get("asset_keys") or [])
            lines.append(f"- **{x.get('gap','Gap')}** — {x.get('recommended_action','')}"+(f"  Public evidence: `{refs}`" if refs else ""))
    if strategy.get("candidate_notes"):
        lines.extend(["","#### Capability-matched organization notes"] )
        for x in strategy["candidate_notes"]:
            refs=", ".join(x.get("asset_keys") or [])
            lines.append(f"- `{x.get('candidate_key','candidate')}` — {x.get('why_relevant','')} **Positioning:** {x.get('how_to_outposition','')}"+(f"  Evidence: `{refs}`" if refs else ""))
    if strategy.get("section_guidance"):
        lines.extend(["","#### Section-specific positioning guidance"] )
        for x in strategy["section_guidance"]:
            refs=", ".join(x.get("asset_keys") or [])
            lines.append(f"- **{x.get('section_key','section')}** — {x.get('guidance','')}"+(f"  Evidence: `{refs}`" if refs else ""))
    if strategy.get("do_not_claim"):
        lines.extend(["","#### Do not claim"]+[f"- {x}" for x in strategy.get("do_not_claim") or []])
    return "\n".join(lines)

def generate_competitive_profile(project):
    if not project:raise gr.Error("Create or open a project first.")
    d=api("POST",f"/api/projects/{project}/competitive/profile/generate",timeout=900)
    return d,f"Generated competitive applicant capability profile version **{d.get('version')}** using `{d.get('model')}`. This describes public capabilities to search for; it does not assert who will apply."

def load_competitive(project):
    if not project:raise gr.Error("Create or open a project first.")
    # Self-healing load: the backend refreshes stale/expired/config-changed public intelligence before returning.
    data=api("GET",f"/api/projects/{project}/competitive",timeout=2400)
    profile=api("GET",f"/api/projects/{project}/competitive/profile")
    refreshed=data.get("auto_refreshed",False)
    reason=", ".join(data.get("refresh_reason") or [])
    if data.get("exists"):
        status=f"Competitive run **{data.get('run_id')}** is current."
        if refreshed: status+=f" Automatically refreshed{(' ('+reason+')') if reason else ''}."
        status+=f" Public-source age: **{data.get('age_seconds','n/a')}s**; refresh TTL: **{data.get('refresh_ttl_seconds','n/a')}s**."
    else:
        status="No competitive intelligence run exists yet."
    return profile,competitive_candidate_rows(data),competitive_asset_rows(data),data.get("provider_status") or [],competitive_strategy_markdown(data),data,status,global_competitive_update_banner(data.get("competitive_updates") or {})

def run_competitive_intelligence(project):
    if not project:raise gr.Error("Create or open a project first.")
    data=api("POST",f"/api/projects/{project}/competitive/refresh",json={},timeout=2400)
    providers=data.get("provider_status") or []
    ok=sum(1 for p in providers if p.get("ok")); total=len(providers)
    status=f"Competitive intelligence run **{data.get('run_id')}** completed with **{len(data.get('candidates') or [])}** capability-matched organizations and **{len(data.get('assets') or [])}** public assets. Providers healthy: {ok}/{total}."
    changed=((data.get("agentic_update") or {}).get("section_updates") or [])
    if changed:status+=f" **{COMPETITIVE_UPDATE_LABEL} refreshed {len(changed)} existing grant section(s); highlighted changes now require human review.**"
    return competitive_candidate_rows(data),competitive_asset_rows(data),providers,competitive_strategy_markdown(data),data,status,global_competitive_update_banner(data.get("competitive_updates") or {})

def load_section(project,project_title,section_title):
    if not project or not section_title:return None,"",section_preview(project,project_title,section_title or "Section",""),"No section selected.","",slug(section_title or "Section"),None,""
    key=slug(section_title);d=api("GET",f"/api/projects/{project}/sections/{key}",timeout=2400)
    latest=d.get("latest") if d.get("exists") else None;approved=d.get("approved") if d.get("exists") else None;update=d.get("competitive_update") or None
    body=(latest or {}).get("body","");version=(latest or {}).get("version")
    status=f"Latest version: {version or 'none'} · approved version: {(approved or {}).get('version','none')}"
    if update:status+=f" · competitive update event {update.get('event_id')} requires review"
    return version,body,section_preview(project,project_title,section_title,body,key,version,update),status,body,key,(update or {}).get("event_id"),competitive_update_banner(update,version)


COMPLIANCE_HEADERS=["Rule ID","Category","Type","Scope","Target","Severity","Mandatory","Numeric value","Text value","List value","Source hint","Document hint","Page hint","Notes"]

def compliance_rule_rows(profile):
    rows=[]
    for r in (profile or {}).get("rules") or []:
        rows.append([r.get("rule_id"),r.get("category"),r.get("rule_type"),r.get("scope"),r.get("target"),r.get("severity"),r.get("mandatory"),r.get("numeric_value"),r.get("text_value"),", ".join(r.get("list_value") or []),r.get("source_hint"),r.get("source_document_hint"),r.get("source_page_hint"),r.get("notes")])
    return rows

def compliance_provenance_rows(profile):
    return [[r.get("rule_id"),r.get("source_status"),r.get("source_document_id"),r.get("source_page"),r.get("source_start_offset"),r.get("source_end_offset"),r.get("source_excerpt")] for r in (profile or {}).get("rules") or []]

def build_compliance_profile(sponsor,mechanism,submission_system,deadline,table):
    rules=[]
    for r in _records(table,COMPLIANCE_HEADERS):
        rid=_cell(r.get("Rule ID"))
        if not rid:continue
        rules.append({"rule_id":rid,"category":_cell(r.get("Category")),"rule_type":_cell(r.get("Type")),"scope":_cell(r.get("Scope")),"target":_cell(r.get("Target")),"severity":_cell(r.get("Severity")) or "hard","mandatory":_truth(r.get("Mandatory")),"numeric_value":_opt_float(r.get("Numeric value")),"text_value":(_cell(r.get("Text value")) or None),"list_value":_csv(r.get("List value")),"source_hint":_cell(r.get("Source hint")),"source_document_hint":(_cell(r.get("Document hint")) or None),"source_page_hint":_opt_int(r.get("Page hint")),"notes":_cell(r.get("Notes"))})
    return {"sponsor":_cell(sponsor) or None,"mechanism":_cell(mechanism) or None,"submission_system":_cell(submission_system) or None,"deadline_iso":_cell(deadline) or None,"rules":rules}

def compliance_finding_rows(a):
    return [[x.get("rule_id"),x.get("severity"),x.get("mandatory"),x.get("status"),x.get("rule_type"),x.get("target"),x.get("detail"),x.get("source_excerpt")] for x in (a or {}).get("findings") or []]

def load_compliance(project):
    if not project:raise gr.Error("Create or open a project first.")
    d=api("GET",f"/api/projects/{project}/compliance")
    profile=d.get("profile") or {}
    assessment=api("GET",f"/api/projects/{project}/compliance/assessment")
    source=api("GET",f"/api/projects/{project}/opportunity-source")
    artifacts=api("GET",f"/api/projects/{project}/submission-artifacts")
    status=(f"Compliance profile v{d.get('version')} · {'approved' if d.get('approved') else 'awaiting human approval'} · {'fresh' if d.get('fresh') else 'STALE — recompile from the current opportunity source'}. "
            f"Hard failures: {assessment.get('hard_failures',0)}.")
    return profile,source.get("text") or "",profile.get("sponsor") or "",profile.get("mechanism") or "",profile.get("submission_system") or "",profile.get("deadline_iso") or "",compliance_rule_rows(profile),compliance_provenance_rows(profile),compliance_finding_rows(assessment),assessment,[[x.get("slot"),x.get("filename"),x.get("extension"),x.get("sha256")] for x in artifacts],status

def compile_compliance(project):
    d=api("POST",f"/api/projects/{project}/compliance/compile",timeout=900);profile=d.get("profile") or {};a=api("GET",f"/api/projects/{project}/compliance/assessment");source=api("GET",f"/api/projects/{project}/opportunity-source")
    sections=api("GET",f"/api/projects/{project}/sections");choices=[x.get("title") for x in sections if x.get("title")]
    missing=sum(1 for r in profile.get("rules") or [] if r.get("source_status")!="located")
    return profile,source.get("text") or "",profile.get("sponsor") or "",profile.get("mechanism") or "",profile.get("submission_system") or "",profile.get("deadline_iso") or "",compliance_rule_rows(profile),compliance_provenance_rows(profile),compliance_finding_rows(a),a,f"Recompiled {len(profile.get('rules') or [])} sponsor rules; {missing} require source-location review. Human approval is required.",gr.update(choices=choices)

def save_compliance(project,sponsor,mechanism,submission_system,deadline,table):
    profile=build_compliance_profile(sponsor,mechanism,submission_system,deadline,table)
    d=api("POST",f"/api/projects/{project}/compliance",json={"profile":profile},timeout=300);a=api("GET",f"/api/projects/{project}/compliance/assessment")
    sections=api("GET",f"/api/projects/{project}/sections");choices=[x.get("title") for x in sections if x.get("title")]
    saved=d.get("profile") or profile
    return saved,compliance_rule_rows(saved),compliance_provenance_rows(saved),compliance_finding_rows(a),a,f"Saved human-reviewed compliance profile v{d.get('version')}. Approval is still required.",gr.update(choices=choices)

def approve_compliance(project):
    d=api("POST",f"/api/projects/{project}/compliance/approve");a=api("GET",f"/api/projects/{project}/compliance/assessment")
    return compliance_provenance_rows(d.get("profile") or {}),compliance_finding_rows(a),a,f"✓ Human approved sponsor compliance profile v{d.get('version')}. Deterministic hard-rule failures remaining: {a.get('hard_failures',0)}."

def resolve_compliance(project,rule_id,status,notes,resolved_by):
    if not (rule_id or "").strip():raise gr.Error("Enter the Rule ID to resolve.")
    a=api("POST",f"/api/projects/{project}/compliance/resolve",json={"rule_id":rule_id.strip(),"status":status,"notes":notes or None,"resolved_by":resolved_by or None})
    return compliance_finding_rows(a),a,f"Saved manual resolution `{status}` for {rule_id}."

def register_submission_artifacts(project,slot,files):
    if not project:raise gr.Error("Open a project first.")
    slot=slug(slot)
    if not slot:raise gr.Error("Enter a submission slot, e.g. letters_of_support or biosketches.")
    dest=WORKSPACE/"projects"/project/"submission"/slot;dest.mkdir(parents=True,exist_ok=True)
    for item in files or []:
        src=Path(file_path(item));digest=hashlib.sha256(src.read_bytes()).hexdigest();target=dest/src.name
        if target.exists() and hashlib.sha256(target.read_bytes()).hexdigest()!=digest:target=dest/f"{src.stem}_{digest[:12]}{src.suffix.lower()}"
        if not target.exists():shutil.copy2(src,target)
        api("POST",f"/api/projects/{project}/submission-artifacts",json={"slot":slot,"filename":target.name,"path":str(target),"sha256":digest,"extension":target.suffix.lower().lstrip('.')})
    artifacts=api("GET",f"/api/projects/{project}/submission-artifacts");a=api("GET",f"/api/projects/{project}/compliance/assessment")
    return [[x.get("slot"),x.get("filename"),x.get("extension"),x.get("sha256")] for x in artifacts],compliance_finding_rows(a),a,f"Registered {len(files or [])} submission artifact(s) under `{slot}`."

def refresh_compliance_measurements(project):
    d=api("GET",f"/api/projects/{project}/approved-document",timeout=2400)
    sections=d.get("sections") or []
    if not sections:return api("GET",f"/api/projects/{project}/compliance/assessment")
    meta=d.get("project") or {}
    m=renderer_api("/measure",{"project_id":project,"title":meta.get("title") or "Grant Application","sponsor":meta.get("sponsor"),"organization_name":ORGANIZATION_NAME,"sections":[{"section_key":x.get("section_key"),"title":x.get("title") or "Section","body":x.get("body") or "","version":x.get("version")} for x in sections],"include_document_title":True,"design_profile":d.get("design_profile")},timeout=240)
    return api("POST",f"/api/projects/{project}/compliance/measurements",json={"measurements":m})

def measure_compliance(project):
    a=refresh_compliance_measurements(project)
    return compliance_finding_rows(a),a,f"Rendered compliance preflight: {a.get('passed',0)} passed, {a.get('hard_failures',0)} hard failure(s), {a.get('deferred',0)} deferred."

def draft_section(project,project_title,section,additional_context,high_value):
    key=slug(section);d=api("POST",f"/api/projects/{project}/draft-section",json={"section_key":key,"title":section,"additional_context":additional_context or None,"high_value":bool(high_value)},timeout=2400)
    state=api("GET",f"/api/projects/{project}/sections/{key}",timeout=2400);latest=state.get("latest") or {};update=state.get("competitive_update") or None
    body=latest.get("body",d["text"]);version=latest.get("version",d["version"])
    return version,body,key,section_preview(project,project_title,section,body,key,version,update),f"Draft version {version} created with `{d['model']}`. Click ✎ to edit or ✓ to approve this exact version.",gr.update(value=body,visible=False),gr.update(visible=False),gr.update(visible=False),(update or {}).get("event_id"),competitive_update_banner(update,version)

def show_editor(baseline):return gr.update(value=baseline or "",visible=True),gr.update(visible=True),gr.update(visible=True)

def save_edit(project,project_title,section,key,current_version,baseline,body):
    assert_section_identity(section,key)
    body=body or ""
    if body==baseline:
        state=api("GET",f"/api/projects/{project}/sections/{key}",timeout=2400);update=state.get("competitive_update") or None
        return current_version,baseline,section_preview(project,project_title,section,baseline,key,current_version,update),f"No changes detected; version {current_version} remains current.",gr.update(value=baseline,visible=False),gr.update(visible=False),gr.update(visible=False),(update or {}).get("event_id"),competitive_update_banner(update,current_version)
    d=api("POST",f"/api/projects/{project}/sections/{key}",json={"title":section,"body":body,"html":None,"base_version_id":int(current_version) if current_version else None},timeout=2400);v=d["version"]
    state=api("GET",f"/api/projects/{project}/sections/{key}",timeout=2400);update=state.get("competitive_update") or None
    return v,body,section_preview(project,project_title,section,body,key,v,update),f"Saved human edit as version {v}; approval is still required.",gr.update(value=body,visible=False),gr.update(visible=False),gr.update(visible=False),(update or {}).get("event_id"),competitive_update_banner(update,v)

def cancel_edit(project,project_title,section,key,current_version,baseline):
    assert_section_identity(section,key)
    state=api("GET",f"/api/projects/{project}/sections/{key}",timeout=2400);update=state.get("competitive_update") or None
    return section_preview(project,project_title,section,baseline,key,current_version,update),gr.update(value=baseline,visible=False),gr.update(visible=False),gr.update(visible=False),"Edit canceled; the persisted version was restored."

def approve_section(project,project_title,section,key,current_version,baseline,editor_body,competitive_update_event_id):
    assert_section_identity(section,key)
    if not current_version:raise gr.Error("Generate or save a section version before approval.")
    body=editor_body if editor_body is not None else baseline;version=current_version
    if body!=baseline:
        saved=api("POST",f"/api/projects/{project}/sections/{key}",json={"title":section,"body":body,"html":None,"base_version_id":int(current_version)},timeout=2400);version=saved["version"];baseline=body
    d=api("POST",f"/api/projects/{project}/sections/{key}/approve",json={"version_id":int(version),"competitive_update_event_id":int(competitive_update_event_id) if competitive_update_event_id else None},timeout=2400)
    return version,baseline,section_preview(project,project_title,section,baseline,key,version,None),f"✓ Human approved exact version {d['approved_version']} for {section}. Workflow stage: `{d['stage']}`.",gr.update(value=baseline,visible=False),gr.update(visible=False),gr.update(visible=False),None,""

def preview_approved_grant(project):
    if not project:raise gr.Error("Create or open a project first.")
    d=api("GET",f"/api/projects/{project}/approved-document")
    plan=d.get("section_plan") or []
    approved_by_key={x.get("section_key"):x for x in d.get("sections") or []}
    rows=[]
    for item in plan:
        approved=approved_by_key.get(item.get("section_key"))
        rows.append([
            item.get("position"),
            item.get("title"),
            "✓ Approved" if approved else "Not approved",
            approved.get("version") if approved else None,
        ])
    counts=d.get("counts") or {}
    status=f"Aggregating **{counts.get('approved',0)} / {counts.get('configured',0)}** configured sections from human-approved versions only."
    if d.get("readiness",{}).get("ready"):
        status += " All required sections are approved; the final DOCX/PDF choice is available."
    return rows,full_document_preview(project,d),status

def readiness(project):
    try:refresh_compliance_measurements(project)
    except Exception:pass
    d=api("GET",f"/api/projects/{project}/readiness",timeout=2400)
    ready=bool(d.get("ready"))
    if ready:
        status="✓ All workflow gates passed. Choose the final format below."
    elif int(d.get("competitive_text_updates_pending") or 0)>0:
        status=f"⚡ **{COMPETITIVE_UPDATE_LABEL}:** {d.get('competitive_text_updates_pending')} refreshed section(s) are waiting for human review. Open the highlighted updates, edit if needed, and approve before export."
    elif int(d.get("competitive_refresh_processing_pending") or 0)>0:
        status="⚡ Competitive intelligence found new public information and is still self-healing affected section text. Retry shortly; export remains fail-closed until the update completes."
    elif not d.get("sponsor_compliance_ready"):
        status=f"Sponsor compliance is not ready: {d.get('sponsor_compliance_hard_failures')} hard rule failure(s). Open **Sponsor Compliance & Submission**, resolve the highlighted rules, then rerun readiness."
    else:
        status="Not export-ready: "+json.dumps(d,indent=2)
    return d,status,gr.update(visible=ready),gr.update(visible=ready)

def system_diagnostics():
    info=api("GET","/api/system/info",timeout=60)
    summary=(f"**Build:** {info.get('build_version')} · **Runtime:** {info.get('runtime_profile')} · "
             f"**Model routing:** {info.get('model_routing_mode')} · **OMP:** {info.get('omp_threads')} · "
             f"**Rayon:** {info.get('rayon_threads')} · **DB:** {int(info.get('database_bytes') or 0)/1024/1024:.1f} MB")
    return info,summary

def run_hpc_diagnostics():
    data=api("POST","/api/hpc/benchmark",timeout=600)
    summary=(f"HPC benchmark completed: normalize {float(data.get('normalize_ms') or 0):.1f} ms · "
             f"SGEMV {float(data.get('sgemv_ms') or 0):.1f} ms · mmap open {float(data.get('mmap_open_ms') or 0):.2f} ms · "
             f"mmap score {float(data.get('mmap_score_ms') or 0):.1f} ms.")
    return data,summary

def export(project,fmt,_title):
    refresh_compliance_measurements(project)
    snap=api("POST",f"/api/projects/{project}/export-snapshot");meta=snap["project"];sections=snap["sections"];fmts=["docx","pdf"] if fmt=="BOTH" else [fmt.lower()];paths=[]
    final_title=meta.get("title") or "Grant Application"
    for f in fmts:
        payload={"project_id":project,"snapshot_id":snap["snapshot_id"],"format":f,"title":final_title,"sponsor":meta.get("sponsor"),"organization_name":ORGANIZATION_NAME,"sections":[{"section_key":x.get("section_key"),"title":x["title"],"body":x["body"],"version":x.get("version")} for x in sections],"include_document_title":True,"design_profile":snap.get("design_profile")}
        paths.append(renderer_api("/render",payload,timeout=240)["path"])
    package=renderer_api("/package",{"project_id":project,"snapshot_id":snap["snapshot_id"],"title":final_title,"generated_paths":paths,"manifest":{"snapshot_sha256":snap.get("sha256"),"sponsor_compliance_profile":snap.get("sponsor_compliance_profile"),"sponsor_compliance_assessment":snap.get("sponsor_compliance_assessment"),"submission_artifacts":snap.get("submission_artifacts")}},timeout=240)["path"]
    paths.append(package)
    return paths,f"Created from immutable export snapshot {snap['snapshot_id']} ({snap['sha256'][:16]}…). Sponsor-compliant package: {package}"

with gr.Blocks(title="Grant Workbench") as demo:
    gr.Markdown(f"# {ORGANIZATION_NAME} Grant Workbench\nLocal OLMo/Apple-MLX drafting + Claude research and high-value synthesis + enforced human approval · Build {GRANT_BUILD_VERSION}")
    project_id=gr.State("");interview_questions=gr.State([]);current_question=gr.State(None);current_version=gr.State(None);baseline_body=gr.State("");current_section_key=gr.State("");current_competitive_update_event=gr.State(None)
    with gr.Row():
        recent=gr.Dropdown(label="Open existing project",choices=[]);refresh_projects_btn=gr.Button("Refresh Projects");open_project_btn=gr.Button("Open Project",variant="secondary")
    agentic_global_notice=gr.Markdown()
    competitive_update_timer=gr.Timer(value=COMPETITIVE_UI_POLL_SECONDS,active=True)
    with gr.Tabs():
        with gr.Tab("1 · Intake & Requirements"):
            with gr.Row():
                with gr.Column(scale=2):
                    project_title=gr.Textbox(label="Working title");sponsor=gr.Textbox(label="Sponsor");mechanism=gr.Textbox(label="Mechanism")
                    gr.Markdown("""### Grant Opportunity
Use whichever input is easiest. Upload, URL, and pasted text are normalized into the same grant-opportunity source before requirements and deterministic submission rules are compiled.""")
                    with gr.Tabs():
                        with gr.Tab("Upload"):
                            source=gr.File(label="Upload RFA / NOFO / grant opportunity (PDF, DOCX, TXT, HTML)",file_count="single",type="filepath")
                        with gr.Tab("URL"):
                            source_url=gr.Textbox(label="Public grant opportunity URL",placeholder="https://...")
                        with gr.Tab("Paste Text"):
                            source_text=gr.Textbox(label="Paste the grant opportunity",lines=16,placeholder="Paste the full funding opportunity, sponsor instructions, or relevant grant announcement text here…")
                    supporting=gr.File(label="Relevant institutional/project materials",file_count="multiple",type="filepath");brand=gr.File(label="Branding/layout inspiration",file_count="multiple",type="filepath")
                    create=gr.Button("Create / Analyze Grant",variant="primary");project_status=gr.Markdown()
                with gr.Column(scale=5):
                    requirements_table=gr.Dataframe(headers=["ID","Category","Mandatory","Requirement","Evidence needed","Status","Approved"],datatype=["str","str","bool","str","str","str","bool"],interactive=False,label="Parsed atomic requirements")
                    approve_req=gr.Button("✓ Approve Requirements",variant="primary");req_status=gr.Markdown()
        with gr.Tab("2 · Investigator Interview"):
            with gr.Row():
                with gr.Column(scale=3):
                    generate_q=gr.Button("Generate / Recompute Missing Questions",variant="primary");question_card=gr.HTML(render_question(None));answer=gr.Textbox(label="Answer")
                    with gr.Row():confidence=gr.Dropdown(["high","medium","low"],value="high",label="Confidence");classification=gr.Dropdown(["verified_fact","investigator_estimate","assumption","unknown"],value="verified_fact",label="Classification")
                    answer_notes=gr.Textbox(label="Supporting explanation / notes",lines=4);answered_by=gr.Textbox(label="Answered by / role");submit=gr.Button("Save Answer & Continue",variant="primary");interview_status=gr.Markdown()
                with gr.Column(scale=2):interview_table=gr.JSON(label="Interview state")
        with gr.Tab("3 · Research & Evidence"):
            with gr.Row():max_queries=gr.Slider(1,24,value=8,step=1,label="Maximum research queries");results_per=gr.Slider(1,10,value=5,step=1,label="Results per query");research_btn=gr.Button("Run Evidence Research",variant="primary")
            evidence_table=gr.Dataframe(headers=["Evidence ID","Requirement","Source type","Evidence purpose","Status","Confidence","URL"],interactive=False);research_status=gr.Markdown()
            gr.Markdown("### Compiled HPC Knowledge Index")
            with gr.Row():rebuild_idx=gr.Button("Build / Refresh MMAP Index");status_idx=gr.Button("Check Index Status")
            index_message=gr.Markdown();index_json=gr.JSON(label="Index manifest / status")
            with gr.Row():retrieval_query=gr.Textbox(label="Test hybrid retrieval query");retrieval_k=gr.Slider(1,50,value=12,step=1,label="Top K");retrieval_btn=gr.Button("Run Retrieval")
            retrieval_table=gr.Dataframe(headers=["Score","Semantic","BM25","Evidence","Freshness","Graph boost","Kind","Source","Excerpt"],interactive=False)
        with gr.Tab("4 · Clinical Study Design"):
            gr.Markdown("### Authoritative clinical study model\nThese structured values are the source of truth injected into every grant section. Deterministic checks flag feasibility and consistency issues; they never rewrite human-approved prose.")
            with gr.Row():
                load_clinical_btn=gr.Button("Load Saved Study")
                save_clinical_btn=gr.Button("Save & Analyze Clinical Study",variant="primary")
            with gr.Row():
                with gr.Column():
                    clinical_problem=gr.Textbox(label="Clinical problem",lines=3)
                    knowledge_gap=gr.Textbox(label="Knowledge gap",lines=3)
                    central_hypothesis=gr.Textbox(label="Central hypothesis",lines=3)
                with gr.Column():
                    disease=gr.Textbox(label="Disease / clinical population")
                    disease_stage=gr.Textbox(label="Stage / subtype")
                    biomarker=gr.Textbox(label="Biomarker / molecular criteria")
                    inclusion=gr.Textbox(label="Inclusion criteria · one per line",lines=4)
                    exclusion=gr.Textbox(label="Exclusion criteria · one per line",lines=4)
            gr.Markdown("#### Study design")
            with gr.Row():
                design_type=gr.Textbox(label="Design type")
                study_phase=gr.Textbox(label="Phase")
                randomization=gr.Textbox(label="Randomization")
                allocation_ratio=gr.Textbox(label="Allocation ratio")
                blinding=gr.Textbox(label="Blinding")
                follow_up_months=gr.Number(label="Follow-up months",value=None)
                design_sites=gr.Number(label="Sites",value=None,precision=0)
            gr.Markdown("#### Recruitment feasibility")
            with gr.Row():
                available_patients=gr.Number(label="Available patients / site / month",value=None)
                eligibility_pct=gr.Number(label="Eligibility rate %",value=None)
                biomarker_pct=gr.Number(label="Biomarker-positive rate %",value=None)
                consent_pct=gr.Number(label="Consent rate %",value=None)
                target_enrollment=gr.Number(label="Target enrollment",value=None,precision=0)
                accrual_months=gr.Number(label="Planned accrual months",value=None)
                recruitment_sites=gr.Number(label="Recruitment sites",value=None,precision=0)
            with gr.Row():
                scenario_sites=gr.Textbox(label="Scenario site counts · comma-separated",placeholder="e.g., 1,2,3")
                scenario_consent=gr.Textbox(label="Scenario consent rates % · comma-separated",placeholder="e.g., 50,65,80")
                scenario_biomarker=gr.Textbox(label="Scenario biomarker-positive rates % · comma-separated",placeholder="e.g., 25,40,55")
                scenario_btn=gr.Button("Run Recruitment Scenario Sweep")
            scenario_table=gr.Dataframe(headers=["Sites","Consent %","Biomarker %","Expected enrollments/mo","Required/mo","Estimated accrual months","Feasible","Shortfall/mo"],interactive=False)
            scenario_status=gr.Markdown()
            gr.Markdown("#### Deterministic statistics")
            with gr.Row():
                test_type=gr.Dropdown(["two_proportions","one_proportion","two_means","log_rank"],label="Sample-size method",allow_custom_value=False)
                alpha=gr.Number(label="Two-sided alpha",value=None)
                power=gr.Number(label="Power (0–1)",value=None)
                attrition_pct=gr.Number(label="Attrition %",value=None)
            with gr.Row():
                control_rate=gr.Number(label="Control rate (0–1)",value=None)
                treatment_rate=gr.Number(label="Treatment rate (0–1)",value=None)
                null_rate=gr.Number(label="Null rate (0–1)",value=None)
                alternative_rate=gr.Number(label="Alternative rate (0–1)",value=None)
                mean_delta=gr.Number(label="Mean difference",value=None)
                std_dev=gr.Number(label="Standard deviation",value=None)
                hazard_ratio=gr.Number(label="Hazard ratio",value=None)
                event_probability=gr.Number(label="Expected event probability (0–1)",value=None)
            calculate_n_btn=gr.Button("Calculate Sample Size")
            sample_size_json=gr.JSON(label="Sample-size calculation")
            sample_size_status=gr.Markdown()
            gr.Markdown("#### Specific Aims")
            aims_table=gr.Dataframe(headers=AIM_HEADERS,datatype=["str"]*8,row_count=(3,"dynamic"),column_count=(8,"fixed"),interactive=True)
            gr.Markdown("#### Study arms")
            arms_table=gr.Dataframe(headers=ARM_HEADERS,datatype=["str","str","str","bool"],row_count=(3,"dynamic"),column_count=(4,"fixed"),interactive=True)
            gr.Markdown("#### Endpoints")
            gr.Markdown("Analysis family values: binary — `chi_square`, `fisher_exact`, `logistic_regression`, `two_proportions`; continuous — `t_test`, `anova`, `linear_regression`, `mixed_model`; count — `poisson`, `negative_binomial`; time-to-event — `log_rank`, `cox`, `cox_regression`; ordinal — `ordinal_logistic`, `wilcoxon`.")
            endpoints_table=gr.Dataframe(headers=ENDPOINT_HEADERS,datatype=["str","str","str","bool","str"],row_count=(3,"dynamic"),column_count=(5,"fixed"),interactive=True)
            gr.Markdown("#### Timeline")
            timeline_table=gr.Dataframe(headers=TIMELINE_HEADERS,datatype=["str","str","number","number","str"],row_count=(4,"dynamic"),column_count=(5,"fixed"),interactive=True)
            gr.Markdown("#### Resources")
            resources_table=gr.Dataframe(headers=RESOURCE_HEADERS,datatype=["str","str","bool","bool","str"],row_count=(4,"dynamic"),column_count=(5,"fixed"),interactive=True)
            clinical_assessment=gr.JSON(label="Deterministic clinical assessment")
            clinical_status=gr.Markdown()
        with gr.Tab("5 · Competitive Applicant Intelligence"):
            gr.Markdown("### Public competitive applicant intelligence\nThe system builds a capability profile from this grant and clinical design, then identifies organizations whose **public** grants, publications, clinical trials, patents/IP signals, and disclosed technologies overlap with that profile. These are potential capability-matched competitors—not confirmed applicants. The resulting strategy is injected into grant drafting to emphasize defensible superiority and differentiation.")
            with gr.Row():
                generate_comp_profile_btn=gr.Button("Generate Likely Strong-Applicant Profile",variant="secondary")
                load_competitive_btn=gr.Button("Open / Auto-Refresh Competitive Intelligence")
                run_competitive_btn=gr.Button("Refresh Public Competitive Intelligence Now",variant="primary")
            competitive_profile_json=gr.JSON(label="Strong-applicant capability profile")
            competitive_status=gr.Markdown()
            competitor_table=gr.Dataframe(headers=["Rank","Potential competitor","Overall","Prior grants","Publications","Clinical trials","Patent/IP signal","Technology","Breadth","Public assets"],interactive=False,label="Capability-matched organizations · public evidence only")
            provider_status_json=gr.JSON(label="Public provider status")
            asset_table=gr.Dataframe(headers=["Potential competitor","Asset type","Relevance","Public asset","Year","Provider","URL"],interactive=False,label="Top public grants / publications / trials / IP signals / technology evidence")
            competitive_strategy=gr.Markdown("No competitive positioning run yet.")
            competitive_raw=gr.JSON(label="Auditable competitive run",visible=False)
        with gr.Tab("6 · Sponsor Compliance & Submission"):
            gr.Markdown("""### Deterministic Sponsor Compliance
The approved funding opportunity is compiled into atomic submission rules. You can correct the parsed rules before approval. Hard sponsor rules—not AI opinion—control final export readiness.""")
            with gr.Row():
                load_compliance_btn=gr.Button("Open Compliance Profile")
                compile_compliance_btn=gr.Button("Recompile Rules from Current Opportunity")
                approve_compliance_btn=gr.Button("✓ Approve Compliance Profile",variant="primary")
                measure_compliance_btn=gr.Button("Run Rendered Preflight")
            compliance_profile_state=gr.State({})
            opportunity_source_preview=gr.Textbox(label="Authoritative stored funding-opportunity source used for deterministic rule compilation",lines=18,interactive=False)
            with gr.Row():
                compliance_sponsor=gr.Textbox(label="Sponsor")
                compliance_mechanism=gr.Textbox(label="Mechanism")
                submission_system=gr.Textbox(label="Submission system / portal")
                compliance_deadline=gr.Textbox(label="Deadline (YYYY-MM-DD)")
            compliance_rules=gr.Dataframe(headers=COMPLIANCE_HEADERS,row_count=(8,"dynamic"),column_count=(14,"fixed"),interactive=True,label="Rule meaning and source hints · editable before approval")
            save_compliance_btn=gr.Button("Save Rule Corrections")
            compliance_provenance=gr.Dataframe(headers=["Rule ID","Source status","Document ID","Page","Start byte","End byte","Exact source excerpt"],interactive=False,label="Deterministically copied provenance · never authored or edited by an LLM")
            compliance_findings=gr.Dataframe(headers=["Rule ID","Severity","Mandatory","Status","Type","Target","Detail","Source excerpt"],interactive=False,label="Deterministic compliance assessment")
            compliance_json=gr.JSON(label="Compliance assessment")
            gr.Markdown("#### Resolve a rule that requires authoritative human confirmation")
            with gr.Row():
                compliance_rule_id=gr.Textbox(label="Rule ID")
                compliance_resolution=gr.Dropdown(["satisfied","not_applicable","waived","unresolved"],value="satisfied",label="Resolution")
                compliance_resolved_by=gr.Textbox(label="Resolved by / role")
            compliance_resolution_notes=gr.Textbox(label="Resolution rationale / evidence note",lines=3)
            resolve_compliance_btn=gr.Button("Save Manual Resolution")
            gr.Markdown("""#### Submission attachments
Register the actual files that must travel with the proposal. Use a stable slot such as `letters_of_support`, `biosketches`, `data_management_plan`, or the sponsor's attachment name.""")
            with gr.Row():
                artifact_slot=gr.Textbox(label="Submission slot")
                artifact_files=gr.File(label="Attach files",file_count="multiple",type="filepath")
                register_artifact_btn=gr.Button("Register Attachment(s)")
            artifact_table=gr.Dataframe(headers=["Slot","Filename","Extension","SHA-256"],interactive=False,label="Registered submission artifacts")
            compliance_status=gr.Markdown()
        with gr.Tab("7 · Write, Edit & Approve"):
            with gr.Row():section=gr.Dropdown(DEFAULT_SECTIONS,value=(DEFAULT_SECTIONS[0] if DEFAULT_SECTIONS else None),label="Section",allow_custom_value=True);high=gr.Checkbox(label="Escalate this draft to Claude (sends this section’s compiled context to the configured cloud API)",value=False)
            additional=gr.Textbox(lines=4,label="Optional additional human context");gen=gr.Button("Compile Context & Draft Section",variant="primary")
            section_update_banner=gr.Markdown()
            preview_box=gr.HTML('<div class="page-frame"><div style="background:white;padding:32px">Open a project and select a section.</div></div>')
            with gr.Row():edit=gr.Button("✎ Edit");approve_btn=gr.Button("✓ Approve Section",variant="primary")
            editor=gr.Textbox(lines=20,label="Section text",visible=False)
            with gr.Row():save_btn=gr.Button("Save Edit",visible=False);cancel=gr.Button("Cancel Edit",visible=False)
            write_status=gr.Markdown("No section loaded.")
        with gr.Tab("8 · Final Export"):
            gr.Markdown("### Human-Approved Grant Assembly\nOnly exact section versions approved by the human are included below or in final exports. AI drafts and unapproved edits are excluded.")
            preview_approved_btn=gr.Button("Refresh Approved Grant Preview",variant="secondary")
            approved_sections_table=gr.Dataframe(headers=["Order","Section","Status","Approved version"],datatype=["number","str","str","number"],interactive=False,label="Approved section assembly")
            approved_grant_preview=gr.HTML('<div class="page-frame"><div style="background:white;padding:32px">Approve sections, then refresh this preview to see the grant assembled in final document order.</div></div>')
            approved_preview_status=gr.Markdown()
            check_ready=gr.Button("Check Submission Readiness");readiness_json=gr.JSON(label="Backend readiness gates");readiness_status=gr.Markdown()
            fmt=gr.Radio(["DOCX","PDF","BOTH"],value="DOCX",label="Would you like me to produce a professionally formatted DOCX, PDF, or both?",visible=False);export_btn=gr.Button("Compile Approved Grant",variant="primary",visible=False);export_file=gr.File(label="Generated file(s)",file_count="multiple");export_status=gr.Markdown()
        with gr.Tab("9 · System & Diagnostics"):
            gr.Markdown("""### Production runtime diagnostics
This view exposes non-secret runtime/build information and a local HPC benchmark. It does not display API keys or uploaded grant content.""")
            with gr.Row():
                system_info_btn=gr.Button("Refresh Runtime Information")
                hpc_bench_btn=gr.Button("Run HPC Benchmark",variant="secondary")
            diagnostics_status=gr.Markdown()
            with gr.Row():
                system_info_json=gr.JSON(label="Runtime / build information")
                hpc_benchmark_json=gr.JSON(label="MMAP / OpenMP / BLAS benchmark")

    create.click(create_project,[project_title,sponsor,mechanism,source,source_url,source_text,supporting,brand],[project_id,project_status,agentic_global_notice,requirements_table,section])
    approve_req.click(approve_requirements,[project_id],[req_status])
    generate_q.click(generate_interview,[project_id],[interview_questions,current_question,question_card,answer,interview_status,interview_table])
    submit.click(submit_answer,[project_id,interview_questions,current_question,answer,confidence,classification,answer_notes,answered_by],[interview_questions,current_question,question_card,answer,interview_status])
    research_btn.click(run_research,[project_id,max_queries,results_per],[evidence_table,research_status]);rebuild_idx.click(rebuild_index,[project_id],[index_message,index_json]);status_idx.click(index_status,[project_id],[index_message,index_json]);retrieval_btn.click(test_retrieval,[project_id,retrieval_query,retrieval_k],[retrieval_table])
    load_clinical_btn.click(load_clinical_study,[project_id],[clinical_problem,knowledge_gap,central_hypothesis,disease,disease_stage,biomarker,inclusion,exclusion,design_type,study_phase,randomization,allocation_ratio,blinding,follow_up_months,design_sites,available_patients,eligibility_pct,biomarker_pct,consent_pct,target_enrollment,accrual_months,recruitment_sites,test_type,alpha,power,attrition_pct,control_rate,treatment_rate,null_rate,alternative_rate,mean_delta,std_dev,hazard_ratio,event_probability,aims_table,arms_table,endpoints_table,timeline_table,resources_table,clinical_assessment,clinical_status])
    save_clinical_btn.click(save_clinical_study,[project_id]+[clinical_problem,knowledge_gap,central_hypothesis,disease,disease_stage,biomarker,inclusion,exclusion,design_type,study_phase,randomization,allocation_ratio,blinding,follow_up_months,design_sites,available_patients,eligibility_pct,biomarker_pct,consent_pct,target_enrollment,accrual_months,recruitment_sites,test_type,alpha,power,attrition_pct,control_rate,treatment_rate,null_rate,alternative_rate,mean_delta,std_dev,hazard_ratio,event_probability,aims_table,arms_table,endpoints_table,timeline_table,resources_table],[clinical_assessment,clinical_status])
    calculate_n_btn.click(calculate_sample_size,[project_id,test_type,alpha,power,attrition_pct,control_rate,treatment_rate,null_rate,alternative_rate,mean_delta,std_dev,hazard_ratio,event_probability],[sample_size_json,sample_size_status])
    scenario_btn.click(run_feasibility_scenarios,[project_id,scenario_sites,scenario_consent,scenario_biomarker],[scenario_table,scenario_status])
    generate_comp_profile_btn.click(generate_competitive_profile,[project_id],[competitive_profile_json,competitive_status])
    load_competitive_btn.click(load_competitive,[project_id],[competitive_profile_json,competitor_table,asset_table,provider_status_json,competitive_strategy,competitive_raw,competitive_status,agentic_global_notice])
    run_competitive_btn.click(run_competitive_intelligence,[project_id],[competitor_table,asset_table,provider_status_json,competitive_strategy,competitive_raw,competitive_status,agentic_global_notice])
    load_compliance_btn.click(load_compliance,[project_id],[compliance_profile_state,opportunity_source_preview,compliance_sponsor,compliance_mechanism,submission_system,compliance_deadline,compliance_rules,compliance_provenance,compliance_findings,compliance_json,artifact_table,compliance_status])
    compile_compliance_btn.click(compile_compliance,[project_id],[compliance_profile_state,opportunity_source_preview,compliance_sponsor,compliance_mechanism,submission_system,compliance_deadline,compliance_rules,compliance_provenance,compliance_findings,compliance_json,compliance_status,section])
    save_compliance_btn.click(save_compliance,[project_id,compliance_sponsor,compliance_mechanism,submission_system,compliance_deadline,compliance_rules],[compliance_profile_state,compliance_rules,compliance_provenance,compliance_findings,compliance_json,compliance_status,section])
    approve_compliance_btn.click(approve_compliance,[project_id],[compliance_provenance,compliance_findings,compliance_json,compliance_status])
    resolve_compliance_btn.click(resolve_compliance,[project_id,compliance_rule_id,compliance_resolution,compliance_resolution_notes,compliance_resolved_by],[compliance_findings,compliance_json,compliance_status])
    register_artifact_btn.click(register_submission_artifacts,[project_id,artifact_slot,artifact_files],[artifact_table,compliance_findings,compliance_json,compliance_status])
    measure_compliance_btn.click(measure_compliance,[project_id],[compliance_findings,compliance_json,compliance_status])
    section.change(load_section,[project_id,project_title,section],[current_version,baseline_body,preview_box,write_status,editor,current_section_key,current_competitive_update_event,section_update_banner])
    gen.click(draft_section,[project_id,project_title,section,additional,high],[current_version,baseline_body,current_section_key,preview_box,write_status,editor,save_btn,cancel,current_competitive_update_event,section_update_banner])
    edit.click(show_editor,[baseline_body],[editor,save_btn,cancel])
    save_btn.click(save_edit,[project_id,project_title,section,current_section_key,current_version,baseline_body,editor],[current_version,baseline_body,preview_box,write_status,editor,save_btn,cancel,current_competitive_update_event,section_update_banner])
    cancel.click(cancel_edit,[project_id,project_title,section,current_section_key,current_version,baseline_body],[preview_box,editor,save_btn,cancel,write_status])
    approve_btn.click(approve_section,[project_id,project_title,section,current_section_key,current_version,baseline_body,editor,current_competitive_update_event],[current_version,baseline_body,preview_box,write_status,editor,save_btn,cancel,current_competitive_update_event,section_update_banner])
    preview_approved_btn.click(preview_approved_grant,[project_id],[approved_sections_table,approved_grant_preview,approved_preview_status])
    check_ready.click(readiness,[project_id],[readiness_json,readiness_status,fmt,export_btn]);export_btn.click(export,[project_id,fmt,project_title],[export_file,export_status])
    refresh_projects_btn.click(refresh_projects,outputs=[recent])
    open_project_btn.click(load_project,[recent],[project_id,project_title,sponsor,mechanism,project_status,agentic_global_notice,requirements_table,section,current_version,baseline_body,preview_box,write_status,editor,current_section_key,current_competitive_update_event,section_update_banner])
    system_info_btn.click(system_diagnostics,outputs=[system_info_json,diagnostics_status])
    hpc_bench_btn.click(run_hpc_diagnostics,outputs=[hpc_benchmark_json,diagnostics_status])
    competitive_update_timer.tick(poll_competitive_updates,[project_id],[agentic_global_notice],show_progress="hidden")

if __name__=="__main__":demo.launch(server_name="0.0.0.0",server_port=7860,css=CSS)

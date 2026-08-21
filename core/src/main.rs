use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::{BTreeSet, HashMap}, path::PathBuf, sync::Arc, time::Instant};
use parking_lot::Mutex as ParkingMutex;
use tracing::{info, warn};
use uuid::Uuid;

mod storage;
mod hpc;
mod models;
mod workflow;
mod parquet_store;
mod vector_store;
mod domain;
mod json_extract;
mod research;
mod embedding;
mod chunker;
mod lexical;
mod csr;
mod record_store;
mod retrieval;
mod context_compiler;
mod clinical;
mod competitive;
mod competitive_updates;
mod compliance;

use domain::{EvidenceValidationEnvelope, InterviewEnvelope, ResearchPlanEnvelope, RequirementsEnvelope};
use clinical::{ClinicalStudy, ScenarioSweepInput, StatisticsPlan};
use competitive::CompetitiveEngine;
use compliance::{ComplianceEnvelope, ComplianceProfile};
use embedding::EmbeddingClient;
use json_extract::parse_json_from_model;
use models::{ModelRouter, ModelTask};
use research::ResearchClient;
use retrieval::RetrievalService;
use storage::Store;
use workflow::{require_at_least, Stage};

#[derive(Clone)]
struct AppState {
    store: Arc<Store>,
    router: Arc<ModelRouter>,
    research: Arc<ResearchClient>,
    embedding: Arc<EmbeddingClient>,
    retrieval: Arc<RetrievalService>,
    competitive_locks: Arc<ParkingMutex<HashMap<String,Arc<tokio::sync::Mutex<()>>>>>,
    workspace: PathBuf,
}

#[derive(Serialize)] struct Health { status: &'static str, version: &'static str, hpc_threads: i32 }
#[derive(Deserialize)] struct CreateProject { title:String, sponsor:Option<String>, mechanism:Option<String>, #[serde(default)] sections:Vec<String> }
#[derive(Serialize)] struct ProjectCreated { id:String, title:String }
#[derive(Deserialize)] struct SectionInput { title:String, body:String, html:Option<String>, base_version_id:Option<i64> }
#[derive(Deserialize)] struct ApproveSectionInput { version_id:i64, competitive_update_event_id:Option<i64> }
#[derive(Deserialize)] struct GenerateRequest { task:String, prompt:String, high_value:Option<bool> }
#[derive(Deserialize)] struct DocumentInput { name:String, kind:String, text:String }
#[derive(Deserialize)] struct FetchUrlInput { url:String, name:Option<String>, kind:Option<String> }
#[derive(Deserialize)] struct AnswerInput { question_id:i64, value:serde_json::Value, confidence:String, classification:String, notes:Option<String>, answered_by:Option<String> }
#[derive(Deserialize)] struct DraftSectionInput { section_key:String, title:String, additional_context:Option<String>, high_value:Option<bool> }
#[derive(Deserialize)] struct ResearchInput { max_queries:Option<usize>, results_per_query:Option<usize> }
#[derive(Deserialize)] struct RetrieveInput { query:String, k:Option<usize> }
#[derive(Deserialize)] struct DesignProfileInput { profile:serde_json::Value }
#[derive(Deserialize)] struct ComplianceProfileInput { profile:ComplianceProfile }
#[derive(Deserialize)] struct ComplianceResolutionInput { rule_id:String, status:String, notes:Option<String>, resolved_by:Option<String> }
#[derive(Deserialize)] struct ComplianceMeasurementsInput { measurements:serde_json::Value }
#[derive(Deserialize)] struct SubmissionArtifactInput { slot:String, filename:String, path:String, sha256:String, extension:String }

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("grant_core=info,info").json().init();
    let workspace=PathBuf::from(std::env::var("GRANT_WORKSPACE").unwrap_or_else(|_|"/workspace".into()));
    std::fs::create_dir_all(&workspace)?;
    let store=Arc::new(Store::open(workspace.join("grant.db"))?);
    let router=Arc::new(ModelRouter::from_env());
    let research=Arc::new(ResearchClient::from_env()?);
    let embedding=Arc::new(EmbeddingClient::from_env()?);
    let retrieval=Arc::new(RetrievalService::new(store.clone(),embedding.clone(),workspace.clone()));
    // Validate competitive-intelligence configuration during startup, but reload it for
    // every refresh so enterprise config changes take effect without restarting Docker.
    let _competitive_config_check=CompetitiveEngine::from_env(research.clone(),embedding.clone(),router.clone())?;
    let competitive_locks=Arc::new(ParkingMutex::new(HashMap::new()));
    let state=AppState{store,router,research,embedding,retrieval,competitive_locks,workspace};
    start_competitive_background_refresh(state.clone());

    let app=Router::new()
        .route("/health",get(health))
        .route("/health/ready",get(ready))
        .route("/api/projects",get(list_projects).post(create_project))
        .route("/api/projects/{id}",get(get_project))
        .route("/api/projects/{id}/readiness",get(get_readiness))
        .route("/api/projects/{id}/design-profile",get(get_design_profile).post(save_design_profile))
        .route("/api/projects/{id}/clinical-study",get(get_clinical_study).post(save_clinical_study))
        .route("/api/projects/{id}/clinical-assessment",get(get_clinical_assessment))
        .route("/api/projects/{id}/clinical/sample-size",post(calculate_sample_size))
        .route("/api/projects/{id}/clinical/scenarios",post(run_clinical_scenarios))
        .route("/api/projects/{id}/competitive/profile",get(get_competitive_profile))
        .route("/api/projects/{id}/competitive/profile/generate",post(generate_competitive_profile))
        .route("/api/projects/{id}/competitive",get(get_competitive_intelligence))
        .route("/api/projects/{id}/competitive/run",post(run_competitive_intelligence))
        .route("/api/projects/{id}/competitive/refresh",post(refresh_competitive_intelligence))
        .route("/api/projects/{id}/competitive/updates",get(get_competitive_updates))
        .route("/api/projects/{id}/compliance",get(get_compliance_profile).post(save_compliance_profile))
        .route("/api/projects/{id}/compliance/compile",post(compile_compliance_profile))
        .route("/api/projects/{id}/compliance/approve",post(approve_compliance_profile))
        .route("/api/projects/{id}/compliance/resolve",post(resolve_compliance_rule))
        .route("/api/projects/{id}/compliance/measurements",post(save_compliance_measurements))
        .route("/api/projects/{id}/compliance/assessment",get(get_compliance_assessment))
        .route("/api/projects/{id}/submission-artifacts",get(get_submission_artifacts).post(register_submission_artifact))
        .route("/api/projects/{id}/documents",post(add_document))
        .route("/api/projects/{id}/documents/fetch-url",post(fetch_url_document))
        .route("/api/projects/{id}/opportunity-source",get(get_opportunity_source))
        .route("/api/projects/{id}/analyze-requirements",post(analyze_requirements))
        .route("/api/projects/{id}/requirements",get(get_requirements))
        .route("/api/projects/{id}/requirements/approve",post(approve_requirements))
        .route("/api/projects/{id}/interview/generate",post(generate_interview))
        .route("/api/projects/{id}/interview",get(get_interview))
        .route("/api/projects/{id}/interview/answer",post(save_answer))
        .route("/api/projects/{id}/research/run",post(run_research))
        .route("/api/projects/{id}/evidence",get(get_evidence))
        .route("/api/projects/{id}/index/rebuild",post(rebuild_index))
        .route("/api/projects/{id}/index/status",get(index_status))
        .route("/api/projects/{id}/retrieve",post(retrieve_context))
        .route("/api/projects/{id}/draft-section",post(draft_section))
        .route("/api/projects/{id}/sections",get(project_sections))
        .route("/api/projects/{id}/sections/{section}",get(get_section).post(save_section))
        .route("/api/projects/{id}/sections/{section}/approve",post(approve_section))
        .route("/api/projects/{id}/approved-sections",get(approved_sections))
        .route("/api/projects/{id}/approved-document",get(approved_document))
        .route("/api/projects/{id}/export-snapshot",post(export_snapshot))
        .route("/api/generate",post(generate))
        .route("/api/hpc/benchmark",post(hpc_benchmark))
        .route("/api/system/info",get(system_info))
        .with_state(state);

    let listener=tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    info!("grant-core listening on 0.0.0.0:8080");
    axum::serve(listener,app).await?;
    Ok(())
}

async fn health()->Json<Health>{Json(Health{status:"ok",version:env!("CARGO_PKG_VERSION"),hpc_threads:hpc::max_threads()})}
async fn ready(State(s):State<AppState>)->Result<Json<serde_json::Value>,ApiError>{
    let model=s.router.health().await.map_err(|e|ApiError::unavailable(format!("model backend not ready: {e}")))?;
    let embedding=s.embedding.health().await.map_err(|e|ApiError::unavailable(format!("embedding model not ready: {e}")))?;
    Ok(Json(serde_json::json!({"status":"ready","version":env!("CARGO_PKG_VERSION"),"model":model,"embedding":embedding,"hpc_threads":hpc::max_threads()})))
}

async fn list_projects(State(s):State<AppState>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.list_projects_json()?))}
async fn create_project(State(s):State<AppState>,Json(req):Json<CreateProject>)->Result<Json<ProjectCreated>,ApiError>{
    if req.title.trim().is_empty(){return Err(ApiError::bad_request("working title is required"));}
    let id=Uuid::new_v4().to_string();
    s.store.create_project(&id,req.title.trim(),req.sponsor.as_deref(),req.mechanism.as_deref(),&req.sections)?;
    std::fs::create_dir_all(s.workspace.join("projects").join(&id)).map_err(anyhow::Error::from)?;
    Ok(Json(ProjectCreated{id,title:req.title}))
}
async fn get_project(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    maybe_auto_refresh_competitive(&s,&id).await?;
    let mut project=s.store.project_json(&id)?;
    if let Some(o)=project.as_object_mut(){o.insert("competitive_updates".into(),s.store.competitive_updates_json(&id,10)?);}
    Ok(Json(project))
}
async fn get_readiness(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    maybe_auto_refresh_competitive(&s,&id).await?;
    Ok(Json(s.store.readiness_json(&id)?))
}
async fn get_design_profile(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.design_profile_json(&id)?))}
async fn save_design_profile(State(s):State<AppState>,Path(id):Path<String>,Json(req):Json<DesignProfileInput>)->Result<Json<serde_json::Value>,ApiError>{
    if !req.profile.is_object(){return Err(ApiError::bad_request("design profile must be a JSON object"));}
    Ok(Json(s.store.save_design_profile(&id,&req.profile)?))
}
async fn project_sections(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.project_sections_json(&id)?))}

async fn get_clinical_study(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.clinical_study_json(&id)?))}
async fn save_clinical_study(State(s):State<AppState>,Path(id):Path<String>,Json(study):Json<ClinicalStudy>)->Result<Json<serde_json::Value>,ApiError>{
    let stage=s.store.project_stage(&id)?;
    require_at_least(stage,Stage::Research,"saving the clinical study model").map_err(ApiError::conflict_err)?;
    let saved=s.store.save_clinical_study(&id,&study).map_err(|e|ApiError::bad_request(e.to_string()))?;
    let assessment=s.store.clinical_assessment_json(&id)?;
    Ok(Json(serde_json::json!({"saved":saved,"assessment":assessment,"stage":s.store.project_stage(&id)?.as_str()})))
}
async fn get_clinical_assessment(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.clinical_assessment_json(&id)?))}
async fn calculate_sample_size(State(s):State<AppState>,Path(id):Path<String>,Json(plan):Json<StatisticsPlan>)->Result<Json<serde_json::Value>,ApiError>{
    // Project lookup makes this endpoint project-scoped and prevents detached calculations from being mistaken for grant state.
    let _=s.store.project_json(&id)?;
    Ok(Json(clinical::sample_size(&plan).map_err(|e|ApiError::bad_request(e.to_string()))?))
}
async fn run_clinical_scenarios(State(s):State<AppState>,Path(id):Path<String>,Json(input):Json<ScenarioSweepInput>)->Result<Json<serde_json::Value>,ApiError>{
    let study=s.store.clinical_study_typed(&id)?.ok_or_else(||ApiError::conflict("save the clinical study before running feasibility scenarios"))?;
    let max=std::env::var("CLINICAL_SCENARIO_MAX_COMBINATIONS").ok().and_then(|v|v.parse().ok()).unwrap_or(10_000usize).clamp(1,1_000_000);
    Ok(Json(clinical::scenario_sweep(&study,&input,max).map_err(|e|ApiError::bad_request(e.to_string()))?))
}


fn competitive_lock(s:&AppState,project:&str)->Arc<tokio::sync::Mutex<()>>{
    let mut locks=s.competitive_locks.lock();
    locks.entry(project.to_string()).or_insert_with(||Arc::new(tokio::sync::Mutex::new(()))).clone()
}

fn competitive_profile_context(store:&Store,project:&str)->Result<String,ApiError>{
    let project_meta=serde_json::to_string_pretty(&store.project_json(project)?)?;
    let requirements=store.requirements_context(project)?;
    let interview=store.interview_context(project)?;
    let evidence=store.evidence_context(project,32_000)?;
    let clinical=store.clinical_context(project)?;
    let documents=store.document_context(project,48_000)?;
    Ok(format!("PROJECT METADATA:\n{project_meta}\n\nAPPROVED REQUIREMENTS:\n{requirements}\n\nINVESTIGATOR INTERVIEW:\n{interview}\n\nAUTHORITATIVE CLINICAL DESIGN:\n{clinical}\n\nCURRENT EVIDENCE:\n{evidence}\n\nSOURCE MATERIAL EXCERPTS:\n{documents}"))
}

async fn get_competitive_profile(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    Ok(Json(s.store.competitive_profile_json(&id)?))
}

async fn generate_competitive_profile(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    require_interview_complete(&s.store,&id)?;
    let stage=s.store.project_stage(&id)?;
    require_at_least(stage,Stage::Science,"competitive applicant profiling").map_err(ApiError::conflict_err)?;
    if s.store.clinical_study_typed(&id)?.is_none(){return Err(ApiError::conflict("save the structured clinical study model before generating a competitive applicant profile"));}
    let lock=competitive_lock(&s,&id); let _guard=lock.lock().await;
    let engine=CompetitiveEngine::from_env(s.research.clone(),s.embedding.clone(),s.router.clone()).map_err(|e|ApiError::bad_gateway(format!("competitive engine reload failed: {e}")))?;
    let input_fingerprint=s.store.competitive_input_fingerprint(&id)?;
    let context=competitive_profile_context(&s.store,&id)?;
    let (profile,model)=engine.generate_profile(&context).await.map_err(|e|ApiError::bad_gateway(format!("competitive profile generation failed: {e}")))?;
    // Refuse to save a profile against an input state that changed during model generation.
    if s.store.competitive_input_fingerprint(&id)?!=input_fingerprint{return Err(ApiError::conflict("project knowledge changed while the competitive profile was being generated; retry against the current clinical/grant state"));}
    Ok(Json(s.store.save_competitive_profile(&id,&profile,&input_fingerprint,&model)?))
}

async fn get_competitive_intelligence(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    let data=ensure_competitive_fresh(&s,&id,false).await?;
    Ok(Json(data))
}

async fn refresh_competitive_intelligence(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    let data=ensure_competitive_fresh(&s,&id,true).await?;
    Ok(Json(data))
}

async fn run_competitive_intelligence(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    let data=ensure_competitive_fresh(&s,&id,true).await?;
    Ok(Json(data))
}

async fn get_competitive_updates(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    maybe_auto_refresh_competitive(&s,&id).await?;
    Ok(Json(s.store.competitive_updates_json(&id,25)?))
}

async fn maybe_auto_refresh_competitive(s:&AppState,id:&str)->Result<(),ApiError>{
    let stage=s.store.project_stage(id)?;
    if stage < Stage::Science { return Ok(()); }
    if s.store.clinical_study_typed(id)?.is_none() { return Ok(()); }
    if !s.store.interview_generated(id)? || s.store.interview_open_count(id)? > 0 { return Ok(()); }
    if let Err(e)=ensure_competitive_fresh(s,id,false).await {
        warn!(project_id=%id,error=%e.message,"competitive auto-refresh failed; continuing with stale state and keeping export fail-closed");
    }
    Ok(())
}

fn start_competitive_background_refresh(state:AppState){
    let enabled=std::env::var("COMPETITIVE_BACKGROUND_REFRESH_ENABLED").ok()
        .map(|v|matches!(v.trim().to_ascii_lowercase().as_str(),"1"|"true"|"yes"|"on"))
        .unwrap_or(true);
    if !enabled {
        info!("competitive background refresh disabled");
        return;
    }
    let interval_seconds=std::env::var("COMPETITIVE_BACKGROUND_REFRESH_SECONDS").ok()
        .and_then(|v|v.parse::<u64>().ok()).unwrap_or(14_400).clamp(300,86_400);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(interval_seconds)).await;
            let projects=match state.store.list_projects_json(){
                Ok(v)=>v,
                Err(e)=>{warn!(error=%e,"competitive background refresh could not list projects");continue;}
            };
            let Some(rows)=projects.as_array() else {continue;};
            // Run sequentially by default. Provider-specific rate limits remain authoritative,
            // and a single weak Mac should not fan out multiple long public-intelligence runs.
            for row in rows {
                let Some(id)=row.get("id").and_then(serde_json::Value::as_str) else {continue;};
                let stage=row.get("stage").and_then(serde_json::Value::as_str).unwrap_or("intake");
                if !matches!(stage,"science"|"strategy"|"writing"|"review"|"export"){continue;}
                if let Err(e)=maybe_auto_refresh_competitive(&state,id).await {
                    warn!(project_id=%id,error=%e.message,"competitive background refresh will retry later");
                }
            }
        }
    });
}

async fn process_competitive_text_update(s:&AppState,id:&str,engine:&CompetitiveEngine)->Result<serde_json::Value,ApiError>{
    let event=s.store.latest_unprocessed_competitive_update_json(id)?;
    if event.as_object().map(|o|o.is_empty()).unwrap_or(true){return Ok(serde_json::json!({"processed":true,"section_updates":[]}));}
    let event_id=event.get("event_id").and_then(serde_json::Value::as_i64).ok_or_else(||ApiError::bad_gateway("competitive update event is missing event_id"))?;
    let delta:competitive_updates::CompetitiveDelta=serde_json::from_value(event.get("delta").cloned().unwrap_or_default()).map_err(|e|ApiError::bad_gateway(format!("stored competitive update delta is invalid: {e}")))?;
    let cfg=&engine.config().updates;
    if !cfg.auto_revise_sections {
        s.store.set_competitive_update_processing(id,event_id,"complete",&serde_json::json!([]))?;
        return Ok(serde_json::json!({"processed":true,"event_id":event_id,"section_updates":[],"auto_revision_disabled":true}));
    }
    let latest=s.store.latest_sections_json(id)?;
    let sections=latest.as_array().cloned().unwrap_or_default();
    let changed=delta.changed_section_keys.iter().cloned().collect::<BTreeSet<_>>();
    let revise_all=(cfg.update_all_sections_on_material_change && delta.material)
        || (cfg.update_all_sections_on_strategy_change && delta.broad_strategy_change);
    let mut candidates=sections.into_iter().filter(|x|{
        let key=x.get("section_key").and_then(serde_json::Value::as_str).unwrap_or("");
        revise_all || changed.contains(key)
    }).collect::<Vec<_>>();
    candidates.truncate(cfg.max_sections_per_refresh.max(1));
    if candidates.is_empty(){
        s.store.set_competitive_update_processing(id,event_id,"complete",&serde_json::json!([]))?;
        return Ok(serde_json::json!({"processed":true,"event_id":event_id,"section_updates":[]}));
    }
    let current=s.store.competitive_latest_json(id)?;
    let strategy=current.get("strategy").cloned().unwrap_or(serde_json::Value::Null);
    let mut updated=Vec::<serde_json::Value>::new(); let mut errors=Vec::<serde_json::Value>::new();
    for sec in candidates {
        let key=sec.get("section_key").and_then(serde_json::Value::as_str).unwrap_or("");
        let title=sec.get("title").and_then(serde_json::Value::as_str).unwrap_or("Section");
        let base_version=sec.get("version").and_then(serde_json::Value::as_i64).unwrap_or(0);
        let body=sec.get("body").and_then(serde_json::Value::as_str).unwrap_or("");
        if key.is_empty() || base_version<=0 || body.trim().is_empty() || s.store.competitive_section_update_exists(event_id,id,key)? {continue;}
        let query=format!("Refresh grant section {title} ({key}) using newly changed public competitive applicant intelligence while preserving authoritative clinical facts and human language.");
        let budget=std::env::var("CONTEXT_MAX_CHARS").ok().and_then(|v|v.parse().ok()).unwrap_or(48_000usize).clamp(8_000,180_000);
        let compiled=match context_compiler::compile(&s.store,&s.retrieval,id,&query,budget).await{Ok(x)=>x,Err(e)=>{errors.push(serde_json::json!({"section_key":key,"error":e.to_string()}));continue;}};
        let prompt=format!(r#"New public competitive-applicant intelligence has changed since this grant section was last written. Update the EXISTING section only where the new public evidence or positioning strategy materially improves competitive differentiation. Preserve the author's scientific meaning, clinical design, enrollment/statistical values, commitments, citations, and wording wherever no change is needed. Never invent competitor intent or confidential information and never imply a potential competitor is a confirmed applicant. Normally do not name competitors in proposal prose. Return the COMPLETE revised section prose only. If no prose change is justified, return the existing section EXACTLY.

SECTION: {title}

EXISTING TEXT:
{body}

COMPETITIVE CHANGE SUMMARY:
{}

CURRENT COMPETITIVE STRATEGY:
{}

CURRENT AUTHORITATIVE CONTEXT:
{}"#,serde_json::to_string_pretty(&delta).unwrap_or_default(),serde_json::to_string_pretty(&strategy).unwrap_or_default(),compiled.text);
        let generated=match s.router.generate(ModelTask{kind:"competitive_section_refresh".into(),prompt,high_value:cfg.section_refresh_high_value}).await{Ok(x)=>x,Err(e)=>{errors.push(serde_json::json!({"section_key":key,"error":e.to_string()}));continue;}};
        let revised=generated.text.trim();
        // The investigator may edit while the competitive refresh model is running.
        // Never publish a proposal against a superseded base version; leave the event
        // retryable so the next access self-heals against the newest human/model text.
        let current_state=match s.store.section_state_json(id,key){Ok(x)=>x,Err(e)=>{errors.push(serde_json::json!({"section_key":key,"error":e.to_string()}));continue;}};
        let current_version=current_state.get("latest").and_then(|x|x.get("version")).and_then(serde_json::Value::as_i64);
        if current_version!=Some(base_version){
            errors.push(serde_json::json!({"section_key":key,"error":"section changed while competitive auto-update was being generated; retry will use the newest version","expected_base_version":base_version,"current_version":current_version}));
            continue;
        }
        if revised==body.trim(){
            if let Err(e)=s.store.record_competitive_section_no_change(event_id,id,key,base_version){errors.push(serde_json::json!({"section_key":key,"error":e.to_string()}));}
            continue;
        }
        let version=match s.store.save_section(id,key,title,revised,None,&format!("agentic_competitive_update:run:{}:{}",delta.to_run_id,generated.model)){Ok(v)=>v,Err(e)=>{errors.push(serde_json::json!({"section_key":key,"error":e.to_string()}));continue;}};
        if let Err(e)=s.store.record_competitive_section_update(event_id,id,key,base_version,version){errors.push(serde_json::json!({"section_key":key,"error":e.to_string()}));continue;}
        updated.push(serde_json::json!({"section_key":key,"title":title,"base_version":base_version,"proposed_version":version,"model":generated.model}));
    }
    if errors.is_empty(){
        s.store.set_competitive_update_processing(id,event_id,"complete",&serde_json::json!([]))?;
    }else{
        s.store.set_competitive_update_processing(id,event_id,"partial",&serde_json::Value::Array(errors.clone()))?;
    }
    if !updated.is_empty(){s.store.set_stage(id,Stage::Writing)?;}
    // Partial update failures do not strand the investigator or make basic project
    // reads fail. The event remains retryable; readiness/export stay fail-closed until
    // all update work is complete, while the UI can explain exactly what remains.
    Ok(serde_json::json!({
        "processed":errors.is_empty(),
        "partial":!errors.is_empty(),
        "event_id":event_id,
        "section_updates":updated,
        "errors":errors,
        "summary":delta.summary
    }))
}

async fn ensure_competitive_fresh(s:&AppState,id:&str,force:bool)->Result<serde_json::Value,ApiError>{
    require_interview_complete(&s.store,id)?;
    let resume_stage=s.store.project_stage(id)?;
    if resume_stage < Stage::Science { return Err(ApiError::conflict(format!("workflow gate: competitive intelligence requires stage 'science' or later; current stage is {resume_stage}"))); }
    if s.store.clinical_study_typed(id)?.is_none(){return Err(ApiError::conflict("save the structured clinical study model before running competitive intelligence"));}

    let lock=competitive_lock(s,id);
    let _guard=lock.lock().await;
    let initial=s.store.competitive_latest_json(id)?;
    let initial_fresh=initial.get("fresh").and_then(serde_json::Value::as_bool).unwrap_or(false)
        && initial.get("status").and_then(serde_json::Value::as_str)==Some("complete");
    if !force && initial_fresh {
        let engine=CompetitiveEngine::from_env(s.research.clone(),s.embedding.clone(),s.router.clone())
            .map_err(|e|ApiError::bad_gateway(format!("competitive engine reload failed: {e}")))?;
        let update=process_competitive_text_update(s,id,&engine).await?;
        let mut out=initial;
        if let Some(o)=out.as_object_mut(){
            o.insert("auto_refreshed".into(),serde_json::Value::Bool(false));
            o.insert("agentic_update".into(),update);
            o.insert("competitive_updates".into(),s.store.competitive_updates_json(id,10)?);
        }
        return Ok(out);
    }

    // Knowledge or enterprise configuration can legitimately change while public APIs
    // are being queried. Retry against the newest state instead of surfacing a stale-
    // intelligence dead end to the user.
    for attempt in 0..3usize {
        let engine=CompetitiveEngine::from_env(s.research.clone(),s.embedding.clone(),s.router.clone())
            .map_err(|e|ApiError::bad_gateway(format!("competitive engine reload failed: {e}")))?;
        let input_fingerprint=s.store.competitive_input_fingerprint(id)?;
        let profile_meta=s.store.competitive_profile_json(id)?;
        let profile_fresh=profile_meta.get("exists").and_then(serde_json::Value::as_bool).unwrap_or(false)
            && profile_meta.get("fresh").and_then(serde_json::Value::as_bool).unwrap_or(false);
        if !profile_fresh {
            let context=competitive_profile_context(&s.store,id)?;
            let (profile,model)=engine.generate_profile(&context).await
                .map_err(|e|ApiError::bad_gateway(format!("competitive profile refresh failed: {e}")))?;
            if s.store.competitive_input_fingerprint(id)?!=input_fingerprint { continue; }
            s.store.save_competitive_profile(id,&profile,&input_fingerprint,&model)?;
        }

        let profile_meta=s.store.competitive_profile_json(id)?;
        let profile_version=profile_meta.get("version").and_then(serde_json::Value::as_i64)
            .ok_or_else(||ApiError::bad_gateway("stored competitive profile is missing its version"))?;
        let profile=s.store.competitive_profile_typed(id)?.ok_or_else(||ApiError::conflict("competitive applicant profile is missing"))?;
        let current_input=s.store.competitive_input_fingerprint(id)?;
        let config_sha=engine.config_sha256()?;
        let run_id=s.store.begin_competitive_run(id,profile_version,&current_input,&config_sha)?;
        let own_context=competitive_profile_context(&s.store,id)?;
        let output=match engine.run(&profile,&own_context).await{
            Ok(x)=>x,
            Err(e)=>{let _=s.store.fail_competitive_run(run_id,&e.to_string());return Err(ApiError::bad_gateway(format!("competitive intelligence refresh failed: {e}")));}
        };
        if s.store.competitive_input_fingerprint(id)?!=current_input{
            let _=s.store.fail_competitive_run(run_id,"project knowledge changed during competitive intelligence refresh; retrying automatically");
            continue;
        }
        let mut out=s.store.finish_competitive_run(id,run_id,&output,resume_stage)?;
        let published_fresh=out.get("fresh").and_then(serde_json::Value::as_bool).unwrap_or(false);
        if published_fresh {
            let refresh_reason=if force{serde_json::json!(["manual_force"])}else{initial.get("stale_reasons").cloned().unwrap_or_else(||serde_json::json!(["missing_or_stale"]))};
            let delta=competitive_updates::diff(&initial,&out,engine.config().updates.candidate_score_delta);
            let event_id=s.store.record_competitive_update_event(id,&delta,&refresh_reason)?;
            let agentic_update=process_competitive_text_update(s,id,&engine).await?;
            // Refresh after text proposals are created so callers receive current stage/pending-review state.
            out=s.store.competitive_latest_json(id)?;
            if let Some(o)=out.as_object_mut(){
                o.insert("auto_refreshed".into(),serde_json::Value::Bool(!force));
                o.insert("forced_refresh".into(),serde_json::Value::Bool(force));
                o.insert("refresh_attempt".into(),serde_json::json!(attempt+1));
                o.insert("previous_run_id".into(),initial.get("run_id").cloned().unwrap_or(serde_json::Value::Null));
                o.insert("refresh_reason".into(),refresh_reason);
                o.insert("competitive_update_event_id".into(),serde_json::json!(event_id));
                o.insert("competitive_delta".into(),serde_json::to_value(&delta)?);
                o.insert("agentic_update".into(),agentic_update);
                o.insert("competitive_updates".into(),s.store.competitive_updates_json(id,10)?);
            }
            return Ok(out);
        }
        // Most commonly means competitive config changed mid-run. Loop with a newly
        // loaded engine/config instead of returning stale data.
    }
    Err(ApiError::conflict("competitive inputs or configuration changed repeatedly during refresh; automatic retries were exhausted. Retry the operation once changes settle."))
}

async fn persist_document(s:&AppState,id:&str,name:&str,kind:&str,text:&str)->Result<serde_json::Value,ApiError>{
    if text.trim().is_empty(){return Err(ApiError::bad_request("document contains no readable text"));}
    let mut h=Sha256::new();h.update(text.as_bytes());let sha=hex::encode(h.finalize());
    let(document_id,added)=s.store.add_document(id,name,kind,text,&sha)?;
    let target=std::env::var("DOCUMENT_CHUNK_WORDS").ok().and_then(|v|v.parse().ok()).unwrap_or(420usize);
    let overlap=std::env::var("DOCUMENT_CHUNK_OVERLAP_WORDS").ok().and_then(|v|v.parse().ok()).unwrap_or(64usize);
    let chunks=chunker::chunk_text(text,target,overlap);
    s.store.replace_document_chunks(id,document_id,&chunks)?;
    Ok(serde_json::json!({"ok":true,"added":added,"document_id":document_id,"chunks":chunks.len(),"sha256":sha}))
}


async fn get_compliance_profile(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.compliance_profile_json(&id)?))}
async fn compile_compliance_profile(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    let ctx=s.store.opportunity_context(&id,180_000)?;
    if ctx.trim().is_empty(){return Err(ApiError::conflict("upload, fetch, or paste a funding opportunity before compiling sponsor submission rules"));}
    let project=s.store.project_json(&id)?;
    let prompt=format!(r#"Compile the funding opportunity into deterministic sponsor/submission rules. Return STRICT JSON only with this exact shape:
{{"profile":{{"sponsor":null,"mechanism":null,"submission_system":null,"deadline_iso":null,"rules":[{{"rule_id":"C-001","category":"format|section|attachment|deadline|budget|eligibility|submission|administrative","rule_type":"required_section|required_form|max_words|min_words|required_attachment|allowed_extensions|min_font_size_pt|min_margin_in|max_pages|deadline|required_letter_count|manual_requirement|submission_system|max_budget|project_period_max_months","scope":"proposal|section|artifact|project","target":"specific target such as Specific Aims or letters_of_support","severity":"hard|warning|info","mandatory":true,"numeric_value":null,"text_value":null,"list_value":[],"source_excerpt":"short exact wording copied from source","source_locator":"page/heading/section if visible","notes":"brief normalization explanation"}}]}}}}
Rules:
- Extract only sponsor requirements explicitly supported by the funding-opportunity source. Never invent a rule.
- Split compound instructions into atomic rules.
- Use severity=hard for explicit must/shall/required/limit/deadline rules whose violation can make the application noncompliant; warning for recommendations; info for metadata.
- Normalize dates to YYYY-MM-DD only when the date is explicit and unambiguous; otherwise create a manual_requirement preserving the source wording.
- Normalize numeric limits into numeric_value. For file extensions, put lowercase extensions without dots in list_value.
- Use required_section for explicitly required narrative sections; required_attachment for explicitly required package attachments; max_pages/max_words/min_font_size_pt/min_margin_in where explicit.
- Use required_form, never required_section, for structured portal forms such as SF424, budgets, Senior/Key Person Profiles, performance sites, and other Grants.gov form components. Structured forms must not become AI-drafted narrative sections.
- Rules that cannot be deterministically proven from the current application model must still be preserved as manual_requirement rather than dropped.
- Every rule MUST include a non-empty source_excerpt copied from the opportunity.

PROJECT METADATA:
{}

FUNDING OPPORTUNITY SOURCE:
{}"#,serde_json::to_string_pretty(&project).unwrap_or_default(),ctx);
    let out=s.router.generate(ModelTask{kind:"sponsor_compliance_compilation".into(),prompt,high_value:false}).await?;
    let parsed:ComplianceEnvelope=parse_json_from_model(&out.text)?;
    crate::compliance::validate_profile(&parsed.profile).map_err(|e|ApiError::bad_gateway(format!("invalid compiled compliance profile: {e}")))?;
    crate::compliance::validate_source_excerpts(&parsed.profile,&ctx)
        .map_err(|e|ApiError::bad_gateway(format!("compliance compiler source verification failed: {e}")))?;
    let saved=s.store.save_compliance_profile(&id,&parsed.profile,&out.model)?;
    s.store.save_analysis(&id,"sponsor_compliance_raw",&out.text)?;
    Ok(Json(saved))
}
async fn save_compliance_profile(State(s):State<AppState>,Path(id):Path<String>,Json(req):Json<ComplianceProfileInput>)->Result<Json<serde_json::Value>,ApiError>{
    crate::compliance::validate_profile(&req.profile).map_err(|e|ApiError::bad_request(e.to_string()))?;
    let ctx=s.store.opportunity_context(&id,usize::MAX)?;
    crate::compliance::validate_source_excerpts(&req.profile,&ctx).map_err(|e|ApiError::bad_request(e.to_string()))?;
    Ok(Json(s.store.save_compliance_profile(&id,&req.profile,"human_reviewed_rules")?))
}
async fn approve_compliance_profile(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.approve_compliance_profile(&id)?))}
async fn resolve_compliance_rule(State(s):State<AppState>,Path(id):Path<String>,Json(req):Json<ComplianceResolutionInput>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.resolve_compliance_rule(&id,&req.rule_id,&req.status,req.notes.as_deref().unwrap_or(""),req.resolved_by.as_deref())?))}
async fn save_compliance_measurements(State(s):State<AppState>,Path(id):Path<String>,Json(req):Json<ComplianceMeasurementsInput>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.save_compliance_measurements(&id,&req.measurements)?))}
async fn get_compliance_assessment(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.compliance_assessment_json(&id)?))}
async fn register_submission_artifact(State(s):State<AppState>,Path(id):Path<String>,Json(req):Json<SubmissionArtifactInput>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.register_submission_artifact(&id,&req.slot,&req.filename,&req.path,&req.sha256,&req.extension)?))}
async fn get_submission_artifacts(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.submission_artifacts_json(&id)?))}

async fn add_document(State(s):State<AppState>,Path(id):Path<String>,Json(req):Json<DocumentInput>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(persist_document(&s,&id,&req.name,&req.kind,&req.text).await?))}
async fn get_opportunity_source(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    let text=s.store.opportunity_context(&id,300_000)?;let fingerprint=s.store.opportunity_source_fingerprint(&id)?;
    Ok(Json(serde_json::json!({"text":text,"fingerprint":fingerprint})))
}

async fn fetch_url_document(State(s):State<AppState>,Path(id):Path<String>,Json(req):Json<FetchUrlInput>)->Result<Json<serde_json::Value>,ApiError>{
    let src=s.research.fetch(&req.url,req.name.as_deref()).await.map_err(|e|ApiError::bad_request(format!("secure URL fetch failed: {e}")))?;
    let name=req.name.unwrap_or_else(||src.title.clone()); let kind=req.kind.unwrap_or_else(||"funding_url".into());
    Ok(Json(persist_document(&s,&id,&name,&kind,&src.text).await?))
}

async fn analyze_requirements(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    if s.store.document_count(&id)?==0{return Err(ApiError::conflict("workflow gate: ingest a funding opportunity or supporting document before requirement analysis"));}
    let stage=s.store.project_stage(&id)?;
    if stage>Stage::Requirements && stage!=Stage::Documents{return Err(ApiError::conflict("workflow gate: requirement analysis cannot overwrite a downstream workflow unless new source material invalidated it first"));}
    let ctx=s.store.document_context(&id,140_000)?;
    let prompt=format!(r#"Analyze the supplied funding opportunity and supporting project materials. Return STRICT JSON only using this shape:
{{"requirements":[{{"external_id":"R-001","category":"eligibility|compliance|scientific|clinical|administrative|document|budget|deadline|format|evidence|review_criterion","requirement":"atomic requirement","mandatory":true,"evidence_needed":["item"],"dependencies":["R-000"],"source_clue":"short source wording or rationale","source_document":null,"source_locator":null}}]}}
Rules: each requirement must be atomic; preserve every explicit eligibility, deadline, budget, page/word, attachment, scientific, clinical, compliance, evidence, and review criterion; never invent a requirement; use stable unique IDs; dependencies must reference IDs in the same output.

SOURCE MATERIAL:
{ctx}"#);
    let out=s.router.generate(ModelTask{kind:"requirement_decomposition".into(),prompt,high_value:false}).await?;
    let parsed:RequirementsEnvelope=parse_json_from_model(&out.text)?;
    if parsed.requirements.is_empty(){return Err(ApiError::bad_gateway("requirement extraction returned zero requirements"));}
    s.store.replace_requirements(&id,&parsed.requirements)?;
    s.store.save_analysis(&id,"requirements_raw",&out.text)?;
    Ok(Json(serde_json::json!({"model":out.model,"count":parsed.requirements.len(),"requirements":s.store.requirements_json(&id)?})))
}
async fn get_requirements(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.requirements_json(&id)?))}
async fn approve_requirements(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    let current=s.store.project_stage(&id)?; if current!=Stage::Requirements{return Err(ApiError::conflict(format!("workflow gate: requirements can only be approved from requirements stage; current stage is {current}")));}
    let n=s.store.approve_requirements(&id)?; Ok(Json(serde_json::json!({"ok":true,"approved":n,"stage":"interview"})))
}

async fn generate_interview(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    if !s.store.requirements_all_approved(&id)?{return Err(ApiError::conflict("workflow gate: approve all parsed requirements before generating the investigator interview"));}
    let stage=s.store.project_stage(&id)?;
    if stage>Stage::Research{return Err(ApiError::conflict(format!("workflow gate: investigator interview cannot be recomputed after writing has begun; current stage is {stage}. Add/revise source material to intentionally reopen discovery.")));}
    let requirements=s.store.requirements_context(&id)?; let docs=s.store.document_context(&id,70_000)?; let answered=s.store.interview_context(&id)?;
    let prompt=format!(r#"Create the minimum investigator interview needed to close unresolved information gaps for this grant. Do not ask questions already answered by the documents or prior interview answers. Return STRICT JSON only:
{{"questions":[{{"requirement_id":"R-001","question":"specific question","answer_type":"text|integer|number|percentage|boolean|date|choice","choices":[],"unit":null,"why_needed":"why this requirement cannot yet be satisfied","evidence_requested":true,"priority":100}}]}}
Prefer typed numeric/boolean/date/choice answers over free text. Every question must map to an existing requirement ID. Prioritize mandatory and high-scoring requirements. If no question is needed, return {{"questions":[]}}.

REQUIREMENTS:
{requirements}

SOURCE MATERIAL:
{docs}

PRIOR ANSWERS:
{answered}"#);
    let out=s.router.generate(ModelTask{kind:"investigator_interview".into(),prompt,high_value:false}).await?;
    let parsed:InterviewEnvelope=parse_json_from_model(&out.text)?;
    let valid_ids=s.store.requirement_ids(&id)?;
    for q in &parsed.questions{
        if !valid_ids.iter().any(|x|x==&q.requirement_id){return Err(ApiError::bad_gateway(format!("interview model referenced unknown requirement {}",q.requirement_id)));}
        if !matches!(q.answer_type.as_str(),"text"|"integer"|"number"|"percentage"|"boolean"|"date"|"choice"){return Err(ApiError::bad_gateway(format!("invalid interview answer type {}",q.answer_type)));}
    }
    s.store.replace_open_interview_questions(&id,&parsed.questions)?;
    Ok(Json(serde_json::json!({"model":out.model,"count":parsed.questions.len(),"questions":s.store.interview_questions_json(&id)?})))
}
async fn get_interview(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.interview_questions_json(&id)?))}
async fn save_answer(State(s):State<AppState>,Path(id):Path<String>,Json(req):Json<AnswerInput>)->Result<Json<serde_json::Value>,ApiError>{
    if !s.store.interview_generated(&id)?{return Err(ApiError::conflict("workflow gate: generate the investigator interview before saving answers"));}
    if !matches!(req.confidence.as_str(),"high"|"medium"|"low"){return Err(ApiError::bad_request("invalid answer confidence"));}
    if !matches!(req.classification.as_str(),"verified_fact"|"investigator_estimate"|"assumption"|"unknown"){return Err(ApiError::bad_request("invalid answer classification"));}
    let aid=s.store.save_interview_answer(&id,req.question_id,&req.value,&req.confidence,&req.classification,req.notes.as_deref(),req.answered_by.as_deref())?;
    Ok(Json(serde_json::json!({"ok":true,"answer_id":aid,"open_questions":s.store.interview_open_count(&id)?})))
}

fn require_interview_complete(store:&Store,id:&str)->Result<(),ApiError>{
    if !store.requirements_all_approved(id)?{return Err(ApiError::conflict("workflow gate: requirements are not fully approved"));}
    if !store.interview_generated(id)?{return Err(ApiError::conflict("workflow gate: investigator interview has not been generated"));}
    let open=store.interview_open_count(id)?; if open>0{return Err(ApiError::conflict(format!("workflow gate: {open} investigator interview question(s) remain open")));}
    Ok(())
}

fn require_compliance_profile_approved(store:&Store,id:&str)->Result<(),ApiError>{
    let c=store.compliance_profile_json(id)?;
    if !c.get("exists").and_then(serde_json::Value::as_bool).unwrap_or(false){return Err(ApiError::conflict("workflow gate: compile sponsor submission rules before writing"));}
    if !c.get("fresh").and_then(serde_json::Value::as_bool).unwrap_or(false){return Err(ApiError::conflict("workflow gate: sponsor submission rules are stale because the funding opportunity changed; recompile and approve them before writing"));}
    if !c.get("approved").and_then(serde_json::Value::as_bool).unwrap_or(false){return Err(ApiError::conflict("workflow gate: human approval of the sponsor compliance profile is required before writing"));}
    Ok(())
}

async fn run_research(State(s):State<AppState>,Path(id):Path<String>,Json(req):Json<ResearchInput>)->Result<Json<serde_json::Value>,ApiError>{
    require_interview_complete(&s.store,&id)?;
    let stage=s.store.project_stage(&id)?;
    if stage<Stage::Research || stage>Stage::Science{return Err(ApiError::conflict(format!("workflow gate: research runs are allowed during research/science design before writing begins; current stage is {stage}. Add/revise source material to intentionally reopen discovery after writing.")));}
    let requirements=s.store.requirements_context(&id)?; let interview=s.store.interview_context(&id)?;
    let clinical=if s.store.clinical_study_typed(&id)?.is_some(){s.store.clinical_context(&id)?}else{"CLINICAL STUDY MODEL: not yet configured".into()};
    let max_queries=req.max_queries.unwrap_or(8).clamp(1,24); let results_per=req.results_per_query.unwrap_or(5).clamp(1,10);
    let prompt=format!(r#"Generate targeted external research queries only for unresolved evidence gaps in this grant. Return STRICT JSON only:
{{"queries":[{{"requirement_id":"R-001","query":"precise web research query","preferred_domains":["nih.gov"],"rationale":"specific evidence gap"}}]}}
Use authoritative primary sources where possible. Do not research facts already established by uploaded institutional evidence. Limit output to at most {max_queries} queries.

REQUIREMENTS:
{requirements}

INVESTIGATOR ANSWERS:
{interview}

CLINICAL STUDY DESIGN / FEASIBILITY CONTEXT:
{clinical}"#);
    let out=s.router.generate(ModelTask{kind:"research_planning".into(),prompt,high_value:false}).await?;
    let mut plan:ResearchPlanEnvelope=parse_json_from_model(&out.text)?; plan.queries.truncate(max_queries);
    let valid_ids=s.store.requirement_ids(&id)?; let mut saved=0usize; let mut failures=Vec::new();
    for q in plan.queries {
        if !valid_ids.iter().any(|x|x==&q.requirement_id){failures.push(format!("ignored research query for unknown requirement {}",q.requirement_id));continue;}
        let qid=s.store.insert_research_query(&id,&q.requirement_id,&q.query,&q.preferred_domains,&q.rationale)?;
        match s.research.search(&q.query,&q.preferred_domains,results_per).await {
            Ok(hits)=>{
                let fetched=s.research.fetch_many(hits).await; let mut valid_sources=Vec::new();
                for item in fetched{match item{Ok(src)=>valid_sources.push(src),Err(e)=>failures.push(e.to_string())}}
                if valid_sources.is_empty(){s.store.mark_research_query(qid,"complete_no_sources")?;continue;}
                let source_packet=valid_sources.iter().enumerate().map(|(i,src)|{let excerpt=src.text.chars().take(6000).collect::<String>();format!("\n--- SOURCE {i} ---\nTITLE: {}\nURL: {}\nTEXT:\n{}",src.title,src.url,excerpt)}).collect::<String>();
                let validation_prompt=format!(r#"Validate whether each supplied source supports the stated evidence need. Return STRICT JSON only:
{{"validations":[{{"source_index":0,"status":"supported|partially_supported|contradicted|irrelevant","confidence":0.0,"supporting_excerpt":"an exact verbatim excerpt copied from the source text, or empty if none","explanation":"brief reason"}}]}}
The supporting_excerpt MUST be copied exactly from the supplied source. Never manufacture a quote. A source being topically related is not enough; it must actually support or contradict the evidence need.

REQUIREMENT: {}
EVIDENCE NEED: {}
RESEARCH QUERY: {}
{}"#,q.requirement_id,q.rationale,q.query,source_packet);
                let validation_out=s.router.generate(ModelTask{kind:"evidence_validation".into(),prompt:validation_prompt,high_value:false}).await?;
                let validations:EvidenceValidationEnvelope=parse_json_from_model(&validation_out.text)?;
                for v in validations.validations {
                    if v.source_index>=valid_sources.len(){continue;}
                    let src=&valid_sources[v.source_index]; let exact=!v.supporting_excerpt.trim().is_empty()&&src.text.contains(&v.supporting_excerpt);
                    let status=match v.status.as_str(){"supported"|"partially_supported"|"contradicted"|"irrelevant"=>v.status.as_str(),_=>"candidate"};
                    let source_id=s.store.add_research_source(&id,qid,src)?.unwrap_or(0); if source_id==0||status=="irrelevant"{continue;}
                    let passage=if exact{v.supporting_excerpt.clone()}else{src.text.chars().take(1800).collect::<String>()}; let effective_status=if exact{status}else{"candidate"}; let confidence=v.confidence.clamp(0.0,1.0);
                    let ev=s.store.add_evidence(&id,Some(&q.requirement_id),"external_research",&format!("research_source:{source_id}"),&q.rationale,&passage,Some(&src.url),None,confidence,effective_status)?;
                    s.store.add_citation(&id,ev,&format!("SRC-{source_id}"),&src.title,Some(&src.url),&passage,&src.sha256,exact)?; saved+=1;
                }
                s.store.mark_research_query(qid,"complete")?;
            },
            Err(e)=>{s.store.mark_research_query(qid,"failed")?;failures.push(e.to_string());}
        }
    }
    s.store.advance_stage(&id,Stage::Research)?;
    Ok(Json(serde_json::json!({"model":out.model,"sources_saved":saved,"failures":failures,"evidence":s.store.evidence_json(&id)?})))
}
async fn get_evidence(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.evidence_json(&id)?))}

async fn draft_section(State(s):State<AppState>,Path(id):Path<String>,Json(req):Json<DraftSectionInput>)->Result<Json<serde_json::Value>,ApiError>{
    require_interview_complete(&s.store,&id)?;
    require_compliance_profile_approved(&s.store,&id)?;
    let stage=s.store.project_stage(&id)?;
    require_at_least(stage,Stage::Strategy,"drafting a section").map_err(ApiError::conflict_err)?;
    if s.store.clinical_study_typed(&id)?.is_none(){return Err(ApiError::conflict("workflow gate: save the structured clinical study model before drafting grant sections"));}
    let _competitive=ensure_competitive_fresh(&s,&id,false).await?;
    let extra=req.additional_context.unwrap_or_default(); let retrieval_query=format!("Grant section: {}. Section key: {}. Additional focus: {}",req.title,req.section_key,extra);
    let budget=std::env::var("CONTEXT_MAX_CHARS").ok().and_then(|v|v.parse().ok()).unwrap_or(48_000usize).clamp(8_000,180_000);
    let compiled=context_compiler::compile(&s.store,&s.retrieval,&id,&retrieval_query,budget).await?;
    let prompt=format!(r#"Draft the grant section named "{}". Use only information supported by the supplied compiled run context. Never fabricate citations, preliminary results, approvals, enrollment numbers, capabilities, clinical claims, or institutional facts. Distinguish verified facts from investigator estimates and assumptions. Use the PUBLIC COMPETITIVE APPLICANT INTELLIGENCE to emphasize defensible differentiators and to address capability gaps where relevant, but never state or imply that any potential competitor is a confirmed applicant. Normally position our capabilities positively rather than naming competitors in proposal prose. Where a material fact is missing, insert [EVIDENCE NEEDED: concise description]. Preserve source/citation identifiers when they are available so later citation assembly can trace the claim. Return publication-ready prose only, not commentary.

COMPILED CONTEXT:
{}

ADDITIONAL HUMAN CONTEXT:
{}"#,req.title,compiled.text,extra);
    let out=s.router.generate(ModelTask{kind:"section_draft".into(),prompt,high_value:req.high_value.unwrap_or(false)}).await?;
    if stage!=Stage::Writing{s.store.set_stage(&id,Stage::Writing)?;}
    let version=s.store.save_section(&id,&req.section_key,&req.title,&out.text,None,&format!("model:{}",out.model))?;
    Ok(Json(serde_json::json!({"model":out.model,"text":out.text,"version":version,"approved":false,"retrieval":compiled.retrieved})))
}

async fn get_section(State(s):State<AppState>,Path((id,section)):Path<(String,String)>)->Result<Json<serde_json::Value>,ApiError>{
    maybe_auto_refresh_competitive(&s,&id).await?;
    Ok(Json(s.store.section_state_json(&id,&section)?))
}
async fn save_section(State(s):State<AppState>,Path((id,section)):Path<(String,String)>,Json(req):Json<SectionInput>)->Result<Json<serde_json::Value>,ApiError>{
    require_interview_complete(&s.store,&id)?;
    require_compliance_profile_approved(&s.store,&id)?;
    let stage=s.store.project_stage(&id)?;
    require_at_least(stage,Stage::Strategy,"saving a section").map_err(ApiError::conflict_err)?;
    if s.store.clinical_study_typed(&id)?.is_none(){return Err(ApiError::conflict("workflow gate: save the structured clinical study model before saving section prose"));}
    let _competitive=ensure_competitive_fresh(&s,&id,false).await?;
    if req.body.trim().is_empty(){return Err(ApiError::bad_request("section body cannot be empty"));}
    let current=s.store.section_state_json(&id,&section)?;
    let latest_version=current.get("latest").and_then(|x|x.get("version")).and_then(serde_json::Value::as_i64);
    if let Some(latest)=latest_version {
        if req.base_version_id!=Some(latest){return Err(ApiError::conflict(format!("section changed since editing began: expected base version {latest}; reload the section before saving")));}
    }
    if stage!=Stage::Writing{s.store.set_stage(&id,Stage::Writing)?;}
    let version=s.store.save_section(&id,&section,&req.title,&req.body,req.html.as_deref(),"human_edit")?;
    Ok(Json(serde_json::json!({"ok":true,"version":version,"approved":false})))
}
async fn approve_section(State(s):State<AppState>,Path((id,section)):Path<(String,String)>,Json(req):Json<ApproveSectionInput>)->Result<Json<serde_json::Value>,ApiError>{
    require_interview_complete(&s.store,&id)?;
    require_compliance_profile_approved(&s.store,&id)?;
    let stage=s.store.project_stage(&id)?;
    require_at_least(stage,Stage::Strategy,"approving a section").map_err(ApiError::conflict_err)?;
    if s.store.clinical_study_typed(&id)?.is_none(){return Err(ApiError::conflict("workflow gate: save the structured clinical study model before approving section prose"));}
    let _competitive=ensure_competitive_fresh(&s,&id,false).await?;
    let pending=s.store.pending_competitive_update_for_section_json(&id,&section)?;
    if !pending.as_object().map(|o|o.is_empty()).unwrap_or(true){
        let event_id=pending.get("event_id").and_then(serde_json::Value::as_i64);
        let proposed=pending.get("proposed_version").and_then(serde_json::Value::as_i64);
        if proposed!=Some(req.version_id) && req.competitive_update_event_id!=event_id {
            return Err(ApiError::conflict("new public competitor intelligence updated this section; reload the highlighted update and explicitly approve or edit it before approval"));
        }
    }
    let version=s.store.approve_section_version(&id,&section,req.version_id).map_err(|e|ApiError::bad_request(e.to_string()))?;
    if stage!=Stage::Writing{s.store.set_stage(&id,Stage::Writing)?;}
    if s.store.all_required_sections_approved(&id)? && s.store.competitive_pending_update_count(&id)?==0 && s.store.competitive_text_refresh_pending_count(&id)?==0{s.store.advance_stage(&id,Stage::Review)?;}
    Ok(Json(serde_json::json!({"ok":true,"section":section,"approved_version":version,"stage":s.store.project_stage(&id)?.as_str()})))
}
async fn approved_sections(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{maybe_auto_refresh_competitive(&s,&id).await?;Ok(Json(s.store.approved_sections_json(&id)?))}
async fn approved_document(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    maybe_auto_refresh_competitive(&s,&id).await?;
    let project=s.store.project_json(&id)?;
    let sections=s.store.approved_sections_json(&id)?;
    let section_plan=s.store.project_sections_json(&id)?;
    let design=s.store.design_profile_json(&id)?;
    let readiness=s.store.readiness_json(&id)?;
    let clinical=s.store.clinical_study_json(&id)?;
    let clinical_assessment=s.store.clinical_assessment_json(&id)?;
    let competitive=s.store.competitive_latest_json(&id)?;
    let compliance_profile=s.store.compliance_profile_json(&id)?;
    let compliance_assessment=s.store.compliance_assessment_json(&id)?;
    let submission_artifacts=s.store.submission_artifacts_json(&id)?;
    let total=section_plan.as_array().map(|x|x.len()).unwrap_or(0);
    let required=section_plan.as_array().map(|x|x.iter().filter(|s|s.get("required").and_then(serde_json::Value::as_bool).unwrap_or(false)).count()).unwrap_or(0);
    let approved=sections.as_array().map(|x|x.len()).unwrap_or(0);
    Ok(Json(serde_json::json!({
        "project":project,
        "sections":sections,
        "section_plan":section_plan,
        "design_profile":design.get("profile").cloned().unwrap_or(serde_json::Value::Null),
        "design_profile_sha256":design.get("sha256").cloned().unwrap_or(serde_json::Value::Null),
        "counts":{"approved":approved,"configured":total,"required":required},
        "clinical_study":clinical,
        "clinical_assessment":clinical_assessment,
        "competitive_intelligence":competitive,
        "competitive_updates":s.store.competitive_updates_json(&id,25)?,
        "sponsor_compliance_profile":compliance_profile,
        "sponsor_compliance_assessment":compliance_assessment,
        "submission_artifacts":submission_artifacts,
        "readiness":readiness
    })))
}
async fn export_snapshot(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    maybe_auto_refresh_competitive(&s,&id).await?;
    let readiness=s.store.readiness_json(&id)?;
    if !readiness.get("ready").and_then(serde_json::Value::as_bool).unwrap_or(false){return Err(ApiError::conflict(format!("workflow gate: project is not ready for export: {}",serde_json::to_string(&readiness)?)));}
    Ok(Json(s.store.create_export_snapshot(&id)?))
}

async fn rebuild_index(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    maybe_auto_refresh_competitive(&s,&id).await?;
    if !s.store.requirements_all_approved(&id)?{return Err(ApiError::conflict("workflow gate: approve requirements before building the production knowledge index"));}
    Ok(Json(serde_json::to_value(s.retrieval.rebuild(&id).await?)?))
}
async fn index_status(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{maybe_auto_refresh_competitive(&s,&id).await?;Ok(Json(s.retrieval.status(&id)?))}
async fn retrieve_context(State(s):State<AppState>,Path(id):Path<String>,Json(req):Json<RetrieveInput>)->Result<Json<serde_json::Value>,ApiError>{
    maybe_auto_refresh_competitive(&s,&id).await?;
    if !s.store.requirements_all_approved(&id)?{return Err(ApiError::conflict("workflow gate: approve requirements before retrieval"));}
    let hits=s.retrieval.search(&id,&req.query,req.k.unwrap_or(20).clamp(1,100)).await?; Ok(Json(serde_json::to_value(hits)?))
}

async fn generate(State(s):State<AppState>,Json(req):Json<GenerateRequest>)->Result<Json<serde_json::Value>,ApiError>{let out=s.router.generate(ModelTask{kind:req.task,prompt:req.prompt,high_value:req.high_value.unwrap_or(false)}).await?;Ok(Json(serde_json::json!({"model":out.model,"text":out.text})))}
async fn hpc_benchmark(State(s):State<AppState>)->Result<Json<serde_json::Value>,ApiError>{
    let mut result=hpc::self_benchmark();
    let path=s.workspace.join(format!("hpc_benchmark_{}.bin",Uuid::new_v4()));
    let rows=10_000usize; let cols=256usize; let data=vec![0.01f32;rows*cols];
    let t=Instant::now(); vector_store::MmapMatrix::create_normalized(&path,rows,cols,&data)?; let mmap_create_ms=t.elapsed().as_secs_f64()*1000.0;
    let t=Instant::now(); let mm=vector_store::MmapMatrix::open(&path)?; let mmap_open_ms=t.elapsed().as_secs_f64()*1000.0;
    let t=Instant::now(); let scores=mm.scores(&vec![0.1;cols])?; let mmap_score_ms=t.elapsed().as_secs_f64()*1000.0;
    result["mmap_rows"]=serde_json::json!(mm.rows); result["mmap_dims"]=serde_json::json!(mm.cols);
    result["mmap_create_ms"]=serde_json::json!(mmap_create_ms); result["mmap_open_ms"]=serde_json::json!(mmap_open_ms); result["mmap_score_ms"]=serde_json::json!(mmap_score_ms);
    result["mmap_score_checksum"]=serde_json::json!(scores.iter().take(32).sum::<f32>());
    drop(mm); let _=std::fs::remove_file(&path);
    Ok(Json(result))
}

async fn system_info(State(s):State<AppState>)->Result<Json<serde_json::Value>,ApiError>{
    let db=s.workspace.join("grant.db");
    let db_bytes=std::fs::metadata(&db).map(|m|m.len()).unwrap_or(0);
    let envv=|k:&str|std::env::var(k).ok();
    Ok(Json(serde_json::json!({
        "version":env!("CARGO_PKG_VERSION"),
        "build_version":envv("GRANT_BUILD_VERSION").unwrap_or_else(||env!("CARGO_PKG_VERSION").to_string()),
        "build_revision":envv("GRANT_BUILD_REVISION").unwrap_or_else(||"development".into()),
        "runtime_profile":envv("GRANT_RUNTIME_PROFILE").unwrap_or_else(||"unknown".into()),
        "model_routing_mode":envv("MODEL_ROUTING_MODE").unwrap_or_else(||"unknown".into()),
        "embedding_model":envv("EMBEDDING_MODEL").unwrap_or_else(||"unknown".into()),
        "omp_threads":envv("OMP_NUM_THREADS").and_then(|v|v.parse::<i32>().ok()).unwrap_or(hpc::max_threads()),
        "rayon_threads":envv("RAYON_NUM_THREADS").and_then(|v|v.parse::<usize>().ok()),
        "openblas_threads":envv("OPENBLAS_NUM_THREADS").and_then(|v|v.parse::<usize>().ok()),
        "database_bytes":db_bytes,
        "competitive_refresh_seconds":envv("COMPETITIVE_REFRESH_TTL_SECONDS").and_then(|v|v.parse::<u64>().ok()),
        "workspace":s.workspace.to_string_lossy(),
        "secrets_exposed":false
    })))
}

#[derive(Debug)] struct ApiError { status:StatusCode, message:String }
impl ApiError {
    fn new(status:StatusCode,message:impl Into<String>)->Self{Self{status,message:message.into()}}
    fn bad_request(m:impl Into<String>)->Self{Self::new(StatusCode::BAD_REQUEST,m)}
    fn conflict(m:impl Into<String>)->Self{Self::new(StatusCode::CONFLICT,m)}
    fn unavailable(m:impl Into<String>)->Self{Self::new(StatusCode::SERVICE_UNAVAILABLE,m)}
    fn bad_gateway(m:impl Into<String>)->Self{Self::new(StatusCode::BAD_GATEWAY,m)}
    fn conflict_err(e:anyhow::Error)->Self{Self::conflict(e.to_string())}
}
impl From<anyhow::Error> for ApiError{fn from(e:anyhow::Error)->Self{Self::new(StatusCode::INTERNAL_SERVER_ERROR,e.to_string())}}
impl From<serde_json::Error> for ApiError{fn from(e:serde_json::Error)->Self{Self::new(StatusCode::INTERNAL_SERVER_ERROR,e.to_string())}}
impl IntoResponse for ApiError{fn into_response(self)->axum::response::Response{(self.status,Json(serde_json::json!({"error":self.message,"status":self.status.as_u16()}))).into_response()}}

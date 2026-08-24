use axum::{
    body::{to_bytes, Body},
    extract::{Path, Query, Request, State},
    http::{header::CONTENT_TYPE, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use anyhow::Context;
use parking_lot::Mutex as ParkingMutex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::Arc,
    time::Instant,
};
use tracing::{info, warn};
use uuid::Uuid;

mod auth;
mod chunker;
mod clinical;
mod competitive;
mod competitive_updates;
mod compliance;
mod context_compiler;
mod csr;
mod domain;
mod embedding;
mod hpc;
mod json_extract;
mod lexical;
mod models;
mod parquet_store;
mod record_store;
mod research;
mod retrieval;
mod source_locator;
mod storage;
mod vector_store;
mod versioning;
mod workflow;
mod workflow_artifacts;

use clinical::{ClinicalStudy, ScenarioSweepInput, StatisticsPlan};
use competitive::CompetitiveEngine;
use compliance::{ComplianceDraftEnvelope, ComplianceProfileDraft};
use domain::{
    EvidenceValidationEnvelope, InterviewEnvelope, RequirementsEnvelope, ResearchPlanEnvelope,
};
use embedding::EmbeddingClient;
use json_extract::parse_json_from_model;
use models::{ModelRouter, ModelTask};
use research::ResearchClient;
use retrieval::RetrievalService;
use auth::{EmailSettings, PasswordPolicy};
use storage::{
    IdempotencyClaim, StagedResearchQuery, StagedResearchRun, StagedResearchSource, Store,
};
use workflow_artifacts::{LiteratureQueryRecord, LiteratureSearchPlan};
use workflow::WorkflowConfig;

#[derive(Clone)]
struct AppState {
    store: Arc<Store>,
    router: Arc<ModelRouter>,
    research: Arc<ResearchClient>,
    embedding: Arc<EmbeddingClient>,
    retrieval: Arc<RetrievalService>,
    competitive_locks: Arc<ParkingMutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    workspace: PathBuf,
    auth: AuthSettings,
    email: Option<EmailSettings>,
}

#[derive(Clone)]
struct AuthSettings {
    mode: String,
    local_user_id: String,
    local_email: Option<String>,
    local_display_name: String,
    local_organization_id: String,
    internal_organization_name: String,
    bootstrap_token_sha256: Option<String>,
    trusted_gateway_secret: Option<Vec<u8>>,
    session_ttl_seconds: u64,
    reset_ttl_seconds: u64,
    login_max_failures: u32,
    login_lock_seconds: u64,
    password_policy: PasswordPolicy,
    dummy_password_hash: String,
}

#[derive(Clone, Debug, Serialize)]
struct AuthUser {
    id: String,
    email: Option<String>,
    display_name: String,
    organization_id: String,
    username: Option<String>,
    system_role: String,
    must_change_password: bool,
}

impl AuthSettings {
    fn from_env() -> anyhow::Result<Self> {
        let mode=std::env::var("AUTH_MODE").unwrap_or_else(|_|"local_single_user".into());
        if !matches!(mode.as_str(),"local_single_user"|"trusted_headers"|"internal_accounts"){anyhow::bail!("AUTH_MODE must be local_single_user, trusted_headers, or internal_accounts");}
        let bootstrap_token_sha256=if mode=="internal_accounts"{
            let value=std::env::var("INITIAL_ADMIN_SETUP_TOKEN").context("INITIAL_ADMIN_SETUP_TOKEN is required for internal account bootstrap")?;
            if value.chars().count()<24{anyhow::bail!("INITIAL_ADMIN_SETUP_TOKEN must contain at least 24 characters");}
            Some(auth::sha256(&value))
        }else{None};
        let trusted_gateway_secret=if mode=="trusted_headers"{
            let path=std::env::var("TRUSTED_GATEWAY_SECRET_FILE").context("TRUSTED_GATEWAY_SECRET_FILE is required in trusted_headers mode")?;
            let value=std::fs::read(&path).with_context(||format!("cannot read trusted gateway secret file {path}"))?;
            if value.len()!=64||!value.iter().all(u8::is_ascii_hexdigit){anyhow::bail!("trusted gateway secret must contain exactly 64 hexadecimal characters");}
            Some(value)
        }else{None};
        let parse_u64=|name:&str,default:&str|->anyhow::Result<u64>{Ok(std::env::var(name).unwrap_or_else(|_|default.into()).parse().with_context(||format!("{name} must be an integer"))?)};
        let settings=Self{
            mode,
            local_user_id:std::env::var("LOCAL_USER_ID").unwrap_or_else(|_|"local-admin".into()),
            local_email:std::env::var("LOCAL_USER_EMAIL").ok().filter(|value|!value.trim().is_empty()),
            local_display_name:std::env::var("LOCAL_USER_DISPLAY_NAME").unwrap_or_else(|_|"Local administrator".into()),
            local_organization_id:std::env::var("LOCAL_ORGANIZATION_ID").unwrap_or_else(|_|"local-organization".into()),
            internal_organization_name:std::env::var("ORGANIZATION_NAME").unwrap_or_else(|_|"Organization".into()),
            bootstrap_token_sha256,
            trusted_gateway_secret,
            session_ttl_seconds:parse_u64("AUTH_SESSION_TTL_SECONDS","43200")?,
            reset_ttl_seconds:parse_u64("PASSWORD_RESET_TTL_SECONDS","1800")?,
            login_max_failures:parse_u64("LOGIN_MAX_FAILURES","5")? as u32,
            login_lock_seconds:parse_u64("LOGIN_LOCK_SECONDS","900")?,
            password_policy:PasswordPolicy::from_env()?,
            dummy_password_hash:auth::hash_password(&auth::generate_secret())?,
        };
        if settings.mode=="local_single_user"&&(settings.local_user_id.trim().is_empty()||settings.local_organization_id.trim().is_empty()){anyhow::bail!("local authentication requires stable LOCAL_USER_ID and LOCAL_ORGANIZATION_ID");}
        Ok(settings)
    }
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
    hpc_threads: i32,
}
#[derive(Deserialize)]
struct CreateProject {
    title: String,
    sponsor: Option<String>,
    mechanism: Option<String>,
    #[serde(default)]
    sections: Vec<String>,
    workflow: Option<WorkflowConfig>,
    actor: Option<String>,
}
#[derive(Serialize)]
struct ProjectCreated {
    id: String,
    title: String,
}
#[derive(Deserialize,Default)]
struct ProjectListQuery { #[serde(default)] include_archived:bool }
#[derive(Deserialize)]
struct UpdateProjectInput { title:Option<String>,archived:Option<bool> }
#[derive(Deserialize)]
struct SectionInput {
    title: String,
    body: String,
    html: Option<String>,
    base_version_id: Option<i64>,
    actor: Option<String>,
}
#[derive(Deserialize)]
struct ApproveSectionInput {
    version_id: i64,
    competitive_update_event_id: Option<i64>,
    actor: Option<String>,
}
#[derive(Deserialize)]
struct CollaborationMessageInput {
    body: String,
    #[serde(default)]
    channel_kind: Option<String>,
    #[serde(default)]
    subject_key: Option<String>,
    #[serde(default)]
    parent_message_id: Option<i64>,
    #[serde(default)]
    mentioned_user_ids: Vec<String>,
}
#[derive(Deserialize)]
struct ProjectMemberInput {
    user_id: String,
    role: String,
}
#[derive(Deserialize)]
struct InviteInput { email:String, role:String, #[serde(default)] expires_in_days:Option<u32> }
#[derive(Deserialize)]
struct AcceptInviteInput { token:String }
#[derive(Deserialize)]
struct ChannelQuery { subject_key:Option<String> }
#[derive(Deserialize)]
struct CommentQuery { version_id:Option<i64> }
#[derive(Deserialize)]
struct CommentInput { version_id:i64,start_offset:Option<i64>,end_offset:Option<i64>,quoted_text:Option<String>,body:String,parent_comment_id:Option<i64>,#[serde(default)] mentioned_user_ids:Vec<String> }
#[derive(Deserialize)]
struct TaskInput { title:String,#[serde(default)] description:String,owner_user_id:String,#[serde(default="default_task_source")] source:String,#[serde(default="default_task_priority")] priority:String,due_at:Option<String>,#[serde(default)] dependencies:Vec<String> }
fn default_task_source()->String{"human".into()}
fn default_task_priority()->String{"normal".into()}
#[derive(Deserialize)]
struct TaskStatusInput { status:String }
#[derive(Deserialize)]
struct ReviewPanelPlanInput { mode:String }
#[derive(Deserialize)]
struct ReviewSimulationInput { panel_plan_id:String }
#[derive(Deserialize)]
struct RevisionTaskSelectionInput {
    #[serde(default)]
    task_indexes: Vec<usize>,
    owner_user_id: String,
    due_at: Option<String>,
}
#[derive(Deserialize)]
struct CausalModelInput { body:serde_json::Value, #[serde(default)] confirmed:bool }
#[derive(Deserialize)]
struct BootstrapAccountInput { setup_token:String,username:String,email:String,display_name:Option<String>,temporary_password:String }
#[derive(Deserialize)]
struct LoginInput { username:String,password:String }
#[derive(Deserialize)]
struct ChangePasswordInput { current_password:String,new_password:String }
#[derive(Deserialize)]
struct PasswordResetRequestInput { login:String }
#[derive(Deserialize)]
struct PasswordResetConfirmInput { token:String,new_password:String }
#[derive(Deserialize)]
struct CreateInternalUserInput { username:String,email:String,display_name:Option<String>,temporary_password:String }
#[derive(Deserialize, JsonSchema)]
struct PanelSynthesis {
    panel_summary: serde_json::Value,
    #[serde(default)]
    revision_tasks: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
struct RestoreSectionInput {
    version_id: i64,
    base_version_id: i64,
    actor: Option<String>,
}
#[derive(Deserialize)]
struct SectionCompareQuery {
    from_version: i64,
    to_version: i64,
}
#[derive(Deserialize)]
struct SectionMergePreviewInput {
    base_version_id: i64,
    latest_version_id: i64,
    proposed_body: String,
}
#[derive(Deserialize)]
struct GenerateRequest {
    task: String,
    prompt: String,
    high_value: Option<bool>,
}
#[derive(Deserialize)]
struct DocumentInput {
    name: String,
    kind: String,
    text: String,
}
#[derive(Deserialize)]
struct FetchUrlInput {
    url: String,
    name: Option<String>,
    kind: Option<String>,
}
#[derive(Deserialize)]
struct AnswerInput {
    question_id: i64,
    value: serde_json::Value,
    confidence: String,
    classification: String,
    notes: Option<String>,
    answered_by: Option<String>,
}
#[derive(Deserialize)]
struct DraftSectionInput {
    section_key: String,
    title: String,
    additional_context: Option<String>,
    high_value: Option<bool>,
}
#[derive(Deserialize)]
struct ResearchInput {
    search_plan_version: i64,
    results_per_query: Option<usize>,
}
#[derive(Deserialize)]
struct ResearchPlanInput {
    max_queries: Option<usize>,
}
#[derive(Deserialize)]
struct RetrieveInput {
    query: String,
    k: Option<usize>,
}
#[derive(Deserialize)]
struct DesignProfileInput {
    profile: serde_json::Value,
}
#[derive(Deserialize)]
struct ComplianceProfileInput {
    profile: ComplianceProfileDraft,
}
#[derive(Deserialize)]
struct ComplianceResolutionInput {
    rule_id: String,
    status: String,
    notes: Option<String>,
    resolved_by: Option<String>,
}
#[derive(Deserialize)]
struct ComplianceMeasurementsInput {
    measurements: serde_json::Value,
}
#[derive(Deserialize)]
struct SubmissionArtifactInput {
    slot: String,
    filename: String,
    path: String,
    sha256: String,
    extension: String,
}
#[derive(Deserialize)]
struct WorkflowImpactInput {
    workflow: WorkflowConfig,
}
#[derive(Deserialize)]
struct WorkflowUpdateInput {
    workflow: WorkflowConfig,
    expected_config_version: i64,
    actor: String,
}
#[derive(Deserialize)]
struct WorkflowArtifactInput {
    body: serde_json::Value,
    source: String,
    author: Option<String>,
    expected_version: Option<i64>,
}
#[derive(Deserialize)]
struct WorkflowArtifactApprovalInput {
    version: i64,
    approver: String,
}
#[derive(Deserialize)]
struct ReturnForRevisionInput {
    version: i64,
    rationale: String,
}
#[derive(Deserialize)]
struct WorkflowArtifactGenerateInput {
    actor: String,
    high_value: Option<bool>,
}
#[derive(Deserialize)]
struct PortableProjectInput { package: serde_json::Value }

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("grant_core=info,info")
        .json()
        .init();
    let workspace =
        PathBuf::from(std::env::var("GRANT_WORKSPACE").unwrap_or_else(|_| "/workspace".into()));
    std::fs::create_dir_all(&workspace)?;
    let store = Arc::new(Store::open(workspace.join("grant.db"))?);
    let router = Arc::new(ModelRouter::from_env()?);
    let research = Arc::new(ResearchClient::from_env()?);
    let embedding = Arc::new(EmbeddingClient::from_env()?);
    let retrieval = Arc::new(RetrievalService::new(
        store.clone(),
        embedding.clone(),
        workspace.clone(),
    ));
    // Validate competitive-intelligence configuration during startup, but reload it for
    // every refresh so enterprise config changes take effect without restarting Docker.
    let _competitive_config_check =
        CompetitiveEngine::from_env(research.clone(), embedding.clone(), router.clone())?;
    let competitive_locks = Arc::new(ParkingMutex::new(HashMap::new()));
    let auth=AuthSettings::from_env()?;
    // Internal accounts remain usable for local evaluation without an SMTP relay.
    // Email-backed account delivery and password reset fail closed at their
    // endpoints until SMTP is configured; they must not prevent the stack from starting.
    let email=EmailSettings::from_env(false)?;
    let state = AppState {
        store,
        router,
        research,
        embedding,
        retrieval,
        competitive_locks,
        workspace,
        auth,
        email,
    };
    start_competitive_background_refresh(state.clone());

    let app = Router::new()
        .route("/health", get(health))
        .route("/health/ready", get(ready))
        .route("/api/auth/bootstrap/status",get(auth_bootstrap_status))
        .route("/api/auth/bootstrap",post(auth_bootstrap))
        .route("/api/auth/login",post(auth_login))
        .route("/api/auth/logout",post(auth_logout))
        .route("/api/auth/change-password",post(auth_change_password))
        .route("/api/auth/password-reset/request",post(auth_password_reset_request))
        .route("/api/auth/password-reset/confirm",post(auth_password_reset_confirm))
        .route("/api/admin/users",get(admin_list_users).post(admin_create_user))
        .route("/api/admin/users/{user_id}/disable",post(admin_disable_user))
        .route("/api/admin/users/{user_id}/enable",post(admin_enable_user))
        .route("/api/admin/users/{user_id}/password-reset",post(admin_send_password_reset))
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/project-imports/validate",post(validate_project_import))
        .route("/api/project-imports",post(import_project))
        .route("/api/me",get(get_authenticated_user))
        .route("/api/workflow-definitions", get(get_workflow_definitions))
        .route("/api/projects/{id}", get(get_project).patch(update_project))
        .route("/api/projects/{id}/portable-export",get(export_portable_project))
        .route(
            "/api/projects/{id}/workflow",
            get(get_project_workflow).patch(update_project_workflow),
        )
        .route(
            "/api/projects/{id}/workflow/impact",
            post(preview_project_workflow_impact),
        )
        .route(
            "/api/projects/{id}/workflow/status",
            get(get_project_workflow_status),
        )
        .route("/api/projects/{id}/health",get(get_project_health))
        .route(
            "/api/projects/{id}/workflow/editor-context",
            get(get_workflow_editor_context),
        )
        .route(
            "/api/projects/{id}/generation-runs/{run}",
            get(get_generation_run),
        )
        .route(
            "/api/projects/{id}/review-panel/roles",
            get(get_proposed_reviewer_roles),
        )
        .route(
            "/api/projects/{id}/review-panel/plan",
            post(create_review_panel_plan),
        )
        .route(
            "/api/projects/{id}/review-panel/plan/{plan}/approve",
            post(approve_review_panel_plan),
        )
        .route(
            "/api/projects/{id}/review-simulations",
            post(run_review_simulation),
        )
        .route(
            "/api/projects/{id}/review-simulations/{run}",
            get(get_review_simulation),
        )
        .route(
            "/api/projects/{id}/review-simulations/{run}/approve",
            post(approve_review_simulation),
        )
        .route(
            "/api/projects/{id}/review-simulations/{run}/tasks",
            post(create_review_revision_tasks),
        )
        .route(
            "/api/projects/{id}/review-simulations/{run}/causal-models",
            get(get_causal_models).post(save_causal_model),
        )
        .route(
            "/api/projects/{id}/workflow/artifacts/{artifact_type}",
            get(get_workflow_artifact).post(save_workflow_artifact),
        )
        .route(
            "/api/projects/{id}/workflow/artifacts/{artifact_type}/approve",
            post(approve_workflow_artifact),
        )
        .route(
            "/api/projects/{id}/workflow/artifacts/{artifact_type}/return-for-revision",
            post(return_workflow_artifact_for_revision),
        )
        .route(
            "/api/projects/{id}/workflow/artifacts/{artifact_type}/generate",
            post(generate_workflow_artifact),
        )
        .route("/api/projects/{id}/readiness", get(get_readiness))
        .route(
            "/api/projects/{id}/design-profile",
            get(get_design_profile).post(save_design_profile),
        )
        .route(
            "/api/projects/{id}/clinical-study",
            get(get_clinical_study).post(save_clinical_study),
        )
        .route(
            "/api/projects/{id}/clinical-assessment",
            get(get_clinical_assessment),
        )
        .route(
            "/api/projects/{id}/clinical/sample-size",
            post(calculate_sample_size),
        )
        .route(
            "/api/projects/{id}/clinical/scenarios",
            post(run_clinical_scenarios),
        )
        .route(
            "/api/projects/{id}/competitive/profile",
            get(get_competitive_profile),
        )
        .route(
            "/api/projects/{id}/competitive/profile/generate",
            post(generate_competitive_profile),
        )
        .route(
            "/api/projects/{id}/competitive",
            get(get_competitive_intelligence),
        )
        .route(
            "/api/projects/{id}/competitive/run",
            post(run_competitive_intelligence),
        )
        .route(
            "/api/projects/{id}/competitive/refresh",
            post(refresh_competitive_intelligence),
        )
        .route(
            "/api/projects/{id}/competitive/updates",
            get(get_competitive_updates),
        )
        .route(
            "/api/projects/{id}/compliance",
            get(get_compliance_profile).post(save_compliance_profile),
        )
        .route(
            "/api/projects/{id}/compliance/compile",
            post(compile_compliance_profile),
        )
        .route(
            "/api/projects/{id}/compliance/approve",
            post(approve_compliance_profile),
        )
        .route(
            "/api/projects/{id}/compliance/resolve",
            post(resolve_compliance_rule),
        )
        .route(
            "/api/projects/{id}/compliance/measurements",
            post(save_compliance_measurements),
        )
        .route(
            "/api/projects/{id}/compliance/assessment",
            get(get_compliance_assessment),
        )
        .route(
            "/api/projects/{id}/submission-artifacts",
            get(get_submission_artifacts).post(register_submission_artifact),
        )
        .route("/api/projects/{id}/documents", post(add_document))
        .route(
            "/api/projects/{id}/documents/fetch-url",
            post(fetch_url_document),
        )
        .route(
            "/api/projects/{id}/opportunity-source",
            get(get_opportunity_source),
        )
        .route(
            "/api/projects/{id}/analyze-requirements",
            post(analyze_requirements),
        )
        .route("/api/projects/{id}/requirements", get(get_requirements))
        .route(
            "/api/projects/{id}/requirements/approve",
            post(approve_requirements),
        )
        .route(
            "/api/projects/{id}/interview/generate",
            post(generate_interview),
        )
        .route("/api/projects/{id}/interview", get(get_interview))
        .route("/api/projects/{id}/interview/answer", post(save_answer))
        .route("/api/projects/{id}/research/plan", post(generate_research_plan))
        .route("/api/projects/{id}/research/run", post(run_research))
        .route("/api/projects/{id}/evidence", get(get_evidence))
        .route("/api/projects/{id}/index/rebuild", post(rebuild_index))
        .route("/api/projects/{id}/index/status", get(index_status))
        .route("/api/projects/{id}/retrieve", post(retrieve_context))
        .route("/api/projects/{id}/draft-section", post(draft_section))
        .route("/api/projects/{id}/sections", get(project_sections))
        .route(
            "/api/projects/{id}/sections/{section}",
            get(get_section).post(save_section),
        )
        .route(
            "/api/projects/{id}/sections/{section}/versions",
            get(get_section_versions),
        )
        .route(
            "/api/projects/{id}/sections/{section}/compare",
            get(compare_section_versions),
        )
        .route(
            "/api/projects/{id}/sections/{section}/merge-preview",
            post(preview_section_merge),
        )
        .route(
            "/api/projects/{id}/sections/{section}/restore",
            post(restore_section),
        )
        .route(
            "/api/projects/{id}/sections/{section}/approve",
            post(approve_section),
        )
        .route(
            "/api/projects/{id}/sections/{section}/return-for-revision",
            post(return_section_for_revision),
        )
        .route(
            "/api/projects/{id}/collaboration",
            get(get_collaboration).post(post_collaboration_message),
        )
        .route(
            "/api/projects/{id}/collaboration/join",
            post(join_collaboration),
        )
        .route("/api/projects/{id}/collaboration/workspace",get(get_collaboration_workspace))
        .route("/api/projects/{id}/invites",get(list_invites).post(create_invite))
        .route("/api/projects/{id}/invites/{invite}/revoke",post(revoke_invite))
        .route("/api/invites/accept",post(accept_invite))
        .route("/api/projects/{id}/channels/{kind}",get(get_channel_messages).post(post_channel_message))
        .route("/api/projects/{id}/comments/{artifact_type}/{artifact_key}",get(get_comments).post(post_comment))
        .route("/api/projects/{id}/comments/{comment_id}/resolve",post(resolve_comment))
        .route("/api/projects/{id}/tasks",get(get_tasks).post(create_task))
        .route("/api/projects/{id}/tasks/{task_id}/status",post(update_task_status))
        .route("/api/notifications",get(get_notifications))
        .route("/api/notifications/{notification_id}/read",post(read_notification))
        .route(
            "/api/projects/{id}/approved-sections",
            get(approved_sections),
        )
        .route(
            "/api/projects/{id}/approved-document",
            get(approved_document),
        )
        .route("/api/projects/{id}/export-snapshot", post(export_snapshot))
        .route("/api/projects/{id}/generate", post(generate))
        .route("/api/hpc/benchmark", post(hpc_benchmark))
        .route("/api/system/info", get(system_info))
        .layer(middleware::from_fn_with_state(state.clone(),authenticate_request))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    info!("grant-core listening on 0.0.0.0:8080");
    axum::serve(listener, app).await?;
    Ok(())
}

fn request_header(req:&Request,name:&str)->Option<String>{
    req.headers().get(name).and_then(|value|value.to_str().ok()).map(str::trim).filter(|value|!value.is_empty()).map(str::to_owned)
}

fn canonical_request_sha256(content_type:&str,body:&[u8])->Result<String,ApiError>{
    let canonical=if content_type.split(';').next().is_some_and(|value|value.trim().eq_ignore_ascii_case("application/json"))&&!body.is_empty(){
        let value:serde_json::Value=serde_json::from_slice(body).map_err(|error|ApiError::bad_request(format!("invalid JSON request body: {error}")))?;
        serde_json::to_vec(&value).map_err(|error|ApiError::bad_request(format!("request body cannot be canonicalized: {error}")))?
    }else{body.to_vec()};
    Ok(format!("{:x}",Sha256::digest(canonical)))
}

fn locate_user_solicitation_sources(
    mut profile: workflow_artifacts::SolicitationProfile,
    documents: &[source_locator::SourceDocument],
) -> workflow_artifacts::SolicitationProfile {
    let locate = |statement: &str| {
        source_locator::locate_statement(statement, Some(statement), documents).map(|source| {
            workflow_artifacts::SourceAnchor {
                document_id: source.document_id,
                document_sha256: source.document_sha256,
                locator: source.locator,
                start_offset: source.start_offset,
                end_offset: source.end_offset,
                excerpt: source.excerpt,
            }
        })
    };
    for fact in profile
        .eligibility
        .iter_mut()
        .chain(profile.requirements.iter_mut())
        .chain(profile.deadlines.iter_mut())
        .chain(profile.budget_rules.iter_mut())
        .chain(profile.attachments.iter_mut())
    {
        if fact.sources.is_empty() {
            if let Some(anchor) = locate(&fact.label) {
                fact.sources.push(anchor);
                fact.status = workflow_artifacts::FactStatus::HumanCorrected;
            }
        }
    }
    for criterion in &mut profile.review_criteria {
        if criterion.sources.is_empty() {
            if let Some(anchor) = locate(&criterion.description) {
                criterion.sources.push(anchor);
                criterion.status = workflow_artifacts::FactStatus::HumanCorrected;
            }
        }
    }
    profile
}

#[cfg(test)]
mod request_contract_tests {
    use super::{canonical_request_sha256,locate_user_solicitation_sources};

    #[test]
    fn json_idempotency_hash_is_canonical_but_content_sensitive() {
        let first=canonical_request_sha256("application/json",br#"{"b":2,"a":1}"#).unwrap();
        let equivalent=canonical_request_sha256("application/json; charset=utf-8",br#"{ "a": 1, "b": 2 }"#).unwrap();
        let different=canonical_request_sha256("application/json",br#"{"a":1,"b":3}"#).unwrap();
        assert_eq!(first,equivalent);
        assert_ne!(first,different);
    }

    #[test]
    fn human_rule_ids_remain_internal_while_exact_sources_are_located() {
        let document=crate::source_locator::SourceDocument{id:7,name:"notice.txt".into(),kind:"opportunity".into(),text:"Applications must include a detailed dissemination plan describing how findings will reach community partners.".into(),sha256:"abc123".into()};
        let profile=crate::workflow_artifacts::SolicitationProfile{
            schema_version:1,working_title:"Test".into(),sponsor:"Sponsor".into(),mechanism:None,purpose:"Purpose".into(),
            eligibility:vec![],requirements:vec![crate::workflow_artifacts::SolicitationFact{id:"RULE-INTERNAL".into(),label:"Applications must include a detailed dissemination plan describing how findings will reach community partners.".into(),value:serde_json::json!("Applications must include a detailed dissemination plan describing how findings will reach community partners."),mandatory:false,status:crate::workflow_artifacts::FactStatus::HumanCorrected,sources:vec![]}],
            review_criteria:vec![],deadlines:vec![],budget_rules:vec![],attachments:vec![],open_questions:vec![]
        };
        let located=locate_user_solicitation_sources(profile,&[document]);
        assert_eq!(located.requirements[0].id,"RULE-INTERNAL");
        assert_eq!(located.requirements[0].sources.len(),1);
        assert_eq!(located.requirements[0].sources[0].document_id,7);
    }
}

async fn authenticate_request(State(s):State<AppState>,mut req:Request,next:Next)->Response{
    let path=req.uri().path().to_owned();
    if path.starts_with("/health") {return next.run(req).await;}
    let public_internal_auth=matches!(path.as_str(),"/api/auth/bootstrap/status"|"/api/auth/bootstrap"|"/api/auth/login"|"/api/auth/password-reset/request"|"/api/auth/password-reset/confirm");
    if s.auth.mode=="internal_accounts"&&public_internal_auth{return next.run(req).await;}
    let user=if s.auth.mode=="local_single_user"{
        AuthUser{id:s.auth.local_user_id.clone(),email:s.auth.local_email.clone(),display_name:s.auth.local_display_name.clone(),organization_id:s.auth.local_organization_id.clone(),username:None,system_role:"system_admin".into(),must_change_password:false}
    }else if s.auth.mode=="trusted_headers"{
        let supplied=request_header(&req,"x-grantspace-gateway-secret").unwrap_or_default();
        let expected=s.auth.trusted_gateway_secret.as_deref().unwrap_or_default();
        if !auth::constant_time_eq(supplied.as_bytes(),expected){return ApiError::new(StatusCode::UNAUTHORIZED,"trusted gateway proof is missing or invalid").into_response();}
        let Some(id)=request_header(&req,"x-grantspace-user-id") else{return ApiError::new(StatusCode::UNAUTHORIZED,"trusted identity header is missing").into_response();};
        let Some(organization_id)=request_header(&req,"x-grantspace-organization-id") else{return ApiError::new(StatusCode::UNAUTHORIZED,"trusted organization header is missing").into_response();};
        AuthUser{id,email:request_header(&req,"x-grantspace-user-email"),display_name:request_header(&req,"x-grantspace-user-name").unwrap_or_else(||"Authenticated user".into()),organization_id,username:None,system_role:"user".into(),must_change_password:false}
    }else{
        let Some(raw_token)=bearer_token(&req) else{return ApiError::new(StatusCode::UNAUTHORIZED,"a valid login session is required").into_response();};
        let session=match s.store.internal_session(&auth::sha256(&raw_token)){Ok(Some(value))=>value,Ok(None)=>return ApiError::new(StatusCode::UNAUTHORIZED,"login session is invalid or expired").into_response(),Err(error)=>return ApiError::from(error).into_response()};
        let account=session.account;
        AuthUser{id:account.id,email:Some(account.email),display_name:account.display_name,organization_id:account.organization_id,username:Some(account.username),system_role:account.system_role,must_change_password:account.must_change_password}
    };
    if s.auth.mode!="internal_accounts"{if let Err(error)=s.store.upsert_identity(&user.id,&user.organization_id,user.email.as_deref(),&user.display_name){return ApiError::from(error).into_response();}}
    if s.auth.mode=="local_single_user"{if let Err(error)=s.store.grant_legacy_projects_to_local_admin(&user.id){return ApiError::from(error).into_response();}}
    if user.must_change_password&&!matches!(path.as_str(),"/api/me"|"/api/auth/change-password"|"/api/auth/logout"){
        return ApiError::new(StatusCode::FORBIDDEN,"password_change_required").into_response();
    }
    let segments=req.uri().path().trim_matches('/').split('/').collect::<Vec<_>>();
    if segments.len()>=3&&segments[0]=="api"&&segments[1]=="projects"{
        let project_id=segments[2];
        if !project_id.is_empty(){
            let role=if user.system_role=="system_admin"{Some("owner".to_owned())}else{match s.store.ensure_organization_project_member(project_id,&user.id,&user.organization_id){Ok(role)=>role,Err(error)=>return ApiError::from(error).into_response()}};
            let Some(role)=role else{return ApiError::new(StatusCode::FORBIDDEN,"project membership is required").into_response();};
            if req.method()!=Method::GET&&role=="viewer"{return ApiError::new(StatusCode::FORBIDDEN,"viewer role cannot modify project state").into_response();}
            req.extensions_mut().insert(role);
        }
    }
    req.extensions_mut().insert(user.clone());
    let mutating = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !mutating {
        return next.run(req).await;
    }
    let Some(key)=request_header(&req,"idempotency-key") else{
        return ApiError::bad_request("Idempotency-Key is required on mutating requests").into_response();
    };
    let method=req.method().as_str().to_owned();
    let path=req.uri().path_and_query().map(|value|value.as_str()).unwrap_or(req.uri().path()).to_owned();
    let request_content_type=request_header(&req,"content-type").unwrap_or_default();
    let (parts,request_body)=req.into_parts();
    let request_bytes=match to_bytes(request_body,128*1024*1024).await{
        Ok(bytes)=>bytes,
        Err(error)=>return ApiError::bad_request(format!("request body exceeds the supported limit or is unreadable: {error}")).into_response(),
    };
    let request_sha256=match canonical_request_sha256(&request_content_type,&request_bytes){
        Ok(value)=>value,
        Err(error)=>return error.into_response(),
    };
    req=Request::from_parts(parts,Body::from(request_bytes));
    match s.store.claim_idempotency(&user.id,&key,&method,&path,&request_sha256){
        Ok(IdempotencyClaim::New)=>{}
        Ok(IdempotencyClaim::InProgress)=>return ApiError::conflict("an identical request is still in progress").into_response(),
        Ok(IdempotencyClaim::Conflict)=>return ApiError::conflict("Idempotency-Key was already used for a different operation").into_response(),
        Ok(IdempotencyClaim::Replay{status_code,content_type,body})=>{
            let status=StatusCode::from_u16(status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut response=(status,body).into_response();
            if let Ok(value)=HeaderValue::from_str(&content_type){response.headers_mut().insert(CONTENT_TYPE,value);}
            response.headers_mut().insert("idempotency-replayed",HeaderValue::from_static("true"));
            return response;
        }
        Err(error)=>return ApiError::bad_request(error.to_string()).into_response(),
    }
    let response=next.run(req).await;
    let status=response.status();
    let content_type=response.headers().get(CONTENT_TYPE).and_then(|value|value.to_str().ok()).unwrap_or("application/json").to_owned();
    let (parts,body)=response.into_parts();
    let bytes=match to_bytes(body,16*1024*1024).await{
        Ok(bytes)=>bytes,
        Err(error)=>return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR,format!("failed to record idempotent response: {error}")).into_response(),
    };
    if let Err(error)=s.store.complete_idempotency(&user.id,&key,status.as_u16(),&content_type,&bytes){
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR,format!("failed to commit idempotent response: {error}")).into_response();
    }
    Response::from_parts(parts,Body::from(bytes))
}

fn bearer_token(req:&Request)->Option<String>{
    request_header(req,"authorization").and_then(|value|value.strip_prefix("Bearer ").map(str::trim).filter(|token|!token.is_empty()).map(str::to_owned))
}

fn require_internal_mode(s:&AppState)->Result<(),ApiError>{if s.auth.mode!="internal_accounts"{Err(ApiError::new(StatusCode::NOT_FOUND,"internal account authentication is not enabled"))}else{Ok(())}}
fn require_system_admin(user:&AuthUser)->Result<(),ApiError>{if user.system_role!="system_admin"{Err(ApiError::new(StatusCode::FORBIDDEN,"system administrator permission is required"))}else{Ok(())}}
fn public_account(user:&AuthUser)->serde_json::Value{serde_json::json!({"id":user.id,"username":user.username,"email":user.email,"display_name":user.display_name,"organization_id":user.organization_id,"system_role":user.system_role,"must_change_password":user.must_change_password})}

async fn auth_bootstrap_status(State(s):State<AppState>)->Result<Json<serde_json::Value>,ApiError>{
    require_internal_mode(&s)?;
    Ok(Json(serde_json::json!({"bootstrap_required":!s.store.internal_bootstrap_complete()?})))
}

async fn auth_bootstrap(State(s):State<AppState>,Json(req):Json<BootstrapAccountInput>)->Result<Json<serde_json::Value>,ApiError>{
    require_internal_mode(&s)?;
    let supplied=auth::sha256(req.setup_token.trim());
    if s.auth.bootstrap_token_sha256.as_deref()!=Some(supplied.as_str()){return Err(ApiError::new(StatusCode::UNAUTHORIZED,"initial setup token is invalid"));}
    let username=auth::normalize_username(&req.username).map_err(|error|ApiError::bad_request(error.to_string()))?;
    let email=auth::normalize_email(&req.email).map_err(|error|ApiError::bad_request(error.to_string()))?;
    s.auth.password_policy.validate(&req.temporary_password,&username,&email).map_err(|error|ApiError::bad_request(error.to_string()))?;
    let display_name=req.display_name.as_deref().map(str::trim).filter(|value|!value.is_empty()).unwrap_or(&username);
    let password_hash=auth::hash_password(&req.temporary_password).map_err(ApiError::from)?;
    let account=s.store.bootstrap_internal_admin(&s.auth.local_organization_id,&s.auth.internal_organization_name,&username,&email,display_name,&password_hash).map_err(|error|ApiError::conflict(error.to_string()))?;
    Ok(Json(serde_json::json!({"created":true,"user":{"id":account.id,"username":account.username,"email":account.email,"display_name":account.display_name,"system_role":account.system_role,"must_change_password":true}})))
}

async fn auth_login(State(s):State<AppState>,Json(req):Json<LoginInput>)->Result<Json<serde_json::Value>,ApiError>{
    require_internal_mode(&s)?;
    let login=req.username.trim().to_ascii_lowercase();
    let account=s.store.internal_account_by_login(&login)?;
    let valid=account.as_ref().map(|value|auth::verify_password(&req.password,&value.password_hash)).unwrap_or_else(||auth::verify_password(&req.password,&s.auth.dummy_password_hash));
    let Some(account)=account else{return Err(ApiError::new(StatusCode::UNAUTHORIZED,"username or password is incorrect"));};
    if !valid{
        s.store.record_login_failure(&account.id,s.auth.login_max_failures,s.auth.login_lock_seconds)?;
        return Err(ApiError::new(StatusCode::UNAUTHORIZED,"username or password is incorrect"));
    }
    if !account.active{return Err(ApiError::new(StatusCode::UNAUTHORIZED,"username or password is incorrect"));}
    if account.locked{return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS,"account is temporarily locked; try again later or reset the password"));}
    s.store.record_login_success(&account.id)?;
    let raw_token=auth::generate_secret();
    let expires_at=s.store.create_auth_session(&account.id,&auth::sha256(&raw_token),s.auth.session_ttl_seconds)?;
    Ok(Json(serde_json::json!({"access_token":raw_token,"token_type":"Bearer","expires_at":expires_at,"user":{"id":account.id,"username":account.username,"email":account.email,"display_name":account.display_name,"organization_id":account.organization_id,"system_role":account.system_role,"must_change_password":account.must_change_password}})))
}

async fn auth_logout(State(s):State<AppState>,req:Request)->Result<Json<serde_json::Value>,ApiError>{
    require_internal_mode(&s)?;
    let token=bearer_token(&req).context("login session is required").map_err(|error|ApiError::new(StatusCode::UNAUTHORIZED,error.to_string()))?;
    s.store.revoke_auth_session(&auth::sha256(&token))?;
    Ok(Json(serde_json::json!({"logged_out":true})))
}

async fn auth_change_password(State(s):State<AppState>,Extension(user):Extension<AuthUser>,req:Request)->Result<Json<serde_json::Value>,ApiError>{
    require_internal_mode(&s)?;
    let current_token=request_header(&req,"authorization").and_then(|value|value.strip_prefix("Bearer ").map(str::trim).map(str::to_owned));
    let bytes=to_bytes(req.into_body(),1024*1024).await.map_err(|error|ApiError::bad_request(error.to_string()))?;
    let input:ChangePasswordInput=serde_json::from_slice(&bytes).map_err(|error|ApiError::bad_request(error.to_string()))?;
    let account=s.store.internal_account_by_id(&user.id)?.context("active account not found").map_err(|error|ApiError::new(StatusCode::UNAUTHORIZED,error.to_string()))?;
    if !auth::verify_password(&input.current_password,&account.password_hash){return Err(ApiError::new(StatusCode::UNAUTHORIZED,"current password is incorrect"));}
    s.auth.password_policy.validate(&input.new_password,&account.username,&account.email).map_err(|error|ApiError::bad_request(error.to_string()))?;
    if auth::verify_password(&input.new_password,&account.password_hash){return Err(ApiError::bad_request("new password must be different from the current password"));}
    s.store.change_internal_password(&user.id,&auth::hash_password(&input.new_password)?)?;
    if let Some(token)=current_token{s.store.revoke_other_auth_sessions(&user.id,&auth::sha256(&token))?;}
    Ok(Json(serde_json::json!({"password_changed":true,"must_change_password":false})))
}

async fn auth_password_reset_request(State(s):State<AppState>,Json(req):Json<PasswordResetRequestInput>)->Result<Json<serde_json::Value>,ApiError>{
    require_internal_mode(&s)?;
    if let Some(account)=s.store.internal_account_by_login(req.login.trim())?{
        if account.active{
            let token=auth::generate_secret();
            s.store.create_password_reset_token(&account.id,&auth::sha256(&token),"self_service",s.auth.reset_ttl_seconds,None)?;
            if let Some(email)=s.email.clone(){let address=account.email.clone();let minutes=(s.auth.reset_ttl_seconds/60).max(1);tokio::task::spawn_blocking(move||email.send_password_reset(&address,&token,minutes)).await.map_err(|error|ApiError::bad_gateway(error.to_string()))?.map_err(|error|{warn!(error=%error,"password reset email delivery failed");ApiError::bad_gateway("password reset email could not be delivered")})?;}
        }
    }
    Ok(Json(serde_json::json!({"accepted":true,"message":"If an active account matches, a single-use reset link has been sent."})))
}

async fn auth_password_reset_confirm(State(s):State<AppState>,Json(req):Json<PasswordResetConfirmInput>)->Result<Json<serde_json::Value>,ApiError>{
    require_internal_mode(&s)?;
    if req.token.trim().len()<32{return Err(ApiError::bad_request("password reset link is invalid"));}
    let token_hash=auth::sha256(req.token.trim());
    let user_id=s.store.password_reset_user(&token_hash)?.context("password reset link is invalid, expired, or already used").map_err(|error|ApiError::bad_request(error.to_string()))?;
    let account=s.store.internal_account_by_id(&user_id)?.context("active account not found").map_err(|error|ApiError::bad_request(error.to_string()))?;
    s.auth.password_policy.validate(&req.new_password,&account.username,&account.email).map_err(|error|ApiError::bad_request(error.to_string()))?;
    s.store.consume_password_reset(&token_hash,&auth::hash_password(&req.new_password)?)?;
    Ok(Json(serde_json::json!({"password_reset":true})))
}

async fn admin_list_users(State(s):State<AppState>,Extension(user):Extension<AuthUser>)->Result<Json<serde_json::Value>,ApiError>{require_internal_mode(&s)?;require_system_admin(&user)?;Ok(Json(s.store.internal_users_json(&user.organization_id)?))}

async fn admin_create_user(State(s):State<AppState>,Extension(user):Extension<AuthUser>,Json(req):Json<CreateInternalUserInput>)->Result<Json<serde_json::Value>,ApiError>{
    require_internal_mode(&s)?;require_system_admin(&user)?;
    let username=auth::normalize_username(&req.username).map_err(|error|ApiError::bad_request(error.to_string()))?;
    let email=auth::normalize_email(&req.email).map_err(|error|ApiError::bad_request(error.to_string()))?;
    s.auth.password_policy.validate(&req.temporary_password,&username,&email).map_err(|error|ApiError::bad_request(error.to_string()))?;
    let display_name=req.display_name.as_deref().map(str::trim).filter(|value|!value.is_empty()).unwrap_or(&username);
    let account=s.store.create_internal_user(&user.id,&user.organization_id,&username,&email,display_name,&auth::hash_password(&req.temporary_password)?).map_err(|error|ApiError::conflict(error.to_string()))?;
    let (email_sent,delivery_error)=if let Some(email_settings)=s.email.clone(){
        let address=account.email.clone();let temporary_password=req.temporary_password.clone();let mail_username=account.username.clone();
        let delivery=tokio::task::spawn_blocking(move||email_settings.send_new_account(&address,&mail_username,&temporary_password)).await.map_err(|error|ApiError::bad_gateway(error.to_string()))?;
        (delivery.is_ok(),delivery.err().map(|error|error.to_string()))
    }else{(false,Some("SMTP delivery is not configured; provide the temporary password to the user through an approved channel and configure SMTP before enabling email resets".to_owned()))};
    Ok(Json(serde_json::json!({"created":true,"email_sent":email_sent,"delivery_error":delivery_error,"user":{"id":account.id,"username":account.username,"email":account.email,"display_name":account.display_name,"system_role":account.system_role,"must_change_password":true}})))
}

async fn admin_disable_user(State(s):State<AppState>,Extension(user):Extension<AuthUser>,Path(user_id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{require_internal_mode(&s)?;require_system_admin(&user)?;s.store.set_internal_user_active(&user.id,&user_id,false).map_err(|error|ApiError::bad_request(error.to_string()))?;Ok(Json(serde_json::json!({"disabled":true,"user_id":user_id})))}
async fn admin_enable_user(State(s):State<AppState>,Extension(user):Extension<AuthUser>,Path(user_id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{require_internal_mode(&s)?;require_system_admin(&user)?;s.store.set_internal_user_active(&user.id,&user_id,true).map_err(|error|ApiError::bad_request(error.to_string()))?;Ok(Json(serde_json::json!({"enabled":true,"user_id":user_id})))}

async fn admin_send_password_reset(State(s):State<AppState>,Extension(user):Extension<AuthUser>,Path(user_id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    require_internal_mode(&s)?;require_system_admin(&user)?;
    let account=s.store.internal_account_by_id(&user_id)?.context("account not found").map_err(|error|ApiError::not_found(error.to_string()))?;
    let token=auth::generate_secret();let expires_at=s.store.create_password_reset_token(&account.id,&auth::sha256(&token),"administrator_reset",s.auth.reset_ttl_seconds,Some(&user.id))?;
    let email=s.email.clone().context("SMTP delivery is not configured").map_err(ApiError::from)?;let address=account.email;let minutes=(s.auth.reset_ttl_seconds/60).max(1);
    tokio::task::spawn_blocking(move||email.send_password_reset(&address,&token,minutes)).await.map_err(|error|ApiError::bad_gateway(error.to_string()))?.map_err(|error|ApiError::bad_gateway(error.to_string()))?;
    Ok(Json(serde_json::json!({"sent":true,"expires_at":expires_at,"user_id":user_id})))
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        hpc_threads: hpc::max_threads(),
    })
}
async fn ready(State(s): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    let model = s
        .router
        .health()
        .await
        .map_err(|e| ApiError::unavailable(format!("model backend not ready: {e}")))?;
    let embedding = s
        .embedding
        .health()
        .await
        .map_err(|e| ApiError::unavailable(format!("embedding model not ready: {e}")))?;
    let ingestion = s
        .research
        .ingestion_health()
        .await
        .map_err(|e| ApiError::unavailable(format!("document ingestion not ready: {e}")))?;
    Ok(Json(
        serde_json::json!({"status":"ready","version":env!("CARGO_PKG_VERSION"),"model":model,"embedding":embedding,"ingestion":ingestion,"hpc_threads":hpc::max_threads()}),
    ))
}

async fn list_projects(State(s): State<AppState>,Extension(user):Extension<AuthUser>,Query(query):Query<ProjectListQuery>) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(if user.system_role=="system_admin"{s.store.list_projects_json(query.include_archived)?}else{s.store.list_projects_for_user_json(&user.id,&user.organization_id,query.include_archived)?}))
}
async fn get_authenticated_user(Extension(user):Extension<AuthUser>)->Json<AuthUser>{Json(user)}
async fn create_project(
    State(s): State<AppState>,
    Extension(user):Extension<AuthUser>,
    Json(req): Json<CreateProject>,
) -> Result<Json<ProjectCreated>, ApiError> {
    if req.title.trim().is_empty() {
        return Err(ApiError::bad_request("working title is required"));
    }
    let id = Uuid::new_v4().to_string();
    let workflow = match req.workflow {
        Some(value) => value,
        None => s.store.default_workflow_config()?,
    };
    s.router
        .project_policy(&workflow)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    s.store
        .create_project_with_workflow(
            &id,
            req.title.trim(),
            req.sponsor.as_deref(),
            req.mechanism.as_deref(),
            &req.sections,
            &workflow,
            Some(&user.id),
        )
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    s.store.add_project_member(&id,&user.id,"owner",None)?;
    s.store.ensure_project_channel(&id,"general",None,"General",&user.id)?;
    std::fs::create_dir_all(s.workspace.join("projects").join(&id)).map_err(anyhow::Error::from)?;
    Ok(Json(ProjectCreated {
        id,
        title: req.title,
    }))
}
async fn update_project(State(s):State<AppState>,Extension(user):Extension<AuthUser>,Extension(role):Extension<String>,Path(id):Path<String>,Json(req):Json<UpdateProjectInput>)->Result<Json<serde_json::Value>,ApiError>{
    require_roles(&role,&["owner","pi","research_administrator"])?;
    if req.title.is_none()&&req.archived.is_none(){return Err(ApiError::bad_request("provide a title and/or archived state"));}
    Ok(Json(s.store.update_project_metadata(&id,req.title.as_deref(),req.archived,&user.id).map_err(|error|ApiError::bad_request(error.to_string()))?))
}
async fn get_workflow_definitions(
    State(s): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(s.store.workflow_registry_json()?))
}
async fn export_portable_project(
    State(s):State<AppState>,
    Path(id):Path<String>,
)->Result<Json<serde_json::Value>,ApiError>{
    Ok(Json(s.store.portable_project_package(&id)?))
}

async fn validate_project_import(
    State(s):State<AppState>,
    Json(req):Json<PortableProjectInput>,
)->Result<Json<serde_json::Value>,ApiError>{
    Ok(Json(s.store.validate_portable_project_package(&req.package).map_err(|error|ApiError::bad_request(error.to_string()))?))
}

async fn import_project(
    State(s):State<AppState>,
    Extension(user):Extension<AuthUser>,
    Json(req):Json<PortableProjectInput>,
)->Result<Json<serde_json::Value>,ApiError>{
    Ok(Json(s.store.import_portable_project_package(&req.package,&user.id).map_err(|error|ApiError::bad_request(error.to_string()))?))
}
async fn get_project_workflow(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut workflow = s.store.workflow_json(&id)?;
    let config = s.store.workflow_config(&id)?;
    let routing = s
        .router
        .routing_disclosure(&config)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    if let Some(object) = workflow.as_object_mut() {
        object.insert("routing".into(), routing);
        object.insert(
            "generation_runs".into(),
            s.store.generation_runs_json(&id, 25)?,
        );
    }
    Ok(Json(workflow))
}
async fn get_project_workflow_status(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(s.store.workflow_status_json(&id)?))
}
async fn get_project_health(
    State(s):State<AppState>,
    Path(id):Path<String>,
)->Result<Json<serde_json::Value>,ApiError>{
    Ok(Json(s.store.project_health_json(&id)?))
}
async fn get_proposed_reviewer_roles(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store, &id, "review_simulator")?;
    Ok(Json(
        s.store
            .proposed_reviewer_roles_json(&id)
            .map_err(ApiError::conflict_err)?,
    ))
}
async fn create_review_panel_plan(
    State(s): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Extension(role): Extension<String>,
    Path(id): Path<String>,
    Json(req): Json<ReviewPanelPlanInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store, &id, "review_simulator")?;
    require_roles(&role, &["owner", "pi", "contributor", "reviewer", "research_administrator"])?;
    Ok(Json(
        s.store
            .create_review_panel_plan(&id, &req.mode, &user.id)
            .map_err(|error| ApiError::bad_request(error.to_string()))?,
    ))
}

async fn approve_review_panel_plan(
    State(s): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Extension(role): Extension<String>,
    Path((id, plan)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store, &id, "review_simulator")?;
    require_roles(&role, &["owner", "pi", "approver", "research_administrator"])?;
    Ok(Json(
        s.store
            .approve_review_panel_plan(&id, &plan, &user.id)
            .map_err(ApiError::conflict_err)?,
    ))
}

fn review_anchor_sets(
    snapshot: &serde_json::Value,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let proposal = snapshot
        .get("sections")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|section| section.get("anchor_id").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let literature = snapshot
        .pointer("/literature_manifest/body")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let mut evidence = BTreeSet::new();
    for (field, prefix) in [
        ("source_ids", "source"),
        ("citation_ids", "citation"),
    ] {
        for id in literature
            .get(field)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(value) = id.as_str().map(str::to_owned).or_else(|| id.as_i64().map(|value| value.to_string())) {
                evidence.insert(format!("{prefix}:{value}"));
            }
        }
    }
    for id in literature
        .get("evidence_needs")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|need| {
            need.get("evidence_ids")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
        })
    {
        if let Some(value) = id.as_str().map(str::to_owned).or_else(|| id.as_i64().map(|value| value.to_string())) {
            evidence.insert(format!("evidence:{value}"));
        }
    }
    (proposal, evidence)
}

fn validate_revision_tasks(tasks: &[serde_json::Value], proposal_anchors: &BTreeSet<String>) -> anyhow::Result<()> {
    for (index, task) in tasks.iter().enumerate() {
        let object = task.as_object().with_context(|| format!("revision task {index} must be an object"))?;
        for field in ["title", "description", "priority", "rationale"] {
            if object.get(field).and_then(serde_json::Value::as_str).map(str::trim).unwrap_or_default().is_empty() {
                anyhow::bail!("revision task {index} requires {field}");
            }
        }
        let anchors = object.get("proposal_anchors").and_then(serde_json::Value::as_array).context("revision task requires proposal_anchors")?;
        if anchors.is_empty() {
            anyhow::bail!("revision task {index} requires at least one proposal anchor");
        }
        for anchor in anchors {
            let anchor = anchor.as_str().context("revision task proposal anchor must be a string")?;
            if !proposal_anchors.contains(anchor) {
                anyhow::bail!("revision task {index} references unknown proposal anchor {anchor}");
            }
        }
    }
    Ok(())
}

async fn run_review_simulation(
    State(s): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Extension(role): Extension<String>,
    Path(id): Path<String>,
    Json(req): Json<ReviewSimulationInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store, &id, "review_simulator")?;
    require_roles(&role, &["owner", "pi", "reviewer", "research_administrator"])?;
    let snapshot = s
        .store
        .create_review_snapshot(&id, &user.id)
        .map_err(ApiError::conflict_err)?;
    let snapshot_id = snapshot.get("id").and_then(serde_json::Value::as_str).context("snapshot ID missing")?.to_owned();
    let started = s
        .store
        .begin_review_run(&id, &snapshot_id, &req.panel_plan_id, &user.id)
        .map_err(ApiError::conflict_err)?;
    let run_id = started.get("id").and_then(serde_json::Value::as_str).context("review run ID missing")?.to_owned();

    let execution = async {
        let inputs = s.store.review_execution_inputs(&id, &run_id)?;
        let snapshot = inputs.get("snapshot").context("review snapshot missing")?;
        let plan_value = inputs.get("panel_plan").context("review panel plan missing")?;
        let plan: workflow_artifacts::ReviewerPanelPlan = serde_json::from_value(plan_value.clone())?;
        workflow_artifacts::validate_panel_plan(&plan)?;
        let solicitation_value = snapshot.pointer("/solicitation_profile/body").context("solicitation profile body missing")?;
        let solicitation: workflow_artifacts::SolicitationProfile = serde_json::from_value(solicitation_value.clone())?;
        let sections = snapshot.get("sections").cloned().unwrap_or_else(|| serde_json::json!([]));
        let rubric = serde_json::to_value(&solicitation.review_criteria)?;
        let (proposal_anchors, evidence_anchors) = review_anchor_sets(snapshot);
        let mut reviews = Vec::with_capacity(plan.roles.len());

        for reviewer_role in &plan.roles {
            let role_criteria = solicitation
                .review_criteria
                .iter()
                .filter(|criterion| reviewer_role.criterion_ids.contains(&criterion.id))
                .collect::<Vec<_>>();
            let prompt = format!(
                r#"You are performing one independent synthetic grant review for the approved role archetype below. You have no access to other reviewers. Use only the supplied approved solicitation criteria and immutable proposal snapshot. Return strict JSON matching this shape:
{{"reviewer_archetype":"exact role key","criterion_scores":[{{"criterion_id":"exact criterion ID","score":null,"strengths":["grounded observation"],"weaknesses":["grounded observation"],"proposal_anchors":["exact allowed proposal anchor"],"solicitation_anchors":["exact criterion ID"],"confidence":0.0}}],"overall_assessment":"...","questions":["..."]}}
Rules:
- Cover every assigned criterion exactly once and no other criterion.
- For scored criteria, use a numeric score inside the stated solicitation scale. For narrative criteria, score must be null.
- Every strength and weakness must be traceable to the cited proposal and solicitation anchors.
- Do not infer named reviewer preferences, private deliberations, award probability, or funding outcome.
- Preserve uncertainty and identify unavailable information in the assessment.

ROLE:
{}

ASSIGNED CRITERIA:
{}

ALLOWED PROPOSAL SECTIONS AND ANCHORS:
{}"#,
                serde_json::to_string_pretty(reviewer_role)?,
                serde_json::to_string_pretty(&role_criteria)?,
                serde_json::to_string_pretty(&sections)?,
            );
            let output = s.router.generate_for_project(
                s.store.as_ref(),
                &id,
                ModelTask::structured::<workflow_artifacts::SimulatedReviewerResult>(
                    "review_simulation",prompt,true,"simulated_reviewer_result",1,
                )?,
            ).await?;
            let review: workflow_artifacts::SimulatedReviewerResult = parse_json_from_model(&output.text)?;
            workflow_artifacts::validate_grounded_individual_review(
                &review,
                reviewer_role,
                &solicitation,
                &proposal_anchors,
            )?;
            reviews.push(review);
        }

        let consensus_prompt = format!(
            r#"Synthesize the validated independent synthetic reviews below. You may not change, replace, or silently reconcile their individual scores. Return strict JSON with exactly this shape:
{{"panel_summary":{{"summary":"...","shared_strengths":[],"shared_weaknesses":[],"disagreements":[{{"criterion_id":"...","positions":[]}}],"score_distribution":{{}}}},"revision_tasks":[{{"title":"...","description":"...","priority":"critical|high|medium|low","rationale":"...","proposal_anchors":["exact allowed proposal anchor"],"criterion_ids":["exact criterion ID"]}}]}}
Every revision task must be grounded in at least one allowed proposal anchor. Preserve genuine disagreement. This is synthetic decision support, not an award prediction.

SOLICITATION RUBRIC:
{}

ALLOWED PROPOSAL ANCHORS:
{}

VALIDATED INDEPENDENT REVIEWS:
{}"#,
            serde_json::to_string_pretty(&rubric)?,
            serde_json::to_string_pretty(&sections)?,
            serde_json::to_string_pretty(&reviews)?,
        );
        let consensus_output = s.router.generate_for_project(
            s.store.as_ref(),
            &id,
            ModelTask::structured::<PanelSynthesis>(
                "review_simulation",consensus_prompt,true,"panel_synthesis",1,
            )?,
        ).await?;
        let consensus: PanelSynthesis = parse_json_from_model(&consensus_output.text)?;
        validate_revision_tasks(&consensus.revision_tasks, &proposal_anchors)?;

        let causal_analysis = if plan.mode == "consensus_causal" {
            let prompt = format!(
                r#"Perform a synthetic causal-methods critique using only the immutable approved proposal snapshot and literature manifest below. Return strict JSON matching this exact shape:
{{"mode":"program_argument_causality|causal_study_validity","graph":{{"nodes":[{{"id":"...","kind":"intervention|exposure|population|outcome|mediator|moderator|confounder|selection|measurement|context","label":"...","inferred":true}}],"edges":[{{"from":"node id","to":"node id","relationship":"...","evidence_anchors":["exact allowed anchor"],"inferred":true}}]}},"assumptions":[],"threats":[],"claim_checks":[]}}
Use causal_study_validity when the proposal claims an identifiable effect; otherwise use program_argument_causality. Every edge must cite an exact allowed proposal or evidence anchor. Label model-proposed nodes and edges inferred=true. Do not claim certainty.

ALLOWED PROPOSAL ANCHORS:
{}

ALLOWED LITERATURE/EVIDENCE ANCHORS:
{}

APPROVED SNAPSHOT:
{}"#,
                serde_json::to_string_pretty(&proposal_anchors)?,
                serde_json::to_string_pretty(&evidence_anchors)?,
                serde_json::to_string_pretty(snapshot)?,
            );
            let output = s.router.generate_for_project(
                s.store.as_ref(),
                &id,
                ModelTask::structured::<workflow_artifacts::CausalAnalysisResult>(
                    "causal_analysis",prompt,true,"causal_analysis",1,
                )?,
            ).await?;
            Some(parse_json_from_model::<workflow_artifacts::CausalAnalysisResult>(&output.text)?)
        } else {
            None
        };

        let result = workflow_artifacts::ReviewSimulationResult {
            schema_version: 1,
            snapshot_id: snapshot_id.clone(),
            rubric_version_id: inputs.get("rubric_version_id").and_then(serde_json::Value::as_str).context("rubric version ID missing")?.to_owned(),
            panel_plan_id: req.panel_plan_id.clone(),
            reviews,
            causal_analysis,
            panel_summary: consensus.panel_summary,
            revision_tasks: consensus.revision_tasks,
            synthetic_review_notice: plan.synthetic_review_notice.clone(),
        };
        workflow_artifacts::validate_grounded_review_result(
            &result,
            &plan,
            &solicitation,
            &proposal_anchors,
            &evidence_anchors,
        )?;
        s.store.finish_review_run(&id, &run_id, &result)
    }.await;

    match execution {
        Ok(result) => Ok(Json(result)),
        Err(error) => {
            if let Err(record_error) = s.store.fail_review_run(&id, &run_id, &error.to_string()) {
                warn!(review_run=%run_id,error=%record_error,"failed to record review run failure");
            }
            Err(ApiError::bad_gateway(format!("review simulation failed: {error}")))
        }
    }
}

async fn get_review_simulation(
    State(s): State<AppState>,
    Path((id, run)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store, &id, "review_simulator")?;
    Ok(Json(s.store.review_run_json(&id, &run)?))
}

async fn approve_review_simulation(
    State(s): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Extension(role): Extension<String>,
    Path((id, run)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store, &id, "review_simulator")?;
    require_roles(&role, &["owner", "pi", "approver", "research_administrator"])?;
    Ok(Json(
        s.store
            .approve_review_run(&id, &run, &user.id)
            .map_err(ApiError::conflict_err)?,
    ))
}

async fn create_review_revision_tasks(
    State(s): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path((id, run)): Path<(String, String)>,
    Json(req): Json<RevisionTaskSelectionInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store, &id, "review_simulator")?;
    require_module_enabled(&s.store, &id, "team_collaboration")?;
    let review = s.store.review_run_json(&id, &run)?;
    if review.get("status").and_then(serde_json::Value::as_str) != Some("complete") {
        return Err(ApiError::conflict("review run must be complete before creating revision tasks"));
    }
    let tasks = review.pointer("/result/revision_tasks").and_then(serde_json::Value::as_array).context("completed review has no revision task list")?;
    let selected = if req.task_indexes.is_empty() { (0..tasks.len()).collect::<Vec<_>>() } else { req.task_indexes };
    let mut created = Vec::new();
    for index in selected {
        let task = tasks.get(index).with_context(|| format!("revision task index {index} does not exist"))?;
        let title = task.get("title").and_then(serde_json::Value::as_str).context("revision task title missing")?;
        let description = task.get("description").and_then(serde_json::Value::as_str).context("revision task description missing")?;
        let priority = task.get("priority").and_then(serde_json::Value::as_str).unwrap_or("high");
        created.push(s.store.create_task(
            &id,
            title,
            description,
            &req.owner_user_id,
            &format!("review_simulation:{run}:{index}"),
            priority,
            req.due_at.as_deref(),
            &user.id,
            &[],
        )?);
    }
    Ok(Json(serde_json::json!({"review_run_id":run,"created_tasks":created})))
}

async fn get_causal_models(
    State(s): State<AppState>,
    Path((id, run)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store, &id, "review_simulator")?;
    Ok(Json(s.store.causal_models_json(&id, &run)?))
}

async fn save_causal_model(
    State(s): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Extension(role): Extension<String>,
    Path((id, run)): Path<(String, String)>,
    Json(req): Json<CausalModelInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store, &id, "review_simulator")?;
    if req.confirmed {
        require_roles(&role, &["owner", "pi", "approver", "research_administrator"])?;
    }
    Ok(Json(
        s.store
            .save_causal_model_version(&id, &run, &req.body, &user.id, req.confirmed)
            .map_err(|error| ApiError::bad_request(error.to_string()))?,
    ))
}
async fn preview_project_workflow_impact(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<WorkflowImpactInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.router
        .project_policy(&req.workflow)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    Ok(Json(
        s.store
            .workflow_impact_json(&id, &req.workflow)
            .map_err(|error| ApiError::bad_request(error.to_string()))?,
    ))
}
async fn update_project_workflow(
    State(s): State<AppState>,
    Extension(user):Extension<AuthUser>,
    Extension(role):Extension<String>,
    Path(id): Path<String>,
    Json(req): Json<WorkflowUpdateInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_roles(&role,&["owner","pi","research_administrator"])?;
    s.router
        .project_policy(&req.workflow)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    Ok(Json(
        s.store
            .update_workflow_config(&id, &req.workflow, req.expected_config_version, &user.id)
            .map_err(ApiError::conflict_err)?,
    ))
}
async fn get_workflow_artifact(
    State(s): State<AppState>,
    Path((id, artifact_type)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(s.store.workflow_artifact_json(&id, &artifact_type)?))
}
async fn get_workflow_editor_context(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(s.store.workflow_editor_context_json(&id)?))
}
async fn get_generation_run(
    State(s):State<AppState>,
    Path((id,run)):Path<(String,String)>,
)->Result<Json<serde_json::Value>,ApiError>{
    Ok(Json(s.store.generation_run_json(&id,&run)?))
}
async fn save_workflow_artifact(
    State(s): State<AppState>,
    Extension(user):Extension<AuthUser>,
    Path((id, artifact_type)): Path<(String, String)>,
    Json(req): Json<WorkflowArtifactInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if req.source.trim().is_empty() {
        return Err(ApiError::bad_request("artifact source is required"));
    }
    let body=if artifact_type=="solicitation_profile"{
        let profile:workflow_artifacts::SolicitationProfile=serde_json::from_value(req.body)
            .map_err(|error|ApiError::bad_request(format!("invalid solicitation profile: {error}")))?;
        let documents=s.store.opportunity_documents(&id)?;
        serde_json::to_value(locate_user_solicitation_sources(profile,&documents))?
    }else{req.body};
    Ok(Json(
        s.store
            .save_workflow_artifact(
                &id,
                &artifact_type,
                &body,
                &req.source,
                Some(&user.id),
                req.expected_version,
            )
            .map_err(|error| ApiError::bad_request(error.to_string()))?,
    ))
}
async fn approve_workflow_artifact(
    State(s): State<AppState>,
    Extension(user):Extension<AuthUser>,
    Extension(role):Extension<String>,
    Path((id, artifact_type)): Path<(String, String)>,
    Json(req): Json<WorkflowArtifactApprovalInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_roles(&role,&["owner","pi","approver","research_administrator"])?;
    Ok(Json(
        s.store
            .approve_workflow_artifact(&id, &artifact_type, req.version, Some(&user.id))
            .map_err(ApiError::conflict_err)?,
    ))
}

async fn return_workflow_artifact_for_revision(
    State(s):State<AppState>,
    Extension(user):Extension<AuthUser>,
    Extension(role):Extension<String>,
    Path((id,artifact_type)):Path<(String,String)>,
    Json(req):Json<ReturnForRevisionInput>,
)->Result<Json<serde_json::Value>,ApiError>{
    require_roles(&role,&["owner","pi","contributor","reviewer","approver","research_administrator"])?;
    Ok(Json(s.store.return_workflow_artifact_for_revision(
        &id,&artifact_type,req.version,&user.id,&req.rationale,
    ).map_err(ApiError::conflict_err)?))
}

async fn generate_workflow_artifact(
    State(s): State<AppState>,
    Extension(user):Extension<AuthUser>,
    Path((id, artifact_type)): Path<(String, String)>,
    Json(req): Json<WorkflowArtifactGenerateInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let actor = user.id.as_str();
    let (input_type, prompt_contract, task_kind) = match artifact_type.as_str() {
        "research_framework" => (
            "solicitation_profile",
            r#"Return STRICT JSON only:
{"schema_version":1,"solicitation_profile_version":1,"overall_argument":"...","nodes":[{"key":"stable_section_key","title":"...","position":1,"requirement_ids":["R-001"],"review_criterion_ids":["R-010"],"narrative_purpose":"...","key_argument":"...","linked_aim_ids":[],"evidence_needs":["stable evidence need"],"missing_investigator_inputs":[],"owner_user_id":"","approver_user_id":"","target_words":1000,"dependencies":[]}]}
Map every mandatory requirement and every review criterion to at least one node. Dependencies must reference node keys. Do not invent requirement or criterion IDs. Produce a coherent, sponsor-responsive argument and realistic word allocations."#,
            "framework_generation",
        ),
        "aim_set" => (
            "research_framework",
            r#"Return STRICT JSON only:
{"schema_version":1,"framework_version":1,"overall_objective":"...","central_hypothesis_or_thesis":"...","aims":[{"id":"aim_1","title":"...","statement":"...","rationale":"...","approach_summary":"...","expected_outcome":"...","impact":"...","innovation":"...","classification":"fact|estimate|assumption","dependencies":[],"supporting_evidence_ids":[]}]}
Capture the investigator's actual claims and unresolved uncertainty. Do not invent supporting evidence IDs. Every dependency must reference an aim ID in the same response."#,
            "aims_generation",
        ),
        _ => {
            return Err(ApiError::bad_request(
                "generation is supported only for research_framework and aim_set; other workflow artifacts use their typed pipelines",
            ))
        }
    };
    let input = s.store.workflow_artifact_json(&id, input_type)?;
    if !input
        .get("approved")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Err(ApiError::conflict(format!(
            "workflow gate: approved {input_type} is required"
        )));
    }
    let input_version = input
        .get("version")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| ApiError::conflict("approved input artifact has no version"))?;
    let project = s.store.project_json(&id)?;
    let interview = if s
        .store
        .workflow_module_enabled(&id, "investigator_interview")?
    {
        s.store.interview_context(&id)?
    } else {
        "Investigator interview module is not enabled.".into()
    };
    let sections = s.store.project_sections_json(&id)?;
    let prompt = format!(
        "Generate the next versioned grant workflow artifact. The upstream artifact is human-approved and authoritative. Never override it or create unsupported facts.\n\n{prompt_contract}\n\nPROJECT:\n{}\n\nAPPROVED {input_type}:\n{}\n\nCONFIGURED SECTIONS:\n{}\n\nATTRIBUTED INVESTIGATOR INPUT:\n{}",
        serde_json::to_string_pretty(&project)?,
        serde_json::to_string_pretty(input.get("body").unwrap_or(&serde_json::Value::Null))?,
        serde_json::to_string_pretty(&sections)?,
        interview
    );
    let model_task=match artifact_type.as_str(){
        "research_framework"=>ModelTask::structured::<workflow_artifacts::ResearchFramework>(task_kind,prompt,req.high_value.unwrap_or(true),"research_framework",1)?,
        "aim_set"=>ModelTask::structured::<workflow_artifacts::AimSet>(task_kind,prompt,req.high_value.unwrap_or(true),"aim_set",1)?,
        _=>unreachable!(),
    };
    let generated = s
        .router
        .generate_for_project(
            s.store.as_ref(),
            &id,
            model_task,
        )
        .await?;
    let mut body: serde_json::Value = parse_json_from_model(&generated.text)?;
    let object = body
        .as_object_mut()
        .ok_or_else(|| ApiError::bad_gateway("generated workflow artifact was not an object"))?;
    match artifact_type.as_str() {
        "research_framework" => {
            object.insert("solicitation_profile_version".into(), input_version.into());
            if let Some(nodes) = object.get_mut("nodes").and_then(serde_json::Value::as_array_mut) {
                for node in nodes {
                    if let Some(node) = node.as_object_mut() {
                        node.insert("owner_user_id".into(), actor.into());
                        node.insert("approver_user_id".into(), actor.into());
                    }
                }
            }
        }
        "aim_set" => {
            object.insert("framework_version".into(), input_version.into());
        }
        _ => unreachable!(),
    }
    let saved = s.store.save_workflow_artifact(
        &id,
        &artifact_type,
        &body,
        &format!("model:{}", generated.model),
        Some(actor),
        None,
    )?;
    Ok(Json(serde_json::json!({
        "artifact":saved,
        "model":generated.model,
        "upstream_artifact_type":input_type,
        "upstream_version":input_version
    })))
}
async fn get_project(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    maybe_auto_refresh_competitive(&s, &id).await?;
    let mut project = s.store.project_json(&id)?;
    if s.store.workflow_module_enabled(&id,"competitive_intelligence")?{if let Some(o) = project.as_object_mut() {
        o.insert(
            "competitive_updates".into(),
            s.store.competitive_updates_json(&id, 10)?,
        );
    }}
    Ok(Json(project))
}
async fn get_readiness(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    maybe_auto_refresh_competitive(&s, &id).await?;
    Ok(Json(s.store.readiness_json(&id)?))
}
async fn get_design_profile(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(s.store.design_profile_json(&id)?))
}
async fn save_design_profile(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<DesignProfileInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !req.profile.is_object() {
        return Err(ApiError::bad_request(
            "design profile must be a JSON object",
        ));
    }
    Ok(Json(s.store.save_design_profile(&id, &req.profile)?))
}
async fn project_sections(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(s.store.project_sections_json(&id)?))
}

async fn get_clinical_study(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"clinical_design")?;
    Ok(Json(s.store.clinical_study_json(&id)?))
}
async fn save_clinical_study(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(study): Json<ClinicalStudy>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"clinical_design")?;
    require_core_step_complete(&s.store, &id, "aims")?;
    let saved = s
        .store
        .save_clinical_study(&id, &study)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let assessment = s.store.clinical_assessment_json(&id)?;
    Ok(Json(
        serde_json::json!({"saved":saved,"assessment":assessment}),
    ))
}
async fn get_clinical_assessment(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"clinical_design")?;
    Ok(Json(s.store.clinical_assessment_json(&id)?))
}
async fn calculate_sample_size(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(plan): Json<StatisticsPlan>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"clinical_design")?;
    // Project lookup makes this endpoint project-scoped and prevents detached calculations from being mistaken for grant state.
    let _ = s.store.project_json(&id)?;
    Ok(Json(
        clinical::sample_size(&plan).map_err(|e| ApiError::bad_request(e.to_string()))?,
    ))
}
async fn run_clinical_scenarios(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ScenarioSweepInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"clinical_design")?;
    let study = s.store.clinical_study_typed(&id)?.ok_or_else(|| {
        ApiError::conflict("save the clinical study before running feasibility scenarios")
    })?;
    let max = std::env::var("CLINICAL_SCENARIO_MAX_COMBINATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000usize)
        .clamp(1, 1_000_000);
    Ok(Json(
        clinical::scenario_sweep(&study, &input, max)
            .map_err(|e| ApiError::bad_request(e.to_string()))?,
    ))
}

fn competitive_lock(s: &AppState, project: &str) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = s.competitive_locks.lock();
    locks
        .entry(project.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

fn competitive_profile_context(store: &Store, project: &str) -> Result<String, ApiError> {
    let project_meta = serde_json::to_string_pretty(&store.project_json(project)?)?;
    let requirements = store.requirements_context(project)?;
    let interview = store.interview_context(project)?;
    let evidence = store.evidence_context(project, 32_000)?;
    let clinical = store.clinical_context(project)?;
    let documents = store.document_context(project, 48_000)?;
    Ok(format!("PROJECT METADATA:\n{project_meta}\n\nAPPROVED REQUIREMENTS:\n{requirements}\n\nINVESTIGATOR INTERVIEW:\n{interview}\n\nAUTHORITATIVE CLINICAL DESIGN:\n{clinical}\n\nCURRENT EVIDENCE:\n{evidence}\n\nSOURCE MATERIAL EXCERPTS:\n{documents}"))
}

async fn get_competitive_profile(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"competitive_intelligence")?;
    Ok(Json(s.store.competitive_profile_json(&id)?))
}

async fn generate_competitive_profile(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"competitive_intelligence")?;
    require_interview_complete(&s.store, &id)?;
    require_core_step_complete(&s.store,&id,"literature")?;
    let lock = competitive_lock(&s, &id);
    let _guard = lock.lock().await;
    let engine =
        CompetitiveEngine::from_env(s.research.clone(), s.embedding.clone(), s.router.clone())
            .map_err(|e| ApiError::bad_gateway(format!("competitive engine reload failed: {e}")))?;
    let input_fingerprint = s.store.competitive_input_fingerprint(&id)?;
    let context = competitive_profile_context(&s.store, &id)?;
    let (profile, model) = engine.generate_profile(s.store.as_ref(), &id, &context).await.map_err(|e| {
        ApiError::bad_gateway(format!("competitive profile generation failed: {e}"))
    })?;
    // Refuse to save a profile against an input state that changed during model generation.
    if s.store.competitive_input_fingerprint(&id)? != input_fingerprint {
        return Err(ApiError::conflict("project knowledge changed while the competitive profile was being generated; retry against the current clinical/grant state"));
    }
    Ok(Json(s.store.save_competitive_profile(
        &id,
        &profile,
        &input_fingerprint,
        &model,
    )?))
}

async fn get_competitive_intelligence(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let data = ensure_competitive_fresh(&s, &id, false).await?;
    Ok(Json(data))
}

async fn refresh_competitive_intelligence(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let data = ensure_competitive_fresh(&s, &id, true).await?;
    Ok(Json(data))
}

async fn run_competitive_intelligence(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let data = ensure_competitive_fresh(&s, &id, true).await?;
    Ok(Json(data))
}

async fn get_competitive_updates(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"competitive_intelligence")?;
    maybe_auto_refresh_competitive(&s, &id).await?;
    Ok(Json(s.store.competitive_updates_json(&id, 25)?))
}

async fn maybe_auto_refresh_competitive(s: &AppState, id: &str) -> Result<(), ApiError> {
    if !s.store.workflow_module_enabled(id,"competitive_intelligence")?{return Ok(());}
    if require_core_step_complete(&s.store,id,"literature").is_err()||require_interview_complete(&s.store,id).is_err(){return Ok(());}
    if let Err(e) = ensure_competitive_fresh(s, id, false).await {
        warn!(project_id=%id,error=%e.message,"competitive auto-refresh failed; continuing with stale state and keeping export fail-closed");
    }
    Ok(())
}

fn start_competitive_background_refresh(state: AppState) {
    let enabled = std::env::var("COMPETITIVE_BACKGROUND_REFRESH_ENABLED")
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(true);
    if !enabled {
        info!("competitive background refresh disabled");
        return;
    }
    let interval_seconds = std::env::var("COMPETITIVE_BACKGROUND_REFRESH_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(14_400)
        .clamp(300, 86_400);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(interval_seconds)).await;
            let projects = match state.store.list_projects_json(false) {
                Ok(v) => v,
                Err(e) => {
                    warn!(error=%e,"competitive background refresh could not list projects");
                    continue;
                }
            };
            let Some(rows) = projects.as_array() else {
                continue;
            };
            // Run sequentially by default. Provider-specific rate limits remain authoritative,
            // and a single weak Mac should not fan out multiple long public-intelligence runs.
            for row in rows {
                let Some(id) = row.get("id").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                let stage = row
                    .get("stage")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("intake");
                if !matches!(
                    stage,
                    "science" | "strategy" | "writing" | "review" | "export"
                ) {
                    continue;
                }
                if let Err(e) = maybe_auto_refresh_competitive(&state, id).await {
                    warn!(project_id=%id,error=%e.message,"competitive background refresh will retry later");
                }
            }
        }
    });
}

async fn process_competitive_text_update(
    s: &AppState,
    id: &str,
    engine: &CompetitiveEngine,
) -> Result<serde_json::Value, ApiError> {
    let event = s.store.latest_unprocessed_competitive_update_json(id)?;
    if event.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        return Ok(serde_json::json!({"processed":true,"section_updates":[]}));
    }
    let event_id = event
        .get("event_id")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| ApiError::bad_gateway("competitive update event is missing event_id"))?;
    let delta: competitive_updates::CompetitiveDelta =
        serde_json::from_value(event.get("delta").cloned().unwrap_or_default()).map_err(|e| {
            ApiError::bad_gateway(format!("stored competitive update delta is invalid: {e}"))
        })?;
    let cfg = &engine.config().updates;
    if !cfg.auto_revise_sections {
        s.store.set_competitive_update_processing(
            id,
            event_id,
            "complete",
            &serde_json::json!([]),
        )?;
        return Ok(
            serde_json::json!({"processed":true,"event_id":event_id,"section_updates":[],"auto_revision_disabled":true}),
        );
    }
    let latest = s.store.latest_sections_json(id)?;
    let sections = latest.as_array().cloned().unwrap_or_default();
    let changed = delta
        .changed_section_keys
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let revise_all = (cfg.update_all_sections_on_material_change && delta.material)
        || (cfg.update_all_sections_on_strategy_change && delta.broad_strategy_change);
    let mut candidates = sections
        .into_iter()
        .filter(|x| {
            let key = x
                .get("section_key")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            revise_all || changed.contains(key)
        })
        .collect::<Vec<_>>();
    candidates.truncate(cfg.max_sections_per_refresh.max(1));
    if candidates.is_empty() {
        s.store.set_competitive_update_processing(
            id,
            event_id,
            "complete",
            &serde_json::json!([]),
        )?;
        return Ok(serde_json::json!({"processed":true,"event_id":event_id,"section_updates":[]}));
    }
    let current = s.store.competitive_latest_json(id)?;
    let strategy = current
        .get("strategy")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let mut updated = Vec::<serde_json::Value>::new();
    let mut errors = Vec::<serde_json::Value>::new();
    for sec in candidates {
        let key = sec
            .get("section_key")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let title = sec
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Section");
        let base_version = sec
            .get("version")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let body = sec
            .get("body")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if key.is_empty()
            || base_version <= 0
            || body.trim().is_empty()
            || s.store
                .competitive_section_update_exists(event_id, id, key)?
        {
            continue;
        }
        let query=format!("Refresh grant section {title} ({key}) using newly changed public competitive applicant intelligence while preserving authoritative clinical facts and human language.");
        let budget = std::env::var("CONTEXT_MAX_CHARS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(48_000usize)
            .clamp(8_000, 180_000);
        let compiled =
            match context_compiler::compile(&s.store, &s.retrieval, id, &query, budget).await {
                Ok(x) => x,
                Err(e) => {
                    errors.push(serde_json::json!({"section_key":key,"error":e.to_string()}));
                    continue;
                }
            };
        let prompt = format!(
            r#"New public competitive-applicant intelligence has changed since this grant section was last written. Update the EXISTING section only where the new public evidence or positioning strategy materially improves competitive differentiation. Preserve the author's scientific meaning, clinical design, enrollment/statistical values, commitments, citations, and wording wherever no change is needed. Never invent competitor intent or confidential information and never imply a potential competitor is a confirmed applicant. Normally do not name competitors in proposal prose. Return the COMPLETE revised section prose only. If no prose change is justified, return the existing section EXACTLY.

SECTION: {title}

EXISTING TEXT:
{body}

COMPETITIVE CHANGE SUMMARY:
{}

CURRENT COMPETITIVE STRATEGY:
{}

CURRENT AUTHORITATIVE CONTEXT:
{}"#,
            serde_json::to_string_pretty(&delta).unwrap_or_default(),
            serde_json::to_string_pretty(&strategy).unwrap_or_default(),
            compiled.text
        );
        let generated = match s
            .router
            .generate_for_project(
                s.store.as_ref(),
                id,
                ModelTask::text("competitive_section_refresh",prompt,cfg.section_refresh_high_value),
            )
            .await
        {
            Ok(x) => x,
            Err(e) => {
                errors.push(serde_json::json!({"section_key":key,"error":e.to_string()}));
                continue;
            }
        };
        let revised = generated.text.as_str();
        // The investigator may edit while the competitive refresh model is running.
        // Never publish a proposal against a superseded base version; leave the event
        // retryable so the next access self-heals against the newest human/model text.
        let current_state = match s.store.section_state_json(id, key) {
            Ok(x) => x,
            Err(e) => {
                errors.push(serde_json::json!({"section_key":key,"error":e.to_string()}));
                continue;
            }
        };
        let current_version = current_state
            .get("latest")
            .and_then(|x| x.get("version"))
            .and_then(serde_json::Value::as_i64);
        if current_version != Some(base_version) {
            errors.push(serde_json::json!({"section_key":key,"error":"section changed while competitive auto-update was being generated; retry will use the newest version","expected_base_version":base_version,"current_version":current_version}));
            continue;
        }
        if revised == body.trim() {
            if let Err(e) =
                s.store
                    .record_competitive_section_no_change(event_id, id, key, base_version)
            {
                errors.push(serde_json::json!({"section_key":key,"error":e.to_string()}));
            }
            continue;
        }
        let version = match s.store.save_generated_section(
            id,
            key,
            title,
            revised,
            None,
            &format!(
                "agentic_competitive_update:run:{}:{}",
                delta.to_run_id, generated.model
            ),
            &generated.generation_run_id,
            Some(base_version),
            None,
        ) {
            Ok(v) => v,
            Err(e) => {
                errors.push(serde_json::json!({"section_key":key,"error":e.to_string()}));
                continue;
            }
        };
        if let Err(e) =
            s.store
                .record_competitive_section_update(event_id, id, key, base_version, version)
        {
            errors.push(serde_json::json!({"section_key":key,"error":e.to_string()}));
            continue;
        }
        updated.push(serde_json::json!({"section_key":key,"title":title,"base_version":base_version,"proposed_version":version,"model":generated.model}));
    }
    if errors.is_empty() {
        s.store.set_competitive_update_processing(
            id,
            event_id,
            "complete",
            &serde_json::json!([]),
        )?;
    } else {
        s.store.set_competitive_update_processing(
            id,
            event_id,
            "partial",
            &serde_json::Value::Array(errors.clone()),
        )?;
    }
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

async fn ensure_competitive_fresh(
    s: &AppState,
    id: &str,
    force: bool,
) -> Result<serde_json::Value, ApiError> {
    require_module_enabled(&s.store,id,"competitive_intelligence")?;
    require_interview_complete(&s.store, id)?;
    require_core_step_complete(&s.store,id,"literature")?;

    let lock = competitive_lock(s, id);
    let _guard = lock.lock().await;
    let initial = s.store.competitive_latest_json(id)?;
    let initial_fresh = initial
        .get("fresh")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        && initial.get("status").and_then(serde_json::Value::as_str) == Some("complete");
    if !force && initial_fresh {
        let engine =
            CompetitiveEngine::from_env(s.research.clone(), s.embedding.clone(), s.router.clone())
                .map_err(|e| {
                    ApiError::bad_gateway(format!("competitive engine reload failed: {e}"))
                })?;
        let update = process_competitive_text_update(s, id, &engine).await?;
        let mut out = initial;
        if let Some(o) = out.as_object_mut() {
            o.insert("auto_refreshed".into(), serde_json::Value::Bool(false));
            o.insert("agentic_update".into(), update);
            o.insert(
                "competitive_updates".into(),
                s.store.competitive_updates_json(id, 10)?,
            );
        }
        return Ok(out);
    }

    // Knowledge or enterprise configuration can legitimately change while public APIs
    // are being queried. Retry against the newest state instead of surfacing a stale-
    // intelligence dead end to the user.
    for attempt in 0..3usize {
        let engine =
            CompetitiveEngine::from_env(s.research.clone(), s.embedding.clone(), s.router.clone())
                .map_err(|e| {
                    ApiError::bad_gateway(format!("competitive engine reload failed: {e}"))
                })?;
        let input_fingerprint = s.store.competitive_input_fingerprint(id)?;
        let profile_meta = s.store.competitive_profile_json(id)?;
        let profile_fresh = profile_meta
            .get("exists")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
            && profile_meta
                .get("fresh")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
        if !profile_fresh {
            let context = competitive_profile_context(&s.store, id)?;
            let (profile, model) = engine.generate_profile(s.store.as_ref(), id, &context).await.map_err(|e| {
                ApiError::bad_gateway(format!("competitive profile refresh failed: {e}"))
            })?;
            if s.store.competitive_input_fingerprint(id)? != input_fingerprint {
                continue;
            }
            s.store
                .save_competitive_profile(id, &profile, &input_fingerprint, &model)?;
        }

        let profile_meta = s.store.competitive_profile_json(id)?;
        let profile_version = profile_meta
            .get("version")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| {
                ApiError::bad_gateway("stored competitive profile is missing its version")
            })?;
        let profile = s
            .store
            .competitive_profile_typed(id)?
            .ok_or_else(|| ApiError::conflict("competitive applicant profile is missing"))?;
        let current_input = s.store.competitive_input_fingerprint(id)?;
        let config_sha = engine.config_sha256()?;
        let run_id =
            s.store
                .begin_competitive_run(id, profile_version, &current_input, &config_sha)?;
        let own_context = competitive_profile_context(&s.store, id)?;
        let output = match engine.run(s.store.as_ref(), id, &profile, &own_context).await {
            Ok(x) => x,
            Err(e) => {
                let _ = s.store.fail_competitive_run(run_id, &e.to_string());
                return Err(ApiError::bad_gateway(format!(
                    "competitive intelligence refresh failed: {e}"
                )));
            }
        };
        if s.store.competitive_input_fingerprint(id)? != current_input {
            let _=s.store.fail_competitive_run(run_id,"project knowledge changed during competitive intelligence refresh; retrying automatically");
            continue;
        }
        let mut out = s
            .store
            .finish_competitive_run(id, run_id, &output)?;
        let published_fresh = out
            .get("fresh")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if published_fresh {
            let refresh_reason = if force {
                serde_json::json!(["manual_force"])
            } else {
                initial
                    .get("stale_reasons")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!(["missing_or_stale"]))
            };
            let delta = competitive_updates::diff(
                &initial,
                &out,
                engine.config().updates.candidate_score_delta,
            );
            let event_id = s
                .store
                .record_competitive_update_event(id, &delta, &refresh_reason)?;
            let agentic_update = process_competitive_text_update(s, id, &engine).await?;
            // Refresh after text proposals are created so callers receive current stage/pending-review state.
            out = s.store.competitive_latest_json(id)?;
            if let Some(o) = out.as_object_mut() {
                o.insert("auto_refreshed".into(), serde_json::Value::Bool(!force));
                o.insert("forced_refresh".into(), serde_json::Value::Bool(force));
                o.insert("refresh_attempt".into(), serde_json::json!(attempt + 1));
                o.insert(
                    "previous_run_id".into(),
                    initial
                        .get("run_id")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                );
                o.insert("refresh_reason".into(), refresh_reason);
                o.insert(
                    "competitive_update_event_id".into(),
                    serde_json::json!(event_id),
                );
                o.insert("competitive_delta".into(), serde_json::to_value(&delta)?);
                o.insert("agentic_update".into(), agentic_update);
                o.insert(
                    "competitive_updates".into(),
                    s.store.competitive_updates_json(id, 10)?,
                );
            }
            return Ok(out);
        }
        // Most commonly means competitive config changed mid-run. Loop with a newly
        // loaded engine/config instead of returning stale data.
    }
    Err(ApiError::conflict("competitive inputs or configuration changed repeatedly during refresh; automatic retries were exhausted. Retry the operation once changes settle."))
}

async fn persist_document(
    s: &AppState,
    id: &str,
    name: &str,
    kind: &str,
    text: &str,
) -> Result<serde_json::Value, ApiError> {
    if text.trim().is_empty() {
        return Err(ApiError::bad_request("document contains no readable text"));
    }
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    let sha = hex::encode(h.finalize());
    let (document_id, added) = s.store.add_document(id, name, kind, text, &sha)?;
    let target = std::env::var("DOCUMENT_CHUNK_WORDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(420usize);
    let overlap = std::env::var("DOCUMENT_CHUNK_OVERLAP_WORDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64usize);
    let chunks = chunker::chunk_text(text, target, overlap);
    s.store.replace_document_chunks(id, document_id, &chunks)?;
    Ok(
        serde_json::json!({"ok":true,"added":added,"document_id":document_id,"chunks":chunks.len(),"sha256":sha}),
    )
}

async fn get_compliance_profile(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"sponsor_compliance")?;
    Ok(Json(s.store.compliance_profile_json(&id)?))
}
async fn compile_compliance_profile(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"sponsor_compliance")?;
    let documents = s.store.opportunity_documents(&id)?;
    if documents.is_empty() {
        return Err(ApiError::conflict("upload, fetch, or paste a funding opportunity before compiling sponsor submission rules"));
    }
    let mut remaining = 180_000usize;
    let mut source_packet = String::new();
    for doc in &documents {
        if remaining == 0 {
            break;
        }
        let text = doc.text.chars().take(remaining).collect::<String>();
        remaining = remaining.saturating_sub(text.chars().count());
        source_packet.push_str(&format!(
            "\n--- SOURCE DOCUMENT ID: {} | KIND: {} | NAME: {} ---\n{}\n",
            doc.id, doc.kind, doc.name, text
        ));
    }
    let project = s.store.project_json(&id)?;
    let prompt = format!(
        r#"Compile the funding opportunity into deterministic sponsor/submission rules. Return STRICT JSON only with this exact shape:
{{"profile":{{"sponsor":null,"mechanism":null,"submission_system":null,"deadline_iso":null,"rules":[{{"rule_id":"C-001","category":"format|section|attachment|deadline|budget|eligibility|submission|administrative","rule_type":"required_section|required_form|max_words|min_words|required_attachment|allowed_extensions|min_font_size_pt|min_margin_in|max_pages|deadline|required_letter_count|manual_requirement|submission_system|max_budget|project_period_max_months","scope":"proposal|section|artifact|project","target":"specific target such as Specific Aims or letters_of_support","severity":"hard|warning|info","mandatory":true,"numeric_value":null,"text_value":null,"list_value":[],"source_hint":"short semantic description used to locate the source passage","source_document_hint":"document ID or name if known, otherwise null","source_page_hint":null,"notes":"brief normalization explanation"}}]}}}}
Rules:
- Extract only sponsor requirements explicitly supported by the funding-opportunity source. Never invent a rule.
- Split compound instructions into atomic rules.
- Use severity=hard for explicit must/shall/required/limit/deadline rules whose violation can make the application noncompliant; warning for recommendations; info for metadata.
- Normalize dates to YYYY-MM-DD only when the date is explicit and unambiguous; otherwise create a manual_requirement preserving the source wording.
- Normalize numeric limits into numeric_value. For file extensions, put lowercase extensions without dots in list_value.
- Use required_section for explicitly required narrative sections; required_attachment for explicitly required package attachments; max_pages/max_words/min_font_size_pt/min_margin_in where explicit.
- Use required_form, never required_section, for structured portal forms such as SF424, budgets, Senior/Key Person Profiles, performance sites, and other Grants.gov form components. Structured forms must not become AI-drafted narrative sections.
- Rules that cannot be deterministically proven from the current application model must still be preserved as manual_requirement rather than dropped.
- Every rule MUST include a concise, non-empty source_hint that identifies where deterministic code should search.
- NEVER return source_excerpt, a quotation, source_locator, source offsets, source_document_id, source_page, or source_status. The application—not the model—locates and copies exact source characters.
- source_document_hint and source_page_hint are approximate hints only; source_document_hint must be a JSON string (for example "42" or a document name), and both must be null when uncertain.

PROJECT METADATA:
{}

FUNDING OPPORTUNITY SOURCE DOCUMENTS:
{}"#,
        serde_json::to_string_pretty(&project).unwrap_or_default(),
        source_packet
    );
    let out = s
        .router
        .generate_for_project(
            s.store.as_ref(),
            &id,
            ModelTask::structured::<ComplianceDraftEnvelope>(
                "sponsor_compliance_compilation",prompt,false,"compliance_profile_draft",1,
            )?,
        )
        .await?;
    let profile = source_locator::compile_model_output(&out.text, &documents)
        .map_err(|e| ApiError::bad_gateway(format!("invalid compiled compliance profile: {e}")))?;
    let saved = s.store.save_compliance_profile(&id, &profile, &out.model)?;
    s.store
        .save_analysis(&id, "sponsor_compliance_raw", &out.text)?;
    Ok(Json(saved))
}
async fn save_compliance_profile(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ComplianceProfileInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"sponsor_compliance")?;
    let documents = s.store.opportunity_documents(&id)?;
    let profile = source_locator::locate_profile(req.profile, &documents);
    crate::compliance::validate_profile(&profile)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    source_locator::validate_exact_sources(&profile, &documents)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(s.store.save_compliance_profile(
        &id,
        &profile,
        "human_reviewed_rules",
    )?))
}
async fn approve_compliance_profile(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"sponsor_compliance")?;
    Ok(Json(s.store.approve_compliance_profile(&id)?))
}
async fn resolve_compliance_rule(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ComplianceResolutionInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"sponsor_compliance")?;
    Ok(Json(s.store.resolve_compliance_rule(
        &id,
        &req.rule_id,
        &req.status,
        req.notes.as_deref().unwrap_or(""),
        req.resolved_by.as_deref(),
    )?))
}
async fn save_compliance_measurements(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ComplianceMeasurementsInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"sponsor_compliance")?;
    Ok(Json(
        s.store
            .save_compliance_measurements(&id, &req.measurements)?,
    ))
}
async fn get_compliance_assessment(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"sponsor_compliance")?;
    Ok(Json(s.store.compliance_assessment_json(&id)?))
}
async fn register_submission_artifact(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SubmissionArtifactInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"sponsor_compliance")?;
    Ok(Json(s.store.register_submission_artifact(
        &id,
        &req.slot,
        &req.filename,
        &req.path,
        &req.sha256,
        &req.extension,
    )?))
}
async fn get_submission_artifacts(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"sponsor_compliance")?;
    Ok(Json(s.store.submission_artifacts_json(&id)?))
}

async fn add_document(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<DocumentInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        persist_document(&s, &id, &req.name, &req.kind, &req.text).await?,
    ))
}
async fn get_opportunity_source(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let text = s.store.opportunity_context(&id, 300_000)?;
    let fingerprint = s.store.opportunity_source_fingerprint(&id)?;
    let documents = s
        .store
        .opportunity_documents(&id)?
        .into_iter()
        .map(|d| serde_json::json!({"id":d.id,"name":d.name,"kind":d.kind,"text":d.text,"sha256":d.sha256}))
        .collect::<Vec<_>>();
    Ok(Json(
        serde_json::json!({"text":text,"documents":documents,"fingerprint":fingerprint}),
    ))
}

async fn fetch_url_document(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<FetchUrlInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let src = s
        .research
        .fetch_rendered(&req.url, req.name.as_deref())
        .await
        .map_err(|e| ApiError::bad_request(format!("browser URL ingestion failed: {e}")))?;
    let name = req.name.unwrap_or_else(|| src.title.clone());
    let kind = req.kind.unwrap_or_else(|| "funding_url".into());
    Ok(Json(
        persist_document(&s, &id, &name, &kind, &src.text).await?,
    ))
}

async fn analyze_requirements(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if s.store.document_count(&id)? == 0 {
        return Err(ApiError::conflict("workflow gate: ingest a funding opportunity or supporting document before requirement analysis"));
    }
    let ctx = s.store.document_context(&id, 140_000)?;
    let prompt = format!(
        r#"Analyze the supplied funding opportunity and supporting project materials. Return STRICT JSON only using this shape:
{{"requirements":[{{"external_id":"R-001","category":"eligibility|compliance|scientific|clinical|administrative|document|budget|deadline|format|evidence|review_criterion","requirement":"atomic requirement","mandatory":true,"evidence_needed":["item"],"dependencies":["R-000"],"source_clue":"short source wording or rationale","source_document":null,"source_locator":null}}]}}
Rules: each requirement must be atomic; preserve every explicit eligibility, deadline, budget, page/word, attachment, scientific, clinical, compliance, evidence, and review criterion; never invent a requirement; use stable unique IDs; dependencies must reference IDs in the same output.

SOURCE MATERIAL:
{ctx}"#
    );
    let out = s
        .router
        .generate_for_project(
            s.store.as_ref(),
            &id,
            ModelTask::structured::<RequirementsEnvelope>(
                "requirement_decomposition",prompt,false,"requirements_envelope",1,
            )?,
        )
        .await?;
    let parsed: RequirementsEnvelope = parse_json_from_model(&out.text)?;
    if parsed.requirements.is_empty() {
        return Err(ApiError::bad_gateway(
            "requirement extraction returned zero requirements",
        ));
    }
    s.store.replace_requirements(&id, &parsed.requirements)?;
    s.store.save_analysis(&id, "requirements_raw", &out.text)?;
    let project = s.store.project_json(&id)?;
    let documents = s.store.opportunity_documents(&id)?;
    let mut facts = Vec::new();
    let mut eligibility = Vec::new();
    let mut deadlines = Vec::new();
    let mut budget_rules = Vec::new();
    let mut attachments = Vec::new();
    let mut review_criteria = Vec::new();
    let mut open_questions = Vec::new();
    for requirement in &parsed.requirements {
        let located = source_locator::locate_statement(
            &requirement.requirement,
            (!requirement.source_clue.trim().is_empty()).then_some(requirement.source_clue.as_str()),
            &documents,
        );
        let sources = located
            .as_ref()
            .map(|source| {
                vec![serde_json::json!({
                    "document_id":source.document_id,
                    "document_sha256":source.document_sha256,
                    "locator":source.locator,
                    "start_offset":source.start_offset,
                    "end_offset":source.end_offset,
                    "excerpt":source.excerpt
                })]
            })
            .unwrap_or_default();
        let status = if located.is_some() {
            "deterministically_located"
        } else {
            open_questions.push(format!(
                "Locate exact source provenance for {}: {}",
                requirement.external_id, requirement.requirement
            ));
            "model_extracted"
        };
        let fact = serde_json::json!({
            "id":requirement.external_id,
            "label":requirement.requirement,
            "value":requirement.requirement,
            "mandatory":requirement.mandatory,
            "status":status,
            "sources":sources.clone()
        });
        facts.push(fact.clone());
        match requirement.category.as_str() {
            "eligibility" => eligibility.push(fact.clone()),
            "deadline" => deadlines.push(fact.clone()),
            "budget" => budget_rules.push(fact.clone()),
            "document" => attachments.push(fact.clone()),
            "review_criterion" => review_criteria.push(serde_json::json!({
                "id":requirement.external_id,
                "title":requirement.requirement,
                "description":requirement.requirement,
                "scored":false,
                "scale":null,
                "status":status,
                "sources":sources.clone()
            })),
            _ => {}
        }
    }
    if review_criteria.is_empty() {
        open_questions.push("No explicit review criteria were located. Add the solicitation rubric with exact source provenance before approval.".into());
    }
    let purpose = parsed
        .requirements
        .iter()
        .find(|item| item.category == "scientific")
        .map(|item| item.requirement.clone())
        .unwrap_or_else(|| {
            project
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Grant application")
                .to_owned()
        });
    let profile = serde_json::json!({
        "schema_version":1,
        "working_title":project.get("title").and_then(serde_json::Value::as_str).unwrap_or(""),
        "sponsor":project.get("sponsor").and_then(serde_json::Value::as_str).unwrap_or(""),
        "mechanism":project.get("mechanism").cloned().unwrap_or(serde_json::Value::Null),
        "purpose":purpose,
        "eligibility":eligibility,
        "requirements":facts,
        "review_criteria":review_criteria,
        "deadlines":deadlines,
        "budget_rules":budget_rules,
        "attachments":attachments,
        "open_questions":open_questions
    });
    let profile_state = s.store.save_workflow_artifact(
        &id,
        "solicitation_profile",
        &profile,
        "model_extraction_with_deterministic_source_location",
        None,
        None,
    )?;
    Ok(Json(
        serde_json::json!({"model":out.model,"count":parsed.requirements.len(),"requirements":s.store.requirements_json(&id)?,"solicitation_profile":profile_state}),
    ))
}
async fn get_requirements(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(s.store.requirements_json(&id)?))
}
async fn approve_requirements(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let n = s.store.approve_requirements(&id)?;
    Ok(Json(
        serde_json::json!({"ok":true,"approved":n}),
    ))
}

async fn generate_interview(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"investigator_interview")?;
    if !s.store.requirements_all_approved(&id)? {
        return Err(ApiError::conflict("workflow gate: approve all parsed requirements before generating the investigator interview"));
    }
    require_core_step_complete(&s.store, &id, "framework")?;
    let requirements = s.store.requirements_context(&id)?;
    let docs = s.store.document_context(&id, 70_000)?;
    let answered = s.store.interview_context(&id)?;
    let prompt = format!(
        r#"Create the minimum investigator interview needed to close unresolved information gaps for this grant. Do not ask questions already answered by the documents or prior interview answers. Return STRICT JSON only:
{{"questions":[{{"requirement_id":"R-001","question":"specific question","answer_type":"text|integer|number|percentage|boolean|date|choice","choices":[],"unit":null,"why_needed":"why this requirement cannot yet be satisfied","evidence_requested":true,"priority":100}}]}}
Prefer typed numeric/boolean/date/choice answers over free text. Every question must map to an existing requirement ID. Prioritize mandatory and high-scoring requirements. If no question is needed, return {{"questions":[]}}.

REQUIREMENTS:
{requirements}

SOURCE MATERIAL:
{docs}

PRIOR ANSWERS:
{answered}"#
    );
    let out = s
        .router
        .generate_for_project(
            s.store.as_ref(),
            &id,
            ModelTask::structured::<InterviewEnvelope>(
                "investigator_interview",prompt,false,"interview_envelope",1,
            )?,
        )
        .await?;
    let parsed: InterviewEnvelope = parse_json_from_model(&out.text)?;
    let valid_ids = s.store.requirement_ids(&id)?;
    for q in &parsed.questions {
        if !valid_ids.iter().any(|x| x == &q.requirement_id) {
            return Err(ApiError::bad_gateway(format!(
                "interview model referenced unknown requirement {}",
                q.requirement_id
            )));
        }
        if !matches!(
            q.answer_type.as_str(),
            "text" | "integer" | "number" | "percentage" | "boolean" | "date" | "choice"
        ) {
            return Err(ApiError::bad_gateway(format!(
                "invalid interview answer type {}",
                q.answer_type
            )));
        }
    }
    s.store
        .replace_open_interview_questions(&id, &parsed.questions)?;
    Ok(Json(
        serde_json::json!({"model":out.model,"count":parsed.questions.len(),"questions":s.store.interview_questions_json(&id)?}),
    ))
}
async fn get_interview(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"investigator_interview")?;
    Ok(Json(s.store.interview_questions_json(&id)?))
}
async fn save_answer(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<AnswerInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_module_enabled(&s.store,&id,"investigator_interview")?;
    if !s.store.interview_generated(&id)? {
        return Err(ApiError::conflict(
            "workflow gate: generate the investigator interview before saving answers",
        ));
    }
    if !matches!(req.confidence.as_str(), "high" | "medium" | "low") {
        return Err(ApiError::bad_request("invalid answer confidence"));
    }
    if !matches!(
        req.classification.as_str(),
        "verified_fact" | "investigator_estimate" | "assumption" | "unknown"
    ) {
        return Err(ApiError::bad_request("invalid answer classification"));
    }
    let aid = s.store.save_interview_answer(
        &id,
        req.question_id,
        &req.value,
        &req.confidence,
        &req.classification,
        req.notes.as_deref(),
        req.answered_by.as_deref(),
    )?;
    Ok(Json(
        serde_json::json!({"ok":true,"answer_id":aid,"open_questions":s.store.interview_open_count(&id)?}),
    ))
}

fn require_interview_complete(store: &Store, id: &str) -> Result<(), ApiError> {
    if !store.requirements_all_approved(id)? {
        return Err(ApiError::conflict(
            "workflow gate: requirements are not fully approved",
        ));
    }
    if !store.workflow_module_required(id,"investigator_interview")?{return Ok(());}
    if !store.interview_generated(id)? {
        return Err(ApiError::conflict(
            "workflow gate: investigator interview has not been generated",
        ));
    }
    let open = store.interview_open_count(id)?;
    if open > 0 {
        return Err(ApiError::conflict(format!(
            "workflow gate: {open} investigator interview question(s) remain open"
        )));
    }
    Ok(())
}

fn require_module_enabled(store:&Store,id:&str,module:&str)->Result<(),ApiError>{
    if !store.workflow_module_enabled(id,module)?{return Err(ApiError::not_found(format!("workflow module is not enabled for this project: {module}")));}
    Ok(())
}

fn require_roles(role:&str,allowed:&[&str])->Result<(),ApiError>{
    if allowed.contains(&role){Ok(())}else{Err(ApiError::new(StatusCode::FORBIDDEN,format!("project role '{role}' is not permitted for this action")))}
}

fn require_core_step_complete(store:&Store,id:&str,step_key:&str)->Result<(),ApiError>{
    let status=store.workflow_status_json(id)?;
    let complete=status.get("steps").and_then(serde_json::Value::as_array).is_some_and(|steps|steps.iter().any(|step|
        step.get("key").and_then(serde_json::Value::as_str)==Some(step_key)&&step.get("status").and_then(serde_json::Value::as_str)==Some("complete")));
    if !complete{return Err(ApiError::conflict(format!("workflow gate: core step '{step_key}' is incomplete")));}
    Ok(())
}

fn require_optional_domain_gates(store:&Store,id:&str)->Result<(),ApiError>{
    require_interview_complete(store,id)?;
    if store.workflow_module_required(id,"clinical_design")?{
        let clinical=store.clinical_assessment_json(id)?;
        if !clinical.get("exists").and_then(serde_json::Value::as_bool).unwrap_or(false)||!clinical.get("errors").and_then(serde_json::Value::as_array).is_some_and(|errors|errors.is_empty()){
            return Err(ApiError::conflict("workflow gate: required clinical design is incomplete"));
        }
    }
    require_compliance_profile_approved(store,id)
}

fn require_compliance_profile_approved(store: &Store, id: &str) -> Result<(), ApiError> {
    if !store.workflow_module_required(id,"sponsor_compliance")?{return Ok(());}
    let c = store.compliance_profile_json(id)?;
    if !c
        .get("exists")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Err(ApiError::conflict(
            "workflow gate: compile sponsor submission rules before writing",
        ));
    }
    if !c
        .get("fresh")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Err(ApiError::conflict("workflow gate: sponsor submission rules are stale because the funding opportunity changed; recompile and approve them before writing"));
    }
    if !c
        .get("approved")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Err(ApiError::conflict("workflow gate: human approval of the sponsor compliance profile is required before writing"));
    }
    Ok(())
}

async fn generate_research_plan(
    State(s): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Extension(role): Extension<String>,
    Path(id): Path<String>,
    Json(req): Json<ResearchPlanInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_roles(
        &role,
        &["owner", "pi", "contributor", "research_administrator"],
    )?;
    require_interview_complete(&s.store, &id)?;
    require_core_step_complete(&s.store, &id, "aims")?;
    let requirements = s.store.requirements_context(&id)?;
    let solicitation = s
        .store
        .workflow_artifact_json(&id, "solicitation_profile")?;
    let framework = s
        .store
        .workflow_artifact_json(&id, "research_framework")?;
    let aims = s.store.workflow_artifact_json(&id, "aim_set")?;
    for (name, artifact) in [
        ("solicitation_profile", &solicitation),
        ("research_framework", &framework),
        ("aim_set", &aims),
    ] {
        if !artifact
            .get("approved")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
            || !s.store.workflow_artifact_is_fresh(&id, name)?
        {
            return Err(ApiError::conflict(format!(
                "workflow gate: an approved and fresh {name} is required for literature planning"
            )));
        }
    }
    let solicitation_version = solicitation
        .get("version")
        .and_then(serde_json::Value::as_i64)
        .context("approved solicitation profile version missing")?;
    let framework_version = framework
        .get("version")
        .and_then(serde_json::Value::as_i64)
        .context("approved research framework version missing")?;
    let aim_set_version = aims
        .get("version")
        .and_then(serde_json::Value::as_i64)
        .context("approved aim set version missing")?;
    let aim_ids: BTreeSet<String> = aims
        .get("body")
        .and_then(|body| body.get("aims"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|aim| {
            aim.get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    let criterion_ids: BTreeSet<String> = solicitation
        .get("body")
        .and_then(|body| body.get("review_criteria"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            item.get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    let config = s.store.workflow_config(&id)?;
    let interview = if config.enabled("investigator_interview") {
        s.store.interview_context(&id)?
    } else {
        "Not selected for this project.".into()
    };
    let clinical = if config.enabled("clinical_design")
        && s.store.clinical_study_typed(&id)?.is_some()
    {
        s.store.clinical_context(&id)?
    } else {
        "Not selected for this project.".into()
    };
    let max_queries = req.max_queries.unwrap_or(8).clamp(1, 24);
    let prompt = format!(
        r#"Generate targeted external research queries only for unresolved evidence gaps in this grant. Return STRICT JSON only:
{{"queries":[{{"requirement_id":"R-001","aim_ids":["aim_1"],"criterion_ids":["R-010"],"query":"precise web research query","preferred_domains":["nih.gov"],"rationale":"specific evidence gap"}}]}}
Every query must address the approved solicitation and at least one approved aim. Use only IDs in the supplied artifacts. Use authoritative primary sources where possible. Do not research facts already established by uploaded institutional evidence. Limit output to at most {max_queries} queries.

REQUIREMENTS:
{requirements}

APPROVED SOLICITATION PROFILE:
{}

APPROVED RESEARCH FRAMEWORK:
{}

APPROVED AIM SET:
{}

INVESTIGATOR ANSWERS:
{interview}

CLINICAL STUDY DESIGN / FEASIBILITY CONTEXT:
{clinical}"#,
        serde_json::to_string_pretty(solicitation.get("body").unwrap_or(&serde_json::Value::Null))?,
        serde_json::to_string_pretty(framework.get("body").unwrap_or(&serde_json::Value::Null))?,
        serde_json::to_string_pretty(aims.get("body").unwrap_or(&serde_json::Value::Null))?
    );
    let out = s
        .router
        .generate_for_project(
            s.store.as_ref(),
            &id,
            ModelTask::structured::<ResearchPlanEnvelope>(
                "research_planning",prompt,false,"research_plan_envelope",1,
            )?,
        )
        .await?;
    let mut plan: ResearchPlanEnvelope = parse_json_from_model(&out.text)?;
    plan.queries.truncate(max_queries);
    let valid_ids = s.store.requirement_ids(&id)?;
    let mut failures = Vec::new();
    let mut accepted_queries = Vec::new();
    let mut normalized_queries = BTreeSet::new();
    for query in plan.queries {
        if !valid_ids.iter().any(|id| id == &query.requirement_id) {
            failures.push(format!(
                "ignored research query for unknown requirement {}",
                query.requirement_id
            ));
            continue;
        }
        if query.aim_ids.is_empty()
            || query.aim_ids.iter().any(|aim| !aim_ids.contains(aim))
        {
            failures.push(format!(
                "ignored research query with missing or unknown aim IDs: {}",
                query.query
            ));
            continue;
        }
        if query
            .criterion_ids
            .iter()
            .any(|criterion| !criterion_ids.contains(criterion))
        {
            failures.push(format!(
                "ignored research query with unknown review criterion IDs: {}",
                query.query
            ));
            continue;
        }
        let normalized = query
            .query
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if normalized.is_empty() || !normalized_queries.insert(normalized.clone()) {
            failures.push(format!("ignored empty or duplicate research query: {}", query.query));
            continue;
        }
        let digest = format!("{:x}", Sha256::digest(normalized.as_bytes()));
        accepted_queries.push(LiteratureQueryRecord {
            id: format!("query_{}", &digest[..16]),
            query: query.query.trim().to_string(),
            rationale: query.rationale.trim().to_string(),
            aim_ids: query.aim_ids,
            requirement_ids: vec![query.requirement_id],
            criterion_ids: query.criterion_ids,
            preferred_domains: query.preferred_domains,
        });
    }
    if accepted_queries.is_empty() {
        return Err(ApiError::bad_request(format!(
            "the research planner produced no valid solicitation-and-aim-grounded queries: {}",
            failures.join("; ")
        )));
    }

    let body = serde_json::to_value(LiteratureSearchPlan {
        schema_version: 1,
        solicitation_profile_version: solicitation_version,
        framework_version,
        aim_set_version,
        queries: accepted_queries,
    })?;
    let current = s
        .store
        .workflow_artifact_json(&id, "literature_search_plan")?;
    let expected_version = current
        .get("version")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let artifact = s
        .store
        .save_workflow_artifact(
            &id,
            "literature_search_plan",
            &body,
            "model_research_planner",
            Some(&user.id),
            Some(expected_version),
        )
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    Ok(Json(serde_json::json!({
        "artifact": artifact,
        "model": out.model,
        "provider": out.provider,
        "generation_run_id": out.generation_run_id,
        "planner_warnings": failures
    })))
}

async fn run_research(
    State(s): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Extension(role): Extension<String>,
    Path(id): Path<String>,
    Json(req): Json<ResearchInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_roles(
        &role,
        &["owner", "pi", "contributor", "research_administrator"],
    )?;
    require_interview_complete(&s.store, &id)?;
    require_core_step_complete(&s.store, &id, "aims")?;
    let artifact = s
        .store
        .workflow_artifact_json(&id, "literature_search_plan")?;
    if artifact.get("version").and_then(serde_json::Value::as_i64)
        != Some(req.search_plan_version)
        || !artifact
            .get("approved")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        || !s
            .store
            .workflow_artifact_is_fresh(&id, "literature_search_plan")?
    {
        return Err(ApiError::conflict(
            "workflow gate: select an approved and fresh literature search plan",
        ));
    }
    let plan: LiteratureSearchPlan = serde_json::from_value(
        artifact
            .get("body")
            .cloned()
            .context("approved literature search plan body missing")?,
    )?;
    let results_per = req.results_per_query.unwrap_or(5).clamp(1, 10);
    let started_at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(anyhow::Error::from)?;
    let provider = s.research.provider_name().to_string();
    let run_id = s
        .store
        .begin_research_run(
            &id,
            req.search_plan_version,
            &provider,
            &user.id,
            &started_at,
        )
        .map_err(ApiError::conflict_err)?;
    let mut staged_queries = Vec::with_capacity(plan.queries.len());
    let mut failures = Vec::new();
    let mut validation_models = BTreeSet::new();

    for query in &plan.queries {
        let hits = match s
            .research
            .search(&query.query, &query.preferred_domains, results_per)
            .await
        {
            Ok(hits) => hits,
            Err(error) => {
                failures.push(format!("{}: search failed: {error}", query.id));
                staged_queries.push(StagedResearchQuery {
                    query: query.clone(),
                    terminal_status: "failed".into(),
                    sources: Vec::new(),
                });
                continue;
            }
        };
        let mut fetched_sources = Vec::new();
        let mut fetched_source_keys = BTreeSet::new();
        for fetched in s.research.fetch_many(hits).await {
            match fetched {
                Ok(source) => {
                    if fetched_source_keys.insert((source.url.clone(), source.sha256.clone())) {
                        fetched_sources.push(source);
                    }
                }
                Err(error) => failures.push(format!("{}: source fetch failed: {error}", query.id)),
            }
        }
        if fetched_sources.is_empty() {
            staged_queries.push(StagedResearchQuery {
                query: query.clone(),
                terminal_status: "complete_no_sources".into(),
                sources: Vec::new(),
            });
            continue;
        }
        let source_packet = fetched_sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let excerpt = source.text.chars().take(6000).collect::<String>();
                format!(
                    "\n--- SOURCE {index} ---\nTITLE: {}\nURL: {}\nTEXT:\n{}",
                    source.title, source.url, excerpt
                )
            })
            .collect::<String>();
        let validation_prompt = format!(
            r#"Validate whether each supplied source supports the stated evidence need. Return STRICT JSON only:
{{"validations":[{{"source_index":0,"status":"supported|partially_supported|contradicted|irrelevant","confidence":0.0,"supporting_excerpt":"an exact verbatim excerpt copied from the source text, or empty if none","explanation":"brief reason"}}]}}
The supporting_excerpt MUST be copied exactly from the supplied source. Never manufacture a quote. A source being topically related is not enough; it must actually support or contradict the evidence need. Return one assessment per useful supplied source and do not repeat a source index.

REQUIREMENTS: {}
AIMS: {}
REVIEW CRITERIA: {}
EVIDENCE NEED: {}
RESEARCH QUERY: {}
{}"#,
            query.requirement_ids.join(", "),
            query.aim_ids.join(", "),
            query.criterion_ids.join(", "),
            query.rationale,
            query.query,
            source_packet
        );
        let validation_out = match s
            .router
            .generate_for_project(
                s.store.as_ref(),
                &id,
                ModelTask::structured::<EvidenceValidationEnvelope>(
                    "evidence_validation",
                    validation_prompt,
                    false,
                    "evidence_validation_envelope",
                    1,
                )?,
            )
            .await
        {
            Ok(output) => output,
            Err(error) => {
                failures.push(format!("{}: evidence validation failed: {error}", query.id));
                staged_queries.push(StagedResearchQuery {
                    query: query.clone(),
                    terminal_status: "failed".into(),
                    sources: Vec::new(),
                });
                continue;
            }
        };
        validation_models.insert(format!(
            "{}:{}",
            validation_out.provider, validation_out.model
        ));
        let validations: EvidenceValidationEnvelope =
            match parse_json_from_model(&validation_out.text) {
                Ok(value) => value,
                Err(error) => {
                    failures.push(format!(
                        "{}: evidence validation response was invalid: {error}",
                        query.id
                    ));
                    staged_queries.push(StagedResearchQuery {
                        query: query.clone(),
                        terminal_status: "failed".into(),
                        sources: Vec::new(),
                    });
                    continue;
                }
            };
        let mut staged_sources = Vec::new();
        let mut assessed_indices = BTreeSet::new();
        for validation in validations.validations {
            if validation.source_index >= fetched_sources.len() {
                failures.push(format!(
                    "{}: validator returned unknown source index {}",
                    query.id, validation.source_index
                ));
                continue;
            }
            if !assessed_indices.insert(validation.source_index) {
                failures.push(format!(
                    "{}: validator repeated source index {}",
                    query.id, validation.source_index
                ));
                continue;
            }
            if !matches!(
                validation.status.as_str(),
                "supported" | "partially_supported" | "contradicted" | "irrelevant"
            ) {
                failures.push(format!(
                    "{}: validator returned unsupported status {}",
                    query.id, validation.status
                ));
                continue;
            }
            staged_sources.push(StagedResearchSource {
                source: fetched_sources[validation.source_index].clone(),
                validation_status: validation.status,
                confidence: validation.confidence.clamp(0.0, 1.0),
                supporting_excerpt: validation.supporting_excerpt,
                explanation: validation.explanation,
            });
        }
        let terminal_status = if staged_sources.is_empty() {
            failures.push(format!(
                "{}: no fetched source received a valid evidence assessment",
                query.id
            ));
            "failed"
        } else {
            "complete"
        };
        staged_queries.push(StagedResearchQuery {
            query: query.clone(),
            terminal_status: terminal_status.into(),
            sources: staged_sources,
        });
    }
    let completed_at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(anyhow::Error::from)?;
    let staged_run = StagedResearchRun {
        id: run_id.clone(),
        search_plan_version: req.search_plan_version,
        solicitation_profile_version: plan.solicitation_profile_version,
        framework_version: plan.framework_version,
        aim_set_version: plan.aim_set_version,
        search_provider: provider,
        started_at,
        completed_at: completed_at.clone(),
        queries: staged_queries,
        failures: failures.clone(),
    };
    let manifest_state = match s.store.finalize_research_run_atomic(&id, &staged_run) {
        Ok(state) => state,
        Err(error) => {
            let finalization_failure = format!("atomic research finalization failed: {error}");
            failures.push(finalization_failure.clone());
            let _ = s
                .store
                .fail_research_run(&id, &run_id, &failures, &completed_at);
            return Err(ApiError::conflict(finalization_failure));
        }
    };
    Ok(Json(serde_json::json!({
        "run_id": run_id,
        "search_plan_version": req.search_plan_version,
        "validation_models": validation_models,
        "sources_saved": manifest_state.get("sources_saved"),
        "failures": failures,
        "evidence": s.store.evidence_json(&id)?,
        "literature_manifest": manifest_state
    })))
}
async fn get_evidence(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(s.store.evidence_json(&id)?))
}

async fn draft_section(
    State(s): State<AppState>,
    Extension(user):Extension<AuthUser>,
    Extension(role):Extension<String>,
    Path(id): Path<String>,
    Json(req): Json<DraftSectionInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_roles(&role,&["owner","pi","contributor","research_administrator"])?;
    require_core_step_complete(&s.store,&id,"literature")?;
    require_optional_domain_gates(&s.store,&id)?;
    if s.store.workflow_module_required(&id,"competitive_intelligence")?{let _=ensure_competitive_fresh(&s,&id,false).await?;}
    let section_state=s.store.section_state_json(&id,&req.section_key).map_err(|_|ApiError::bad_request("draft target is not present in the approved research framework"))?;
    let base_version=section_state.pointer("/latest/version").and_then(serde_json::Value::as_i64);
    let config=s.store.workflow_config(&id)?;
    let extra = req.additional_context.unwrap_or_default();
    let retrieval_query = format!(
        "Grant section: {}. Section key: {}. Additional focus: {}",
        req.title, req.section_key, extra
    );
    let budget = std::env::var("CONTEXT_MAX_CHARS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(48_000usize)
        .clamp(8_000, 180_000);
    let compiled =
        context_compiler::compile(&s.store, &s.retrieval, &id, &retrieval_query, budget).await?;
    let competitive_instruction=if config.enabled("competitive_intelligence"){"Use available public competitive-applicant intelligence for defensible differentiation, without implying that a potential competitor is a confirmed applicant or exposing unsupported intent."}else{"No competitive-applicant intelligence is part of this project's configured context."};
    let prompt = format!(
        r#"Draft the grant section named "{}". Use only information supported by the supplied compiled run context. Never fabricate citations, preliminary results, approvals, enrollment numbers, capabilities, clinical claims, or institutional facts. Distinguish verified facts from investigator estimates and assumptions. {} Where a material fact is missing, insert [EVIDENCE NEEDED: concise description]. Preserve source/citation identifiers when they are available so later citation assembly can trace the claim. Return publication-ready prose only, not commentary.

COMPILED CONTEXT:
{}

ADDITIONAL HUMAN CONTEXT:
{}"#,
        req.title,competitive_instruction, compiled.text, extra
    );
    let out = s
        .router
        .generate_for_project(
            s.store.as_ref(),
            &id,
            ModelTask::text("section_draft",prompt,req.high_value.unwrap_or(false)),
        )
        .await?;
    let version = s.store.save_generated_section(
        &id,
        &req.section_key,
        &req.title,
        &out.text,
        None,
        &format!("model:{}:{}",out.provider,out.model),
        &out.generation_run_id,
        base_version,
        Some(&user.id),
    ).map_err(|error|{
        let current=s.store.section_state_json(&id,&req.section_key).ok().and_then(|value|value.pointer("/latest/version").and_then(serde_json::Value::as_i64));
        ApiError::conflict_details(error.to_string(),serde_json::json!({"code":"stale_generated_section","base_version_id":base_version,"current_version_id":current,"generation_run_id":out.generation_run_id}))
    })?;
    Ok(Json(
        serde_json::json!({"provider":out.provider,"model":out.model,"generation_run_id":out.generation_run_id,"text":out.text,"version":version,"approved":false,"retrieval":compiled.retrieved}),
    ))
}

async fn get_section(
    State(s): State<AppState>,
    Path((id, section)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    maybe_auto_refresh_competitive(&s, &id).await?;
    Ok(Json(s.store.section_state_json(&id, &section)?))
}
async fn get_section_versions(
    State(s): State<AppState>,
    Path((id, section)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(s.store.section_versions_json(&id, &section)?))
}
async fn compare_section_versions(
    State(s):State<AppState>,Path((id,section)):Path<(String,String)>,Query(query):Query<SectionCompareQuery>,
)->Result<Json<serde_json::Value>,ApiError>{
    Ok(Json(s.store.section_compare_json(&id,&section,query.from_version,query.to_version).map_err(|error|ApiError::bad_request(error.to_string()))?))
}
async fn preview_section_merge(
    State(s):State<AppState>,Extension(role):Extension<String>,Path((id,section)):Path<(String,String)>,Json(req):Json<SectionMergePreviewInput>,
)->Result<Json<serde_json::Value>,ApiError>{
    require_roles(&role,&["owner","pi","contributor","research_administrator"])?;
    if req.proposed_body.trim().is_empty(){return Err(ApiError::bad_request("proposed section body cannot be empty"));}
    Ok(Json(s.store.section_merge_preview_json(&id,&section,req.base_version_id,req.latest_version_id,&req.proposed_body).map_err(|error|ApiError::conflict(error.to_string()))?))
}
async fn restore_section(
    State(s): State<AppState>,
    Extension(user):Extension<AuthUser>,
    Extension(role):Extension<String>,
    Path((id, section)): Path<(String, String)>,
    Json(req): Json<RestoreSectionInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_roles(&role,&["owner","pi","contributor","research_administrator"])?;
    let _ = s.store.project_json(&id)?;
    let version = s
        .store
        .restore_section_version(
            &id,
            &section,
            req.version_id,
            req.base_version_id,
            Some(&user.id),
        )
        .map_err(|error|{
            let current=s.store.section_state_json(&id,&section).ok().and_then(|value|value.pointer("/latest/version").and_then(serde_json::Value::as_i64));
            ApiError::conflict_details(error.to_string(),serde_json::json!({"code":"stale_section_version","base_version_id":req.base_version_id,"current_version_id":current}))
        })?;
    Ok(Json(
        serde_json::json!({"ok":true,"version":version,"restored_from":req.version_id,"approved":false}),
    ))
}
async fn get_collaboration(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let _ = s.store.project_json(&id)?;
    Ok(Json(s.store.collaboration_json(&id)?))
}
async fn join_collaboration(
    State(s): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Extension(role): Extension<String>,
    Path(id): Path<String>,
    Json(req): Json<ProjectMemberInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_roles(&role, &["owner", "pi", "research_administrator"])?;
    let _ = s.store.project_json(&id)?;
    s.store
        .add_project_member(&id, &req.user_id, &req.role, Some(&user.id))
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(s.store.collaboration_json(&id)?))
}
async fn post_collaboration_message(
    State(s): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Extension(role): Extension<String>,
    Path(id): Path<String>,
    Json(req): Json<CollaborationMessageInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_roles(&role,&["owner","pi","contributor","reviewer","approver","research_administrator"])?;
    let _ = s.store.project_json(&id)?;
    s.store
        .post_channel_message(&id,req.channel_kind.as_deref().unwrap_or("general"),req.subject_key.as_deref(),&user.id,&req.body,req.parent_message_id,&req.mentioned_user_ids)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(s.store.collaboration_json(&id)?))
}
async fn get_collaboration_workspace(State(s):State<AppState>,Extension(user):Extension<AuthUser>,Extension(role):Extension<String>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{
    let collaboration=s.store.collaboration_json(&id)?;
    let can_manage_members=matches!(role.as_str(),"owner"|"pi"|"research_administrator");
    let can_post=role!="viewer";
    let can_create_tasks=matches!(role.as_str(),"owner"|"pi"|"contributor"|"research_administrator");
    let invites=if can_manage_members{s.store.project_invites_json(&id)?}else{serde_json::json!([])};
    Ok(Json(serde_json::json!({
        "members":collaboration.get("members").cloned().unwrap_or_else(||serde_json::json!([])),
        "activity":collaboration.get("activity").cloned().unwrap_or_else(||serde_json::json!([])),
        "tasks":s.store.tasks_json(&id)?,
        "notifications":s.store.notifications_json(&user.id,Some(&id))?,
        "invites":invites,
        "approval_routing":s.store.approval_routing_status_json(&id)?,
        "health":s.store.project_health_json(&id)?,
        "permissions":{"role":role,"can_manage_members":can_manage_members,"can_post":can_post,"can_create_tasks":can_create_tasks}
    })))
}
async fn create_invite(State(s):State<AppState>,Extension(user):Extension<AuthUser>,Extension(role):Extension<String>,Path(id):Path<String>,Json(req):Json<InviteInput>)->Result<Json<serde_json::Value>,ApiError>{
    require_roles(&role,&["owner","pi","research_administrator"])?;
    let days=req.expires_in_days.unwrap_or(7).clamp(1,30);
    let mut invite=s.store.create_project_invite(&id,&req.email,&req.role,&user.id,days).map_err(|e|ApiError::bad_request(e.to_string()))?;
    let token=invite.get("token").and_then(serde_json::Value::as_str).context("created invite token is missing")?.to_owned();
    let project_title=s.store.project_json(&id)?.get("title").and_then(serde_json::Value::as_str).unwrap_or("Grantspace project").to_owned();
    if let Some(email)=s.email.clone(){
        let address=req.email.clone();let invite_role=req.role.clone();
        match tokio::task::spawn_blocking(move||email.send_project_invite(&address,&project_title,&invite_role,&token,days)).await{
            Ok(Ok(()))=>invite["email_sent"]=serde_json::Value::Bool(true),
            Ok(Err(error))=>{warn!(error=%error,"project invitation email delivery failed");invite["email_sent"]=serde_json::Value::Bool(false);invite["delivery_error"]=serde_json::Value::String(error.to_string());},
            Err(error)=>{warn!(error=%error,"project invitation email task failed");invite["email_sent"]=serde_json::Value::Bool(false);invite["delivery_error"]=serde_json::Value::String(error.to_string());}
        }
    }else{invite["email_sent"]=serde_json::Value::Bool(false);invite["delivery_error"]=serde_json::Value::String("SMTP delivery is not configured; deliver the one-time link through an approved secure channel".into());}
    Ok(Json(invite))
}
async fn list_invites(State(s):State<AppState>,Extension(role):Extension<String>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{require_roles(&role,&["owner","pi","research_administrator"])?;Ok(Json(s.store.project_invites_json(&id)?))}
async fn revoke_invite(State(s):State<AppState>,Extension(user):Extension<AuthUser>,Extension(role):Extension<String>,Path((id,invite)):Path<(String,String)>)->Result<Json<serde_json::Value>,ApiError>{require_roles(&role,&["owner","pi","research_administrator"])?;s.store.revoke_project_invite(&id,&invite,&user.id).map_err(|e|ApiError::bad_request(e.to_string()))?;Ok(Json(serde_json::json!({"revoked":true,"invite_id":invite})))}
async fn accept_invite(State(s):State<AppState>,Extension(user):Extension<AuthUser>,Json(req):Json<AcceptInviteInput>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.accept_project_invite(&req.token,&user.id,user.email.as_deref()).map_err(|e|ApiError::bad_request(e.to_string()))?))}
async fn get_channel_messages(State(s):State<AppState>,Path((id,kind)):Path<(String,String)>,Query(query):Query<ChannelQuery>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.channel_messages_json(&id,&kind,query.subject_key.as_deref())?))}
async fn post_channel_message(State(s):State<AppState>,Extension(user):Extension<AuthUser>,Extension(role):Extension<String>,Path((id,kind)):Path<(String,String)>,Query(query):Query<ChannelQuery>,Json(req):Json<CollaborationMessageInput>)->Result<Json<serde_json::Value>,ApiError>{require_roles(&role,&["owner","pi","contributor","reviewer","approver","research_administrator"])?;Ok(Json(s.store.post_channel_message(&id,&kind,query.subject_key.as_deref(),&user.id,&req.body,req.parent_message_id,&req.mentioned_user_ids).map_err(|e|ApiError::bad_request(e.to_string()))?))}
async fn get_comments(State(s):State<AppState>,Path((id,artifact_type,artifact_key)):Path<(String,String,String)>,Query(query):Query<CommentQuery>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.comments_json(&id,&artifact_type,&artifact_key,query.version_id)?))}
async fn post_comment(State(s):State<AppState>,Extension(user):Extension<AuthUser>,Extension(role):Extension<String>,Path((id,artifact_type,artifact_key)):Path<(String,String,String)>,Json(req):Json<CommentInput>)->Result<Json<serde_json::Value>,ApiError>{require_roles(&role,&["owner","pi","contributor","reviewer","approver","research_administrator"])?;Ok(Json(s.store.add_comment(&id,&artifact_type,&artifact_key,req.version_id,req.start_offset,req.end_offset,req.quoted_text.as_deref(),&user.id,&req.body,req.parent_comment_id,&req.mentioned_user_ids).map_err(|e|ApiError::bad_request(e.to_string()))?))}
async fn resolve_comment(State(s):State<AppState>,Extension(user):Extension<AuthUser>,Extension(role):Extension<String>,Path((id,comment_id)):Path<(String,i64)>)->Result<Json<serde_json::Value>,ApiError>{require_roles(&role,&["owner","pi","contributor","reviewer","approver","research_administrator"])?;s.store.resolve_comment(&id,comment_id,&user.id).map_err(|e|ApiError::bad_request(e.to_string()))?;Ok(Json(serde_json::json!({"resolved":true,"comment_id":comment_id})))}
async fn get_tasks(State(s):State<AppState>,Path(id):Path<String>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.tasks_json(&id)?))}
async fn create_task(State(s):State<AppState>,Extension(user):Extension<AuthUser>,Extension(role):Extension<String>,Path(id):Path<String>,Json(req):Json<TaskInput>)->Result<Json<serde_json::Value>,ApiError>{require_roles(&role,&["owner","pi","contributor","research_administrator"])?;Ok(Json(s.store.create_task(&id,&req.title,&req.description,&req.owner_user_id,&req.source,&req.priority,req.due_at.as_deref(),&user.id,&req.dependencies).map_err(|e|ApiError::bad_request(e.to_string()))?))}
async fn update_task_status(State(s):State<AppState>,Extension(user):Extension<AuthUser>,Extension(role):Extension<String>,Path((id,task_id)):Path<(String,String)>,Json(req):Json<TaskStatusInput>)->Result<Json<serde_json::Value>,ApiError>{s.store.update_task_status(&id,&task_id,&req.status,&user.id,&role).map_err(|e|ApiError::bad_request(e.to_string()))?;Ok(Json(serde_json::json!({"task_id":task_id,"status":req.status})))}
async fn get_notifications(State(s):State<AppState>,Extension(user):Extension<AuthUser>)->Result<Json<serde_json::Value>,ApiError>{Ok(Json(s.store.notifications_json(&user.id,None)?))}
async fn read_notification(State(s):State<AppState>,Extension(user):Extension<AuthUser>,Path(notification_id):Path<i64>)->Result<Json<serde_json::Value>,ApiError>{s.store.mark_notification_read(&user.id,notification_id).map_err(|e|ApiError::bad_request(e.to_string()))?;Ok(Json(serde_json::json!({"read":true,"notification_id":notification_id})))}
async fn save_section(
    State(s): State<AppState>,
    Extension(user):Extension<AuthUser>,
    Extension(role):Extension<String>,
    Path((id, section)): Path<(String, String)>,
    Json(req): Json<SectionInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_roles(&role,&["owner","pi","contributor","research_administrator"])?;
    require_core_step_complete(&s.store,&id,"literature")?;
    require_optional_domain_gates(&s.store,&id)?;
    if s.store.workflow_module_required(&id,"competitive_intelligence")?{let _=ensure_competitive_fresh(&s,&id,false).await?;}
    if req.body.trim().is_empty() {
        return Err(ApiError::bad_request("section body cannot be empty"));
    }
    if versioning::contains_conflict_markers(&req.body){
        return Err(ApiError::bad_request("resolve every three-way merge conflict marker before saving the section"));
    }
    let version = s.store.save_section_edit(&id,&section,&req.title,&req.body,req.html.as_deref(),req.base_version_id,&user.id).map_err(|error|{
        let current=s.store.section_state_json(&id,&section).ok().and_then(|value|value.pointer("/latest/version").and_then(serde_json::Value::as_i64));
        ApiError::conflict_details(error.to_string(),serde_json::json!({"code":"stale_section_version","base_version_id":req.base_version_id,"current_version_id":current}))
    })?;
    Ok(Json(
        serde_json::json!({"ok":true,"version":version,"approved":false}),
    ))
}
async fn approve_section(
    State(s): State<AppState>,
    Extension(user):Extension<AuthUser>,
    Extension(role):Extension<String>,
    Path((id, section)): Path<(String, String)>,
    Json(req): Json<ApproveSectionInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_roles(&role,&["owner","pi","approver","research_administrator"])?;
    require_core_step_complete(&s.store,&id,"literature")?;
    require_optional_domain_gates(&s.store,&id)?;
    let current=s.store.section_state_json(&id,&section)?;
    let latest=current.pointer("/latest/version").and_then(serde_json::Value::as_i64);
    if latest!=Some(req.version_id){
        return Err(ApiError::conflict_details("the selected version is no longer the latest section version; compare or reconcile it before approval",serde_json::json!({"code":"stale_section_approval","requested_version_id":req.version_id,"current_version_id":latest})));
    }
    let candidate=s.store.section_version_json(&id,&section,req.version_id)?;
    if candidate.get("body").and_then(serde_json::Value::as_str).is_some_and(versioning::contains_conflict_markers){
        return Err(ApiError::bad_request("a section containing unresolved merge conflict markers cannot be approved"));
    }
    let competitive_enabled=s.store.workflow_module_enabled(&id,"competitive_intelligence")?;
    if s.store.workflow_module_required(&id,"competitive_intelligence")?{let _=ensure_competitive_fresh(&s,&id,false).await?;}
    let pending=if competitive_enabled{s.store.pending_competitive_update_for_section_json(&id,&section)?}else{serde_json::json!({})};
    if competitive_enabled&&!pending.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        let event_id = pending.get("event_id").and_then(serde_json::Value::as_i64);
        let proposed = pending
            .get("proposed_version")
            .and_then(serde_json::Value::as_i64);
        if proposed != Some(req.version_id) && req.competitive_update_event_id != event_id {
            return Err(ApiError::conflict("new public competitor intelligence updated this section; reload the highlighted update and explicitly approve or edit it before approval"));
        }
    }
    let result = s
        .store
        .approve_section_version_by(&id, &section, req.version_id, Some(&user.id))
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let mut response=serde_json::json!({"ok":true,"section":section});
    if let (Some(target),Some(source))=(response.as_object_mut(),result.as_object()){
        target.extend(source.clone());
    }
    response["stage"]=serde_json::json!(s.store.compatibility_stage(&id)?);
    Ok(Json(response))
}

async fn return_section_for_revision(
    State(s):State<AppState>,
    Extension(user):Extension<AuthUser>,
    Extension(role):Extension<String>,
    Path((id,section)):Path<(String,String)>,
    Json(req):Json<ReturnForRevisionInput>,
)->Result<Json<serde_json::Value>,ApiError>{
    require_roles(&role,&["owner","pi","contributor","reviewer","approver","research_administrator"])?;
    Ok(Json(s.store.return_section_for_revision(
        &id,&section,req.version,&user.id,&req.rationale,
    ).map_err(ApiError::conflict_err)?))
}
async fn approved_sections(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    maybe_auto_refresh_competitive(&s, &id).await?;
    Ok(Json(s.store.approved_sections_json(&id)?))
}
async fn approved_document(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    maybe_auto_refresh_competitive(&s, &id).await?;
    let project = s.store.project_json(&id)?;
    let sections = s.store.approved_sections_json(&id)?;
    let section_plan = s.store.project_sections_json(&id)?;
    let design = s.store.design_profile_json(&id)?;
    let readiness = s.store.readiness_json(&id)?;
    let clinical = s.store.clinical_study_json(&id)?;
    let clinical_assessment = s.store.clinical_assessment_json(&id)?;
    let competitive = s.store.competitive_latest_json(&id)?;
    let compliance_profile = s.store.compliance_profile_json(&id)?;
    let compliance_assessment = s.store.compliance_assessment_json(&id)?;
    let submission_artifacts = s.store.submission_artifacts_json(&id)?;
    let total = section_plan.as_array().map(|x| x.len()).unwrap_or(0);
    let required = section_plan
        .as_array()
        .map(|x| {
            x.iter()
                .filter(|s| {
                    s.get("required")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0);
    let approved = sections.as_array().map(|x| x.len()).unwrap_or(0);
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
async fn export_snapshot(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    maybe_auto_refresh_competitive(&s, &id).await?;
    let readiness = s.store.readiness_json(&id)?;
    if !readiness
        .get("ready")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Err(ApiError::conflict(format!(
            "workflow gate: project is not ready for export: {}",
            serde_json::to_string(&readiness)?
        )));
    }
    Ok(Json(s.store.create_export_snapshot(&id)?))
}

async fn rebuild_index(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    maybe_auto_refresh_competitive(&s, &id).await?;
    if !s.store.requirements_all_approved(&id)? {
        return Err(ApiError::conflict(
            "workflow gate: approve requirements before building the production knowledge index",
        ));
    }
    Ok(Json(serde_json::to_value(s.retrieval.rebuild(&id).await?)?))
}
async fn index_status(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    maybe_auto_refresh_competitive(&s, &id).await?;
    Ok(Json(s.retrieval.status(&id)?))
}
async fn retrieve_context(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RetrieveInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    maybe_auto_refresh_competitive(&s, &id).await?;
    if !s.store.requirements_all_approved(&id)? {
        return Err(ApiError::conflict(
            "workflow gate: approve requirements before retrieval",
        ));
    }
    let hits = s
        .retrieval
        .search(&id, &req.query, req.k.unwrap_or(20).clamp(1, 100))
        .await?;
    Ok(Json(serde_json::to_value(hits)?))
}

async fn generate(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<GenerateRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let out = s
        .router
        .generate_for_project(
            s.store.as_ref(),
            &id,
            ModelTask::text(req.task,req.prompt,req.high_value.unwrap_or(false)),
        )
        .await?;
    Ok(Json(serde_json::json!({"provider":out.provider,"model":out.model,"routing_mode":out.routing_mode,"generation_run_id":out.generation_run_id,"text":out.text})))
}
async fn hpc_benchmark(State(s): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    let mut result = hpc::self_benchmark();
    let path = s
        .workspace
        .join(format!("hpc_benchmark_{}.bin", Uuid::new_v4()));
    let rows = 10_000usize;
    let cols = 256usize;
    let data = vec![0.01f32; rows * cols];
    let t = Instant::now();
    vector_store::MmapMatrix::create_normalized(&path, rows, cols, &data)?;
    let mmap_create_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let mm = vector_store::MmapMatrix::open(&path)?;
    let mmap_open_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let scores = mm.scores(&vec![0.1; cols])?;
    let mmap_score_ms = t.elapsed().as_secs_f64() * 1000.0;
    result["mmap_rows"] = serde_json::json!(mm.rows);
    result["mmap_dims"] = serde_json::json!(mm.cols);
    result["mmap_create_ms"] = serde_json::json!(mmap_create_ms);
    result["mmap_open_ms"] = serde_json::json!(mmap_open_ms);
    result["mmap_score_ms"] = serde_json::json!(mmap_score_ms);
    result["mmap_score_checksum"] = serde_json::json!(scores.iter().take(32).sum::<f32>());
    drop(mm);
    let _ = std::fs::remove_file(&path);
    Ok(Json(result))
}

async fn system_info(State(s): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    let db = s.workspace.join("grant.db");
    let db_bytes = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
    let envv = |k: &str| std::env::var(k).ok();
    let email_delivery=match s.email.as_ref(){
        Some(settings)=>serde_json::json!({"configured":true,"mode":settings.delivery_mode()}),
        None=>serde_json::json!({"configured":false,"mode":serde_json::Value::Null}),
    };
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
        "email_delivery":email_delivery,
        "workspace":s.workspace.to_string_lossy(),
        "secrets_exposed":false
    })))
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
    details: Option<serde_json::Value>,
}
impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            details: None,
        }
    }
    fn bad_request(m: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, m)
    }
    fn conflict(m: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, m)
    }
    fn conflict_details(m:impl Into<String>,details:serde_json::Value)->Self{
        Self{status:StatusCode::CONFLICT,message:m.into(),details:Some(details)}
    }
    fn not_found(m: impl Into<String>) -> Self { Self::new(StatusCode::NOT_FOUND,m) }
    fn unavailable(m: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, m)
    }
    fn bad_gateway(m: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, m)
    }
    fn conflict_err(e: anyhow::Error) -> Self {
        Self::conflict(e.to_string())
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}
impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let mut body=serde_json::json!({"error":self.message,"status":self.status.as_u16()});
        if let Some(details)=self.details{body["details"]=details;}
        (
            self.status,
            Json(body),
        )
            .into_response()
    }
}

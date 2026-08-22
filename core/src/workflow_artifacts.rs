use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const SYNTHETIC_REVIEW_NOTICE: &str = "Synthetic reviewer feedback is decision support derived from the approved solicitation and proposal snapshot. It does not represent named real reviewers, private deliberations, an award probability, or a predicted sponsor decision.";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactStatus {
    ModelExtracted,
    DeterministicallyLocated,
    HumanCorrected,
    HumanApproved,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceAnchor {
    pub document_id: i64,
    pub document_sha256: String,
    pub locator: String,
    pub start_offset: usize,
    pub end_offset: usize,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolicitationFact {
    pub id: String,
    pub label: String,
    pub value: Value,
    #[serde(default)]
    pub mandatory: bool,
    pub status: FactStatus,
    #[serde(default)]
    pub sources: Vec<SourceAnchor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewCriterion {
    pub id: String,
    pub title: String,
    pub description: String,
    pub scored: bool,
    pub scale: Option<String>,
    pub status: FactStatus,
    #[serde(default)]
    pub sources: Vec<SourceAnchor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolicitationProfile {
    pub schema_version: u32,
    pub working_title: String,
    pub sponsor: String,
    pub mechanism: Option<String>,
    pub purpose: String,
    #[serde(default)]
    pub eligibility: Vec<SolicitationFact>,
    #[serde(default)]
    pub requirements: Vec<SolicitationFact>,
    #[serde(default)]
    pub review_criteria: Vec<ReviewCriterion>,
    #[serde(default)]
    pub deadlines: Vec<SolicitationFact>,
    #[serde(default)]
    pub budget_rules: Vec<SolicitationFact>,
    #[serde(default)]
    pub attachments: Vec<SolicitationFact>,
    #[serde(default)]
    pub open_questions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameworkNode {
    pub key: String,
    pub title: String,
    pub position: u32,
    #[serde(default)]
    pub requirement_ids: Vec<String>,
    #[serde(default)]
    pub review_criterion_ids: Vec<String>,
    pub narrative_purpose: String,
    pub key_argument: String,
    #[serde(default)]
    pub linked_aim_ids: Vec<String>,
    #[serde(default)]
    pub evidence_needs: Vec<String>,
    #[serde(default)]
    pub missing_investigator_inputs: Vec<String>,
    pub owner_user_id: String,
    pub approver_user_id: String,
    pub target_words: u32,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchFramework {
    pub schema_version: u32,
    pub solicitation_profile_version: i64,
    pub overall_argument: String,
    pub nodes: Vec<FrameworkNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertionClassification {
    Fact,
    Estimate,
    Assumption,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchAim {
    pub id: String,
    pub title: String,
    pub statement: String,
    pub rationale: String,
    pub approach_summary: String,
    pub expected_outcome: String,
    pub impact: String,
    pub innovation: String,
    pub classification: AssertionClassification,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub supporting_evidence_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AimSet {
    pub schema_version: u32,
    pub framework_version: i64,
    pub overall_objective: String,
    pub central_hypothesis_or_thesis: String,
    pub aims: Vec<ResearchAim>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiteratureQueryRecord {
    pub id: String,
    pub query: String,
    pub rationale: String,
    #[serde(default)]
    pub aim_ids: Vec<String>,
    #[serde(default)]
    pub requirement_ids: Vec<String>,
    #[serde(default)]
    pub criterion_ids: Vec<String>,
    #[serde(default)]
    pub preferred_domains: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceNeedDisposition {
    Supported,
    Waived,
    UnresolvedRisk,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceNeedResolution {
    pub evidence_need_id: String,
    pub disposition: EvidenceNeedDisposition,
    #[serde(default)]
    pub evidence_ids: Vec<i64>,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiteratureManifest {
    pub schema_version: u32,
    pub run_id: String,
    pub solicitation_profile_version: i64,
    pub framework_version: i64,
    pub aim_set_version: i64,
    pub started_at: String,
    pub completed_at: String,
    pub search_provider: String,
    pub queries: Vec<LiteratureQueryRecord>,
    pub evidence_needs: Vec<EvidenceNeedResolution>,
    #[serde(default)]
    pub source_ids: Vec<i64>,
    #[serde(default)]
    pub citation_ids: Vec<i64>,
    #[serde(default)]
    pub contradictions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalSectionVersionRef {
    pub section_key: String,
    pub version_id: i64,
    pub content_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalSnapshot {
    pub schema_version: u32,
    pub solicitation_profile_version: i64,
    pub framework_version: i64,
    pub aim_set_version: i64,
    pub literature_manifest_version: i64,
    pub workflow_definition_version: u32,
    pub workflow_definition_sha256: String,
    pub workflow_config_version: i64,
    pub sections: Vec<ProposalSectionVersionRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FitDisposition { Fit, Mismatch, Unknown }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FitDimension { pub key:String, pub disposition:FitDisposition, pub rationale:String, #[serde(default)] pub sources:Vec<SourceAnchor> }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoNoGoDecision { Go, NoGo, Hold }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpportunityFitAssessment { pub schema_version:u32, pub solicitation_profile_version:i64, pub dimensions:Vec<FitDimension>, pub decision:GoNoGoDecision, pub decision_rationale:String, pub decided_by_user_id:String }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstitutionalMemoryEntry { pub id:String, pub kind:String, pub content:String, pub origin:String, pub source_document_id:Option<i64>, pub last_reviewed_at:String, pub approved_by_user_id:String }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstitutionalMemoryLibrary { pub schema_version:u32, pub entries:Vec<InstitutionalMemoryEntry> }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRoute { pub artifact_type:String, pub owner_user_id:String, pub approver_user_ids:Vec<String>, pub minimum_approvals:u32 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborationRouting { pub schema_version:u32, pub project_owner_user_id:String, pub routes:Vec<ApprovalRoute> }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectTask { pub id:String, pub title:String, pub owner_user_id:String, pub due_at:String, pub source:String, #[serde(default)] pub dependencies:Vec<String> }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlan { pub schema_version:u32, pub target_submission_at:String, pub tasks:Vec<ProjectTask> }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewerPanelRole { pub key:String, pub title:String, pub description:String, pub criterion_ids:Vec<String> }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewerPanelPlan { pub schema_version:u32, pub solicitation_profile_version:i64, pub registry_definition_version:u32, pub mode:String, pub roles:Vec<ReviewerPanelRole>, pub synthetic_review_notice:String }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionScore { pub criterion_id:String, pub score:Option<f64>, pub strengths:Vec<String>, pub weaknesses:Vec<String>, pub proposal_anchors:Vec<String>, pub solicitation_anchors:Vec<String>, pub confidence:f64 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatedReviewerResult { pub reviewer_archetype:String, pub criterion_scores:Vec<CriterionScore>, pub overall_assessment:String, pub questions:Vec<String> }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalNode { pub id:String, pub kind:String, pub label:String, pub inferred:bool }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalEdge { pub from:String, pub to:String, pub relationship:String, pub evidence_anchors:Vec<String>, pub inferred:bool }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalGraph { pub nodes:Vec<CausalNode>, pub edges:Vec<CausalEdge> }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalAnalysisResult { pub mode:String, pub graph:CausalGraph, pub assumptions:Vec<Value>, pub threats:Vec<Value>, pub claim_checks:Vec<Value> }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewSimulationResult { pub schema_version:u32, pub snapshot_id:String, pub rubric_version_id:String, pub panel_plan_id:String, pub reviews:Vec<SimulatedReviewerResult>, pub causal_analysis:Option<CausalAnalysisResult>, pub panel_summary:Value, pub revision_tasks:Vec<Value>, pub synthetic_review_notice:String }

pub fn validate_panel_plan(plan:&ReviewerPanelPlan)->Result<()> {
    if plan.schema_version!=1||plan.solicitation_profile_version<=0||plan.registry_definition_version==0{bail!("review panel plan version references are invalid");}
    if !matches!(plan.mode.as_str(),"quick_red_team"|"full_panel"|"consensus_panel"|"consensus_causal"){bail!("unsupported review panel mode");}
    required(&plan.synthetic_review_notice,"synthetic_review_notice")?;
    if plan.roles.is_empty(){bail!("review panel plan requires at least one role");}
    unique_non_empty(plan.roles.iter().map(|role|role.key.as_str()),"reviewer role key")?;
    for role in &plan.roles{required(&role.title,"reviewer role title")?;required(&role.description,"reviewer role description")?;if role.criterion_ids.is_empty(){bail!("reviewer role {} must map to solicitation criteria",role.key);}unique_non_empty(role.criterion_ids.iter().map(String::as_str),"reviewer role criterion id")?;}
    Ok(())
}

pub fn validate_review_result(run:&ReviewSimulationResult,approval:bool)->Result<()>{validate_review_simulation(run.clone(),approval)}

pub fn validate_grounded_review_result(
    run: &ReviewSimulationResult,
    plan: &ReviewerPanelPlan,
    solicitation: &SolicitationProfile,
    proposal_anchor_ids: &BTreeSet<String>,
    evidence_anchor_ids: &BTreeSet<String>,
) -> Result<()> {
    validate_review_simulation(run.clone(), true)?;
    validate_panel_plan(plan)?;

    let expected_roles = plan
        .roles
        .iter()
        .map(|role| (role.key.as_str(), role))
        .collect::<std::collections::BTreeMap<_, _>>();
    let actual_roles = run
        .reviews
        .iter()
        .map(|review| review.reviewer_archetype.as_str())
        .collect::<BTreeSet<_>>();
    let expected_role_keys = expected_roles.keys().copied().collect::<BTreeSet<_>>();
    if actual_roles != expected_role_keys {
        bail!("individual reviews must cover each approved panel role exactly once");
    }

    let criteria = solicitation
        .review_criteria
        .iter()
        .map(|criterion| (criterion.id.as_str(), criterion))
        .collect::<std::collections::BTreeMap<_, _>>();
    for review in &run.reviews {
        let role = expected_roles
            .get(review.reviewer_archetype.as_str())
            .context("review references a role not present in the approved panel plan")?;
        let expected_criteria = role
            .criterion_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let actual_criteria = review
            .criterion_scores
            .iter()
            .map(|score| score.criterion_id.as_str())
            .collect::<BTreeSet<_>>();
        if actual_criteria != expected_criteria {
            bail!(
                "review {} must cover exactly its approved solicitation criteria",
                review.reviewer_archetype
            );
        }
        for score in &review.criterion_scores {
            let criterion = criteria
                .get(score.criterion_id.as_str())
                .with_context(|| format!("unknown solicitation criterion {}", score.criterion_id))?;
            if criterion.scored {
                let value = score.score.with_context(|| {
                    format!("scored criterion {} requires a numeric score", criterion.id)
                })?;
                if !value.is_finite() {
                    bail!("criterion {} score must be finite", criterion.id);
                }
                if let Some((minimum, maximum)) = numeric_scale_bounds(criterion.scale.as_deref()) {
                    if value < minimum || value > maximum {
                        bail!(
                            "criterion {} score {value} is outside its solicitation scale {minimum}-{maximum}",
                            criterion.id
                        );
                    }
                }
            } else if score.score.is_some() {
                bail!(
                    "narrative criterion {} must retain a null score",
                    criterion.id
                );
            }
            if !score
                .solicitation_anchors
                .iter()
                .any(|anchor| anchor == &criterion.id)
            {
                bail!(
                    "criterion {} must cite its own solicitation criterion ID",
                    criterion.id
                );
            }
            for anchor in &score.solicitation_anchors {
                if !criteria.contains_key(anchor.as_str()) {
                    bail!("unknown solicitation anchor {anchor}");
                }
            }
            for anchor in &score.proposal_anchors {
                if !proposal_anchor_ids.contains(anchor) {
                    bail!("unknown or stale proposal anchor {anchor}");
                }
            }
        }
    }

    if let Some(causal) = &run.causal_analysis {
        for edge in &causal.graph.edges {
            for anchor in &edge.evidence_anchors {
                if !proposal_anchor_ids.contains(anchor) && !evidence_anchor_ids.contains(anchor) {
                    bail!("causal edge references unknown evidence anchor {anchor}");
                }
            }
        }
    }
    Ok(())
}

fn numeric_scale_bounds(scale: Option<&str>) -> Option<(f64, f64)> {
    let values = scale?
        .split(|character: char| !(character.is_ascii_digit() || character == '.'))
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<f64>().ok())
        .collect::<Vec<_>>();
    if values.len() < 2 {
        return None;
    }
    let first = values[0];
    let second = values[1];
    Some((first.min(second), first.max(second)))
}

pub fn validate_artifact_document(artifact_type: &str, body: &Value, approval: bool) -> Result<()> {
    if !body.is_object() {
        bail!("workflow artifact body must be a JSON object");
    }
    match artifact_type {
        "solicitation_profile" => validate_solicitation(parse(body, artifact_type)?, approval),
        "research_framework" => validate_framework(parse(body, artifact_type)?, approval),
        "aim_set" => validate_aims(parse(body, artifact_type)?, approval),
        "literature_manifest" => validate_literature(parse(body, artifact_type)?, approval),
        "proposal_snapshot" => validate_proposal_snapshot(parse(body, artifact_type)?, approval),
        "opportunity_fit" => validate_opportunity_fit(parse(body, artifact_type)?, approval),
        "institutional_memory" => validate_institutional_memory(parse(body, artifact_type)?, approval),
        "collaboration_record" => validate_collaboration_routing(parse(body, artifact_type)?, approval),
        "task_plan" => validate_task_plan(parse(body, artifact_type)?, approval),
        "review_simulation" => validate_review_simulation(parse(body, artifact_type)?, approval),
        "investigator_interview" => bail!("investigator interview state is managed by the interview question and answer endpoints"),
        "clinical_design" => bail!("clinical design state is managed by the typed clinical-study endpoint"),
        "competitive_intelligence" => bail!("competitive intelligence state is managed by the competitive run endpoints"),
        "sponsor_compliance" => bail!("sponsor compliance state is managed by the compliance profile and assessment endpoints"),
        _ => bail!("unsupported workflow artifact contract: {artifact_type}"),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(body: &Value, artifact_type: &str) -> Result<T> {
    serde_json::from_value(body.clone())
        .with_context(|| format!("invalid {artifact_type} contract"))
}

fn required(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{field} is required");
    }
    Ok(())
}

fn unique_non_empty<'a>(values: impl IntoIterator<Item = &'a str>, field: &str) -> Result<()> {
    let mut found = BTreeSet::new();
    for value in values {
        required(value, field)?;
        if !found.insert(value) {
            bail!("duplicate {field}: {value}");
        }
    }
    Ok(())
}

fn validate_anchor(anchor: &SourceAnchor) -> Result<()> {
    if anchor.document_id <= 0 {
        bail!("source document_id must be positive");
    }
    required(&anchor.document_sha256, "source document_sha256")?;
    required(&anchor.locator, "source locator")?;
    if anchor.end_offset<=anchor.start_offset{bail!("source end_offset must be greater than start_offset");}
    required(&anchor.excerpt, "source excerpt")
}

fn validate_solicitation(profile: SolicitationProfile, approval: bool) -> Result<()> {
    if profile.schema_version != 1 {
        bail!("unsupported solicitation profile schema version");
    }
    if approval {
        required(&profile.working_title, "working_title")?;
        required(&profile.sponsor, "sponsor")?;
        required(&profile.purpose, "purpose")?;
        if profile.requirements.is_empty() {
            bail!("at least one solicitation requirement is required for approval");
        }
        if profile.review_criteria.is_empty() {
            bail!("at least one review criterion is required for approval");
        }
    }
    unique_non_empty(
        profile.requirements.iter().map(|fact| fact.id.as_str()),
        "solicitation requirement id",
    )?;
    unique_non_empty(
        profile.review_criteria.iter().map(|item| item.id.as_str()),
        "review criterion id",
    )?;
    for fact in profile
        .eligibility
        .iter()
        .chain(&profile.requirements)
        .chain(&profile.deadlines)
        .chain(&profile.budget_rules)
        .chain(&profile.attachments)
    {
        required(&fact.label, "solicitation fact label")?;
        if approval&&!matches!(fact.status,FactStatus::HumanApproved){bail!("approved solicitation fact {} must have human_approved status",fact.id);}
        if approval && fact.sources.is_empty() {
            bail!(
                "approved solicitation fact {} requires source provenance",
                fact.id
            );
        }
        for source in &fact.sources {
            validate_anchor(source)?;
        }
    }
    for criterion in &profile.review_criteria {
        required(&criterion.title, "review criterion title")?;
        required(&criterion.description, "review criterion description")?;
        if approval&&!matches!(criterion.status,FactStatus::HumanApproved){bail!("approved review criterion {} must have human_approved status",criterion.id);}
        if approval && criterion.sources.is_empty() {
            bail!(
                "approved review criterion {} requires source provenance",
                criterion.id
            );
        }
        for source in &criterion.sources {
            validate_anchor(source)?;
        }
    }
    Ok(())
}

fn validate_framework(framework: ResearchFramework, approval: bool) -> Result<()> {
    if framework.schema_version != 1 {
        bail!("unsupported research framework schema version");
    }
    if framework.solicitation_profile_version <= 0 {
        bail!("solicitation_profile_version must be positive");
    }
    if approval {
        required(&framework.overall_argument, "overall_argument")?;
    }
    if framework.nodes.is_empty() {
        bail!("research framework requires at least one node");
    }
    unique_non_empty(
        framework.nodes.iter().map(|node| node.key.as_str()),
        "framework node key",
    )?;
    let keys: BTreeSet<&str> = framework
        .nodes
        .iter()
        .map(|node| node.key.as_str())
        .collect();
    let mut positions = BTreeSet::new();
    for node in &framework.nodes {
        if !positions.insert(node.position) {
            bail!("framework node positions must be unique");
        }
        required(&node.title, "framework node title")?;
        if approval {
            required(&node.narrative_purpose, "framework node narrative_purpose")?;
            required(&node.key_argument, "framework node key_argument")?;
            required(&node.owner_user_id, "framework node owner_user_id")?;
            required(&node.approver_user_id, "framework node approver_user_id")?;
            if node.target_words == 0 {
                bail!(
                    "framework node {} requires a positive target_words value",
                    node.key
                );
            }
        }
        for dependency in &node.dependencies {
            if !keys.contains(dependency.as_str()) {
                bail!(
                    "framework node {} has unknown dependency {dependency}",
                    node.key
                );
            }
        }
    }
    Ok(())
}

fn validate_aims(aim_set: AimSet, approval: bool) -> Result<()> {
    if aim_set.schema_version != 1 {
        bail!("unsupported aim set schema version");
    }
    if aim_set.framework_version <= 0 {
        bail!("framework_version must be positive");
    }
    if aim_set.aims.is_empty() {
        bail!("at least one research aim is required");
    }
    unique_non_empty(
        aim_set.aims.iter().map(|aim| aim.id.as_str()),
        "research aim id",
    )?;
    let ids: BTreeSet<&str> = aim_set.aims.iter().map(|aim| aim.id.as_str()).collect();
    if approval {
        required(&aim_set.overall_objective, "overall_objective")?;
        required(
            &aim_set.central_hypothesis_or_thesis,
            "central_hypothesis_or_thesis",
        )?;
    }
    for aim in &aim_set.aims {
        required(&aim.title, "aim title")?;
        required(&aim.statement, "aim statement")?;
        if approval {
            required(&aim.rationale, "aim rationale")?;
            required(&aim.approach_summary, "aim approach_summary")?;
            required(&aim.expected_outcome, "aim expected_outcome")?;
            required(&aim.impact, "aim impact")?;
            required(&aim.innovation, "aim innovation")?;
        }
        for dependency in &aim.dependencies {
            if !ids.contains(dependency.as_str()) {
                bail!("aim {} has unknown dependency {dependency}", aim.id);
            }
        }
    }
    Ok(())
}

fn validate_literature(manifest: LiteratureManifest, approval: bool) -> Result<()> {
    if manifest.schema_version != 1 {
        bail!("unsupported literature manifest schema version");
    }
    if manifest.solicitation_profile_version <= 0
        || manifest.framework_version <= 0
        || manifest.aim_set_version <= 0
    {
        bail!("literature manifest input versions must be positive");
    }
    required(&manifest.run_id, "literature run_id")?;
    required(&manifest.started_at, "literature started_at")?;
    required(&manifest.completed_at, "literature completed_at")?;
    required(&manifest.search_provider, "literature search_provider")?;
    if manifest.queries.is_empty() {
        bail!("literature manifest requires at least one query");
    }
    unique_non_empty(
        manifest.queries.iter().map(|query| query.id.as_str()),
        "literature query id",
    )?;
    let mut normalized_queries = BTreeSet::new();
    for query in &manifest.queries {
        required(&query.query, "literature query")?;
        required(&query.rationale, "literature query rationale")?;
        if approval && query.aim_ids.is_empty() {
            bail!("approved literature query {} must map to at least one aim", query.id);
        }
        if approval && query.requirement_ids.is_empty() {
            bail!("approved literature query {} must map to at least one solicitation requirement", query.id);
        }
        let normalized = query.query.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        if !normalized_queries.insert(normalized) {
            bail!("literature manifest contains duplicate search queries");
        }
    }
    if approval && manifest.evidence_needs.is_empty() {
        bail!("approved literature manifest requires evidence-need dispositions");
    }
    unique_non_empty(
        manifest
            .evidence_needs
            .iter()
            .map(|need| need.evidence_need_id.as_str()),
        "evidence need id",
    )?;
    let query_ids = manifest
        .queries
        .iter()
        .map(|query| query.id.as_str())
        .collect::<BTreeSet<_>>();
    let resolution_ids = manifest
        .evidence_needs
        .iter()
        .map(|need| need.evidence_need_id.as_str())
        .collect::<BTreeSet<_>>();
    if approval && query_ids != resolution_ids {
        bail!("every approved literature query requires exactly one evidence-need disposition");
    }
    for need in &manifest.evidence_needs {
        required(&need.rationale, "evidence need rationale")?;
        if approval
            && matches!(need.disposition, EvidenceNeedDisposition::Supported)
            && need.evidence_ids.is_empty()
        {
            bail!(
                "supported evidence need {} requires evidence IDs",
                need.evidence_need_id
            );
        }
    }
    Ok(())
}

fn validate_proposal_snapshot(snapshot: ProposalSnapshot, approval: bool) -> Result<()> {
    if snapshot.schema_version != 1 {
        bail!("unsupported proposal snapshot schema version");
    }
    if snapshot.solicitation_profile_version <= 0
        || snapshot.framework_version <= 0
        || snapshot.aim_set_version <= 0
        || snapshot.literature_manifest_version <= 0
        || snapshot.workflow_config_version <= 0
    {
        bail!("proposal snapshot input versions must be positive");
    }
    if snapshot.workflow_definition_version == 0 {
        bail!("workflow_definition_version must be positive");
    }
    required(
        &snapshot.workflow_definition_sha256,
        "workflow_definition_sha256",
    )?;
    if approval && snapshot.sections.is_empty() {
        bail!("proposal snapshot requires approved section versions");
    }
    unique_non_empty(
        snapshot
            .sections
            .iter()
            .map(|section| section.section_key.as_str()),
        "proposal section key",
    )?;
    for section in &snapshot.sections {
        if section.version_id <= 0 {
            bail!("proposal section version_id must be positive");
        }
        required(&section.content_sha256, "proposal section content_sha256")?;
    }
    Ok(())
}

fn validate_opportunity_fit(assessment:OpportunityFitAssessment,approval:bool)->Result<()>{
    if assessment.schema_version!=1{bail!("unsupported opportunity fit schema version");}
    if assessment.solicitation_profile_version<=0{bail!("opportunity fit requires a positive solicitation_profile_version");}
    if assessment.dimensions.is_empty(){bail!("opportunity fit requires at least one fit dimension");}
    unique_non_empty(assessment.dimensions.iter().map(|item|item.key.as_str()),"fit dimension key")?;
    for dimension in &assessment.dimensions{required(&dimension.rationale,"fit dimension rationale")?;for source in &dimension.sources{validate_anchor(source)?;}}
    if approval{required(&assessment.decision_rationale,"fit decision_rationale")?;required(&assessment.decided_by_user_id,"fit decided_by_user_id")?;}
    Ok(())
}

fn validate_institutional_memory(library:InstitutionalMemoryLibrary,approval:bool)->Result<()>{
    if library.schema_version!=1{bail!("unsupported institutional memory schema version");}
    if approval&&library.entries.is_empty(){bail!("approved institutional memory requires at least one entry");}
    unique_non_empty(library.entries.iter().map(|entry|entry.id.as_str()),"institutional memory entry id")?;
    for entry in &library.entries{
        required(&entry.kind,"institutional memory kind")?;required(&entry.content,"institutional memory content")?;
        required(&entry.origin,"institutional memory origin")?;required(&entry.last_reviewed_at,"institutional memory last_reviewed_at")?;
        if approval{required(&entry.approved_by_user_id,"institutional memory approved_by_user_id")?;}
    }
    Ok(())
}

fn validate_collaboration_routing(routing:CollaborationRouting,approval:bool)->Result<()>{
    if routing.schema_version!=1{bail!("unsupported collaboration routing schema version");}
    if approval{required(&routing.project_owner_user_id,"project_owner_user_id")?;if routing.routes.is_empty(){bail!("approval routing requires at least one route");}}
    unique_non_empty(routing.routes.iter().map(|route|route.artifact_type.as_str()),"approval route artifact_type")?;
    for route in &routing.routes{
        required(&route.owner_user_id,"approval route owner_user_id")?;
        unique_non_empty(route.approver_user_ids.iter().map(String::as_str),"approval route approver_user_id")?;
        if route.minimum_approvals==0||route.minimum_approvals as usize>route.approver_user_ids.len(){bail!("approval route {} has an invalid minimum_approvals",route.artifact_type);}
    }
    Ok(())
}

fn validate_task_plan(plan:TaskPlan,approval:bool)->Result<()>{
    if plan.schema_version!=1{bail!("unsupported task plan schema version");}
    required(&plan.target_submission_at,"target_submission_at")?;
    if approval&&plan.tasks.is_empty(){bail!("approved task plan requires at least one task");}
    unique_non_empty(plan.tasks.iter().map(|task|task.id.as_str()),"task id")?;
    let ids:BTreeSet<&str>=plan.tasks.iter().map(|task|task.id.as_str()).collect();
    for task in &plan.tasks{
        required(&task.title,"task title")?;required(&task.owner_user_id,"task owner_user_id")?;required(&task.due_at,"task due_at")?;required(&task.source,"task source")?;
        for dependency in &task.dependencies{if !ids.contains(dependency.as_str()){bail!("task {} has unknown dependency {dependency}",task.id);}}
    }
    Ok(())
}

fn validate_review_simulation(run:ReviewSimulationResult,approval:bool)->Result<()>{
    if run.schema_version!=1{bail!("unsupported review simulation schema version");}
    required(&run.snapshot_id,"review snapshot_id")?;required(&run.rubric_version_id,"review rubric_version_id")?;required(&run.panel_plan_id,"review panel_plan_id")?;
    required(&run.synthetic_review_notice,"synthetic_review_notice")?;
    if approval&&run.reviews.is_empty(){bail!("approved review simulation requires individual reviews");}
    unique_non_empty(run.reviews.iter().map(|review|review.reviewer_archetype.as_str()),"reviewer archetype")?;
    for review in &run.reviews{
        required(&review.overall_assessment,"review overall_assessment")?;
        if review.criterion_scores.is_empty(){bail!("review {} has no criterion scores",review.reviewer_archetype);}
        unique_non_empty(review.criterion_scores.iter().map(|score|score.criterion_id.as_str()),"review criterion id")?;
        for score in &review.criterion_scores{
            if !(0.0..=1.0).contains(&score.confidence){bail!("review confidence must be between zero and one");}
            if score.proposal_anchors.is_empty()||score.solicitation_anchors.is_empty(){bail!("every simulated criterion review requires proposal and solicitation anchors");}
        }
    }
    if let Some(causal)=&run.causal_analysis{
        if causal.mode!="program_argument_causality"&&causal.mode!="causal_study_validity"{bail!("unsupported causal analysis mode");}
        unique_non_empty(causal.graph.nodes.iter().map(|node|node.id.as_str()),"causal node id")?;
        let nodes:BTreeSet<&str>=causal.graph.nodes.iter().map(|node|node.id.as_str()).collect();
        for node in &causal.graph.nodes{required(&node.kind,"causal node kind")?;required(&node.label,"causal node label")?;}
        for edge in &causal.graph.edges{
            if !nodes.contains(edge.from.as_str())||!nodes.contains(edge.to.as_str()){bail!("causal edge references an unknown node");}
            required(&edge.relationship,"causal edge relationship")?;
            if edge.evidence_anchors.is_empty(){bail!("causal edges require proposal or literature evidence anchors");}
        }
    }
    if approval&&!run.panel_summary.is_object(){bail!("approved review simulation requires a structured panel_summary");}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn incomplete_aims_cannot_be_approved() {
        let body = json!({"schema_version":1,"framework_version":1,"overall_objective":"","central_hypothesis_or_thesis":"",
          "aims":[{"id":"aim-1","title":"Aim 1","statement":"Test it","rationale":"","approach_summary":"","expected_outcome":"","impact":"","innovation":"","classification":"assumption"}]});
        assert!(validate_artifact_document("aim_set", &body, false).is_ok());
        assert!(validate_artifact_document("aim_set", &body, true).is_err());
    }

    #[test]
    fn approved_literature_requires_one_resolution_per_grounded_query() {
        let body = json!({
            "schema_version": 1,
            "run_id": "run-1",
            "solicitation_profile_version": 1,
            "framework_version": 1,
            "aim_set_version": 1,
            "started_at": "2030-01-01T00:00:00Z",
            "completed_at": "2030-01-01T00:01:00Z",
            "search_provider": "test",
            "queries": [{
                "id": "query-1", "query": "targeted evidence query",
                "rationale": "Resolve the evidence gap", "aim_ids": ["aim-1"],
                "requirement_ids": ["R-001"], "criterion_ids": ["C-001"],
                "preferred_domains": ["nih.gov"]
            }],
            "evidence_needs": [],
            "source_ids": [], "citation_ids": [], "contradictions": []
        });
        assert!(validate_artifact_document("literature_manifest", &body, false).is_ok());
        assert!(validate_artifact_document("literature_manifest", &body, true).is_err());

        let mut complete = body;
        complete["evidence_needs"] = json!([{
            "evidence_need_id": "query-1", "disposition": "unresolved_risk",
            "evidence_ids": [], "rationale": "No qualifying source was located"
        }]);
        assert!(validate_artifact_document("literature_manifest", &complete, true).is_ok());
    }
}

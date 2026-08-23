use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RequirementDraft {
    pub external_id: String,
    pub category: String,
    pub requirement: String,
    #[serde(default)]
    pub mandatory: bool,
    #[serde(default)]
    pub evidence_needed: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub source_clue: String,
    #[serde(default)]
    pub source_document: Option<String>,
    #[serde(default)]
    pub source_locator: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RequirementsEnvelope {
    pub requirements: Vec<RequirementDraft>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InterviewQuestionDraft {
    pub requirement_id: String,
    pub question: String,
    pub answer_type: String,
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub why_needed: String,
    #[serde(default)]
    pub evidence_requested: bool,
    #[serde(default)]
    pub priority: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InterviewEnvelope {
    pub questions: Vec<InterviewQuestionDraft>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResearchQueryDraft {
    pub requirement_id: String,
    pub query: String,
    #[serde(default)]
    pub aim_ids: Vec<String>,
    #[serde(default)]
    pub criterion_ids: Vec<String>,
    #[serde(default)]
    pub preferred_domains: Vec<String>,
    #[serde(default)]
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResearchPlanEnvelope {
    pub queries: Vec<ResearchQueryDraft>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CitationCandidate {
    pub source_id: String,
    pub title: String,
    pub url: String,
    pub passage: String,
    pub published_at: Option<String>,
    pub retrieved_at: String,
    pub content_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EvidenceValidationItem {
    pub source_index: usize,
    pub status: String,
    pub confidence: f64,
    #[serde(default)]
    pub supporting_excerpt: String,
    #[serde(default)]
    pub explanation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EvidenceValidationEnvelope {
    pub validations: Vec<EvidenceValidationItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalRecord {
    pub row: u32,
    pub item_id: String,
    pub kind: String,
    #[serde(default)]
    pub requirement_id: Option<String>,
    pub source_ref: String,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub source_locator: Option<String>,
    pub text: String,
    #[serde(default)]
    pub confidence: f32,
    pub status: String,
    #[serde(default)]
    pub created_unix: Option<i64>,
}

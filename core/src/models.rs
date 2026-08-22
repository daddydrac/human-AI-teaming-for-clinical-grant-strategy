use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, time::Duration};

use crate::workflow::WorkflowConfig;

pub struct ModelTask {
    pub kind: String,
    pub prompt: String,
    pub high_value: bool,
}
#[derive(Debug, Clone)]
pub struct ModelOutput {
    pub model: String,
    pub provider: String,
    pub routing_mode: String,
    pub task_kind: String,
    pub generation_run_id: String,
    pub text: String,
}

pub trait GenerationAudit: Send + Sync {
    fn workflow_config_for_model(&self, project: &str) -> Result<WorkflowConfig>;
    fn begin_generation(
        &self,
        project: &str,
        task_kind: &str,
        routing_mode: &str,
        provider: &str,
        model: &str,
        prompt_sha256: &str,
        high_value: bool,
    ) -> Result<String>;
    fn complete_generation(
        &self,
        run_id: &str,
        response_sha256: &str,
    ) -> Result<()>;
    fn fail_generation(&self, run_id: &str, error: &str) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRoutingPolicy {
    mode: RoutingMode,
    cloud_task_kinds: HashSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingDecision {
    pub routing_mode: String,
    pub provider: String,
    pub model: String,
    pub reason: String,
}

fn parse_olmo_wire_response(
    v: &serde_json::Value,
    model: &str,
    max_tokens: usize,
) -> Result<ModelOutput> {
    if v.pointer("/choices/0/finish_reason")
        .and_then(|x| x.as_str())
        == Some("length")
    {
        anyhow::bail!("local model response reached LOCAL_LLM_MAX_TOKENS={max_tokens}; increase the limit or reduce the source/context size and retry");
    }
    let text = v
        .pointer("/choices/0/message/content")
        .and_then(|x| x.as_str())
        .context("missing local model response")?
        .to_string();
    Ok(ModelOutput {
        model: model.to_string(),
        provider: "local".into(),
        routing_mode: String::new(),
        task_kind: String::new(),
        generation_run_id: String::new(),
        text,
    })
}

fn parse_claude_wire_response(
    v: &serde_json::Value,
    model: &str,
    max_tokens: usize,
) -> Result<ModelOutput> {
    if v.get("stop_reason").and_then(|x| x.as_str()) == Some("max_tokens") {
        anyhow::bail!(
            "Claude response reached CLAUDE_MAX_TOKENS={max_tokens}; increase the limit and retry"
        );
    }
    let text = v
        .get("content")
        .and_then(|x| x.as_array())
        .and_then(|a| {
            a.iter()
                .find_map(|x| x.get("text").and_then(|t| t.as_str()))
        })
        .context("missing Claude response")?
        .to_string();
    Ok(ModelOutput {
        model: model.to_string(),
        provider: "anthropic".into(),
        routing_mode: String::new(),
        task_kind: String::new(),
        generation_run_id: String::new(),
        text,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingMode {
    Hybrid,
    ClaudeOnly,
    LocalOnly,
}
impl RoutingMode {
    fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "hybrid" => Ok(Self::Hybrid),
            "claude_only" | "cloud" | "cloud_only" => Ok(Self::ClaudeOnly),
            "local_only" | "local" => Ok(Self::LocalOnly),
            other => anyhow::bail!("unsupported model routing mode: {other}"),
        }
    }

    fn from_env() -> Self {
        Self::parse(
            &std::env::var("MODEL_ROUTING_MODE").unwrap_or_else(|_| "hybrid".into()),
        )
        .unwrap_or(Self::Hybrid)
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Hybrid => "hybrid",
            Self::ClaudeOnly => "claude_only",
            Self::LocalOnly => "local_only",
        }
    }
}

pub struct ModelRouter {
    client: Client,
    local_url: String,
    local_model: String,
    local_prompt_prefix: String,
    anthropic_key: Option<String>,
    claude_model: String,
    claude_task_kinds: HashSet<String>,
    routing_mode: RoutingMode,
}
impl ModelRouter {
    pub fn from_env() -> Self {
        let timeout = std::env::var("MODEL_HTTP_TIMEOUT_SECONDS")
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(300u64);
        Self{
            client:Client::builder().timeout(Duration::from_secs(timeout)).build().expect("HTTP client"),
            local_url:std::env::var("LOCAL_LLM_URL").or_else(|_|std::env::var("OLMO_URL")).unwrap_or_else(|_|"http://host.docker.internal:8000/v1/chat/completions".into()),
            local_model:std::env::var("LOCAL_LLM_MODEL").or_else(|_|std::env::var("OLMO_MODEL")).unwrap_or_else(|_|"grant-olmo".into()),
            local_prompt_prefix:std::env::var("LOCAL_LLM_PROMPT_PREFIX").unwrap_or_default(),
            anthropic_key:std::env::var("ANTHROPIC_API_KEY").ok().filter(|s|!s.trim().is_empty()),
            claude_model:std::env::var("CLAUDE_MODEL").unwrap_or_else(|_|"claude-sonnet-4-5".into()),
            claude_task_kinds:std::env::var("CLAUDE_TASK_KINDS")
                .unwrap_or_else(|_|"requirement_decomposition,sponsor_compliance_compilation,investigator_interview,research_planning,evidence_validation,competitor_profile,competitive_positioning,complex_scientific_synthesis".into())
                .split(',').map(str::trim).filter(|x|!x.is_empty()).map(str::to_owned).collect(),
            routing_mode:RoutingMode::from_env(),
        }
    }

    pub async fn health(&self) -> Result<serde_json::Value> {
        let require_hybrid_claude = std::env::var("REQUIRE_CLAUDE_IN_HYBRID")
            .ok()
            .map(|x| {
                matches!(
                    x.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false);
        if self.routing_mode == RoutingMode::Hybrid
            && require_hybrid_claude
            && self.anthropic_key.is_none()
        {
            anyhow::bail!("MODEL_ROUTING_MODE=hybrid with REQUIRE_CLAUDE_IN_HYBRID=true requires ANTHROPIC_API_KEY");
        }
        if self.routing_mode == RoutingMode::ClaudeOnly {
            if self.anthropic_key.is_none() {
                anyhow::bail!("MODEL_ROUTING_MODE=claude_only requires ANTHROPIC_API_KEY");
            }
            return Ok(
                serde_json::json!({"ok":true,"model":self.claude_model,"provider":"anthropic","routing_mode":self.routing_mode.as_str()}),
            );
        }
        let models_url = self
            .local_url
            .split("/v1/")
            .next()
            .unwrap_or(&self.local_url)
            .trim_end_matches('/')
            .to_string()
            + "/v1/models";
        let r = self
            .client
            .get(models_url)
            .send()
            .await?
            .error_for_status()?;
        let v: serde_json::Value = r.json().await?;
        let found = v
            .get("data")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter().any(|m| {
                    m.get("id").and_then(|x| x.as_str()) == Some(self.local_model.as_str())
                })
            })
            .unwrap_or(false);
        if !found {
            anyhow::bail!(
                "configured local model '{}' is not reported by the local model server",
                self.local_model
            );
        }
        Ok(
            serde_json::json!({"ok":true,"model":self.local_model,"provider":"local","routing_mode":self.routing_mode.as_str(),"claude_configured":self.anthropic_key.is_some()}),
        )
    }

    pub fn project_policy(&self, config: &WorkflowConfig) -> Result<ProjectRoutingPolicy> {
        let mode = match config.model_routing_mode.as_deref() {
            Some(value) => RoutingMode::parse(value)?,
            None => self.routing_mode,
        };
        match self.routing_mode {
            RoutingMode::LocalOnly if mode != RoutingMode::LocalOnly => {
                anyhow::bail!("deployment is local_only and cannot execute a project configured for cloud routing")
            }
            RoutingMode::ClaudeOnly if mode != RoutingMode::ClaudeOnly => {
                anyhow::bail!("deployment is claude_only and has no approved local execution path")
            }
            _ => {}
        }
        if let Some(model) = config.local_model.as_deref().filter(|value| !value.trim().is_empty()) {
            if model != self.local_model {
                anyhow::bail!(
                    "project local model '{}' is unavailable; this deployment provides '{}'",
                    model,
                    self.local_model
                );
            }
        }
        if let Some(model) = config.cloud_model.as_deref().filter(|value| !value.trim().is_empty()) {
            if model != self.claude_model {
                anyhow::bail!(
                    "project cloud model '{}' is unavailable; this deployment provides '{}'",
                    model,
                    self.claude_model
                );
            }
        }
        let cloud_task_kinds = if config.cloud_task_kinds.is_empty() {
            self.claude_task_kinds.clone()
        } else {
            config.cloud_task_kinds.iter().cloned().collect()
        };
        Ok(ProjectRoutingPolicy {
            mode,
            cloud_task_kinds,
        })
    }

    pub fn route(&self, policy: &ProjectRoutingPolicy, task: &ModelTask) -> Result<RoutingDecision> {
        let (provider, model, reason) = match policy.mode {
            RoutingMode::LocalOnly => (
                "local",
                self.local_model.as_str(),
                "project policy prohibits cloud processing",
            ),
            RoutingMode::ClaudeOnly => {
                if self.anthropic_key.is_none() {
                    anyhow::bail!("project requires Claude but ANTHROPIC_API_KEY is unavailable");
                }
                (
                    "anthropic",
                    self.claude_model.as_str(),
                    "project policy requires Claude for every model task",
                )
            }
            RoutingMode::Hybrid => {
                let cloud = task.high_value || policy.cloud_task_kinds.contains(&task.kind);
                if cloud {
                    if self.anthropic_key.is_none() {
                        anyhow::bail!(
                            "project hybrid policy routes task '{}' to Claude, but ANTHROPIC_API_KEY is unavailable",
                            task.kind
                        );
                    }
                    (
                        "anthropic",
                        self.claude_model.as_str(),
                        "task is configured for cloud execution",
                    )
                } else {
                    (
                        "local",
                        self.local_model.as_str(),
                        "task is configured for local execution",
                    )
                }
            }
        };
        Ok(RoutingDecision {
            routing_mode: policy.mode.as_str().into(),
            provider: provider.into(),
            model: model.into(),
            reason: reason.into(),
        })
    }

    pub fn routing_disclosure(&self, config: &WorkflowConfig) -> Result<serde_json::Value> {
        let policy = self.project_policy(config)?;
        let mut cloud_task_kinds = policy.cloud_task_kinds.iter().cloned().collect::<Vec<_>>();
        cloud_task_kinds.sort();
        Ok(json!({
            "routing_mode": policy.mode.as_str(),
            "local_provider": "local",
            "local_model": self.local_model,
            "cloud_provider": "anthropic",
            "cloud_model": self.claude_model,
            "cloud_task_kinds": if policy.mode == RoutingMode::Hybrid { cloud_task_kinds } else { Vec::<String>::new() },
            "cloud_receives_project_content": policy.mode != RoutingMode::LocalOnly,
            "deployment_routing_ceiling": self.routing_mode.as_str()
        }))
    }

    pub async fn generate_for_project<A: GenerationAudit + ?Sized>(
        &self,
        audit: &A,
        project: &str,
        task: ModelTask,
    ) -> Result<ModelOutput> {
        let config = audit.workflow_config_for_model(project)?;
        let policy = self.project_policy(&config)?;
        let decision = self.route(&policy, &task)?;
        let prompt_sha256 = hex::encode(Sha256::digest(task.prompt.as_bytes()));
        let run_id = audit.begin_generation(
            project,
            &task.kind,
            &decision.routing_mode,
            &decision.provider,
            &decision.model,
            &prompt_sha256,
            task.high_value,
        )?;
        let task_kind = task.kind.clone();
        let generated = match decision.provider.as_str() {
            "anthropic" => self.claude(task).await,
            "local" => self.local(task).await,
            other => anyhow::bail!("unsupported routed model provider: {other}"),
        };
        match generated {
            Ok(mut output) => {
                let response_sha256 = hex::encode(Sha256::digest(output.text.as_bytes()));
                audit.complete_generation(&run_id, &response_sha256)?;
                output.provider = decision.provider;
                output.routing_mode = decision.routing_mode;
                output.task_kind = task_kind;
                output.generation_run_id = run_id;
                Ok(output)
            }
            Err(error) => {
                audit.fail_generation(&run_id, &error.to_string())?;
                Err(error)
            }
        }
    }
    async fn local(&self, t: ModelTask) -> Result<ModelOutput> {
        let max_tokens = std::env::var("LOCAL_LLM_MAX_TOKENS")
            .or_else(|_| std::env::var("OLMO_MAX_TOKENS"))
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(4096usize);
        let prompt = if self.local_prompt_prefix.trim().is_empty() {
            t.prompt
        } else {
            format!("{}\n{}", self.local_prompt_prefix.trim(), t.prompt)
        };
        let r=self.client.post(&self.local_url).json(&json!({"model":self.local_model,"messages":[{"role":"system","content":"You are a rigorous enterprise oncology grant-writing system. Obey output schemas exactly. Never invent evidence, citations, approvals, clinical results, or institutional capabilities. Explicitly preserve uncertainty."},{"role":"user","content":prompt}],"temperature":0.1,"max_tokens":max_tokens})).send().await?.error_for_status()?;
        let v: serde_json::Value = r.json().await?;
        parse_olmo_wire_response(&v, &self.local_model, max_tokens)
    }
    async fn claude(&self, t: ModelTask) -> Result<ModelOutput> {
        let key = self
            .anthropic_key
            .as_ref()
            .context("ANTHROPIC_API_KEY missing")?;
        let max_tokens = std::env::var("CLAUDE_MAX_TOKENS")
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(6000usize);
        let r=self.client.post("https://api.anthropic.com/v1/messages")
            .header("x-api-key",key).header("anthropic-version","2023-06-01")
            .json(&json!({"model":self.claude_model,"max_tokens":max_tokens,"temperature":0.1,"system":"You are the senior scientific grant strategist. Never fabricate evidence or citations. Preserve uncertainty and distinguish facts, investigator estimates, assumptions, and unknowns. When strict JSON is requested, return strict JSON only.","messages":[{"role":"user","content":t.prompt}]}))
            .send().await?.error_for_status()?;
        let v: serde_json::Value = r.json().await?;
        parse_claude_wire_response(&v, &self.claude_model, max_tokens)
    }
}

#[cfg(test)]
mod contract_mock_tests {
    use super::*;
    use crate::source_locator::{compile_model_output, SourceDocument};
    use sha2::Digest;

    fn router(mode: RoutingMode, anthropic_key: Option<&str>) -> ModelRouter {
        ModelRouter {
            client: Client::new(),
            local_url: "http://127.0.0.1:1/v1/chat/completions".into(),
            local_model: "local-test-model".into(),
            local_prompt_prefix: String::new(),
            anthropic_key: anthropic_key.map(str::to_owned),
            claude_model: "claude-test-model".into(),
            claude_task_kinds: ["evidence_validation".to_string()].into_iter().collect(),
            routing_mode: mode,
        }
    }

    fn workflow(mode: &str) -> WorkflowConfig {
        WorkflowConfig {
            schema_version: 1,
            definition_version: 1,
            template: "test".into(),
            enabled_modules: Vec::new(),
            required_modules: Vec::new(),
            review_mode: None,
            review_required: false,
            grant_type: None,
            target_deadline: None,
            model_routing_mode: Some(mode.into()),
            local_model_provider: Some("local".into()),
            local_model: Some("local-test-model".into()),
            cloud_model: Some("claude-test-model".into()),
            cloud_task_kinds: vec!["evidence_validation".into()],
        }
    }

    fn compliance_contract() -> String {
        serde_json::json!({"profile":{"sponsor":"Example Sponsor","mechanism":"R01","submission_system":"Portal","deadline_iso":"2030-10-15","rules":[
          {"rule_id":"C-001","category":"format","rule_type":"max_pages","scope":"section","target":"Research Strategy","severity":"hard","mandatory":true,"numeric_value":12.0,"text_value":null,"list_value":[],"source_hint":"Research Strategy page limitation","source_document_hint":"Pasted funding opportunity","source_page_hint":null,"notes":""},
          {"rule_id":"C-002","category":"format","rule_type":"min_font_size_pt","scope":"proposal","target":"application text","severity":"hard","mandatory":true,"numeric_value":11.0,"text_value":null,"list_value":[],"source_hint":"application text font size 11 points","source_document_hint":"Pasted funding opportunity","source_page_hint":null,"notes":""}
        ]}}).to_string()
    }

    fn source() -> Vec<SourceDocument> {
        let text="Formatting\nThe Research Strategy may not exceed 12 pages.\nApplication text must use a font size of 11 points or larger.";
        vec![SourceDocument{id:17,name:"Pasted funding opportunity".into(),kind:"funding_paste".into(),text:text.into(),sha256:hex::encode(sha2::Sha256::digest(text.as_bytes()))}]
    }

    #[test]
    fn claude_and_olmo_mocks_use_real_wire_and_domain_contracts() {
        let contract = compliance_contract();
        let claude = json!({"id":"msg_mock","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":contract}],"stop_reason":"end_turn"});
        let olmo = json!({"id":"chatcmpl-mock","object":"chat.completion","model":"grant-olmo","choices":[{"index":0,"message":{"role":"assistant","content":compliance_contract()},"finish_reason":"stop"}]});
        let claude_output =
            parse_claude_wire_response(&claude, "claude-sonnet-4-5", 16000).unwrap();
        let olmo_output = parse_olmo_wire_response(&olmo, "grant-olmo", 16000).unwrap();
        let documents = source();
        let claude_profile = compile_model_output(&claude_output.text, &documents).unwrap();
        let olmo_profile = compile_model_output(&olmo_output.text, &documents).unwrap();
        assert_eq!(claude_profile.rules.len(), 2);
        assert_eq!(olmo_profile.rules.len(), 2);
        for profile in [&claude_profile, &olmo_profile] {
            for rule in &profile.rules {
                assert_eq!(rule.source_status, "located");
                let document = &documents[0];
                let exact = &document.text
                    [rule.source_start_offset.unwrap()..rule.source_end_offset.unwrap()];
                assert_eq!(rule.source_excerpt, exact);
                assert_eq!(rule.source_document_id, Some(17));
            }
        }
        assert_eq!(claude_profile.rules[0].numeric_value, Some(12.0));
        assert_eq!(olmo_profile.rules[1].numeric_value, Some(11.0));
    }

    #[test]
    fn provider_faithful_mock_rejects_wrong_contract_types() {
        let bad_contract = r#"{"profile":{"rules":[{"rule_id":"C-001","category":"format","rule_type":"max_pages","target":"Research Strategy","severity":"hard","mandatory":true,"numeric_value":"twelve","source_hint":"Research Strategy page limitation"}]}}"#;
        let wire = json!({"choices":[{"message":{"content":bad_contract},"finish_reason":"stop"}]});
        let output = parse_olmo_wire_response(&wire, "grant-olmo", 16000).unwrap();
        assert!(compile_model_output(&output.text, &source()).is_err());
    }

    #[test]
    fn local_only_project_cannot_route_high_value_content_to_claude() {
        let router = router(RoutingMode::Hybrid, Some("configured"));
        let policy = router.project_policy(&workflow("local_only")).unwrap();
        let decision = router
            .route(
                &policy,
                &ModelTask {
                    kind: "evidence_validation".into(),
                    prompt: "private proposal".into(),
                    high_value: true,
                },
            )
            .unwrap();
        assert_eq!(decision.provider, "local");
        assert_eq!(decision.routing_mode, "local_only");
    }

    #[test]
    fn hybrid_cloud_task_fails_closed_without_claude_credentials() {
        let router = router(RoutingMode::Hybrid, None);
        let policy = router.project_policy(&workflow("hybrid")).unwrap();
        let error = router
            .route(
                &policy,
                &ModelTask {
                    kind: "evidence_validation".into(),
                    prompt: "private proposal".into(),
                    high_value: false,
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn deployment_policy_is_a_hard_routing_ceiling() {
        let router = router(RoutingMode::LocalOnly, Some("configured"));
        let error = router.project_policy(&workflow("hybrid")).unwrap_err();
        assert!(error.to_string().contains("deployment is local_only"));
    }
}

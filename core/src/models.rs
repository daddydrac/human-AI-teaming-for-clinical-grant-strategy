use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::json;
use std::{collections::HashSet, time::Duration};

pub struct ModelTask { pub kind:String, pub prompt:String, pub high_value:bool }
pub struct ModelOutput { pub model:String, pub text:String }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoutingMode { Hybrid, ClaudeOnly, LocalOnly }
impl RoutingMode {
    fn from_env() -> Self {
        match std::env::var("MODEL_ROUTING_MODE").unwrap_or_else(|_|"hybrid".into()).to_ascii_lowercase().as_str() {
            "claude_only" | "cloud" | "cloud_only" => Self::ClaudeOnly,
            "local_only" | "local" => Self::LocalOnly,
            _ => Self::Hybrid,
        }
    }
    fn as_str(self)->&'static str { match self {Self::Hybrid=>"hybrid",Self::ClaudeOnly=>"claude_only",Self::LocalOnly=>"local_only"} }
}

pub struct ModelRouter {
    client: Client,
    olmo_url: String,
    olmo_model: String,
    anthropic_key: Option<String>,
    claude_model: String,
    claude_task_kinds: HashSet<String>,
    routing_mode: RoutingMode,
}
impl ModelRouter {
    pub fn from_env()->Self{
        let timeout=std::env::var("MODEL_HTTP_TIMEOUT_SECONDS").ok().and_then(|x|x.parse().ok()).unwrap_or(300u64);
        Self{
            client:Client::builder().timeout(Duration::from_secs(timeout)).build().expect("HTTP client"),
            olmo_url:std::env::var("OLMO_URL").unwrap_or_else(|_|"http://host.docker.internal:8000/v1/chat/completions".into()),
            olmo_model:std::env::var("OLMO_MODEL").unwrap_or_else(|_|"grant-olmo".into()),
            anthropic_key:std::env::var("ANTHROPIC_API_KEY").ok().filter(|s|!s.trim().is_empty()),
            claude_model:std::env::var("CLAUDE_MODEL").unwrap_or_else(|_|"claude-sonnet-4-5".into()),
            claude_task_kinds:std::env::var("CLAUDE_TASK_KINDS")
                .unwrap_or_else(|_|"requirement_decomposition,sponsor_compliance_compilation,investigator_interview,research_planning,evidence_validation,competitor_profile,competitive_positioning,complex_scientific_synthesis".into())
                .split(',').map(str::trim).filter(|x|!x.is_empty()).map(str::to_owned).collect(),
            routing_mode:RoutingMode::from_env(),
        }
    }

    pub async fn health(&self)->Result<serde_json::Value>{
        let require_hybrid_claude=std::env::var("REQUIRE_CLAUDE_IN_HYBRID")
            .ok().map(|x|matches!(x.trim().to_ascii_lowercase().as_str(),"1"|"true"|"yes"|"on")).unwrap_or(false);
        if self.routing_mode==RoutingMode::Hybrid && require_hybrid_claude && self.anthropic_key.is_none() {
            anyhow::bail!("MODEL_ROUTING_MODE=hybrid with REQUIRE_CLAUDE_IN_HYBRID=true requires ANTHROPIC_API_KEY");
        }
        if self.routing_mode==RoutingMode::ClaudeOnly {
            if self.anthropic_key.is_none(){anyhow::bail!("MODEL_ROUTING_MODE=claude_only requires ANTHROPIC_API_KEY");}
            return Ok(serde_json::json!({"ok":true,"model":self.claude_model,"provider":"anthropic","routing_mode":self.routing_mode.as_str()}));
        }
        let models_url=self.olmo_url.split("/v1/").next().unwrap_or(&self.olmo_url).trim_end_matches('/').to_string()+"/v1/models";
        let r=self.client.get(models_url).send().await?.error_for_status()?;
        let v:serde_json::Value=r.json().await?;
        let found=v.get("data").and_then(|x|x.as_array()).map(|a|a.iter().any(|m|m.get("id").and_then(|x|x.as_str())==Some(self.olmo_model.as_str()))).unwrap_or(false);
        if !found { anyhow::bail!("configured local model '{}' is not reported by the local model server",self.olmo_model); }
        Ok(serde_json::json!({"ok":true,"model":self.olmo_model,"provider":"local","routing_mode":self.routing_mode.as_str(),"claude_configured":self.anthropic_key.is_some()}))
    }

    pub async fn generate(&self,t:ModelTask)->Result<ModelOutput>{
        match self.routing_mode {
            RoutingMode::ClaudeOnly => self.claude(t).await,
            RoutingMode::LocalOnly => self.olmo(t).await,
            RoutingMode::Hybrid => {
                let claude_worthy=t.high_value || self.claude_task_kinds.contains(&t.kind);
                if claude_worthy {
                    if self.anthropic_key.is_some(){ return self.claude(t).await; }
                    tracing::warn!(task=%t.kind,"Claude requested but ANTHROPIC_API_KEY is unavailable; falling back to local OLMo");
                }
                self.olmo(t).await
            }
        }
    }
    async fn olmo(&self,t:ModelTask)->Result<ModelOutput>{
        let max_tokens=std::env::var("OLMO_MAX_TOKENS").ok().and_then(|x|x.parse().ok()).unwrap_or(4096usize);
        let r=self.client.post(&self.olmo_url).json(&json!({"model":self.olmo_model,"messages":[{"role":"system","content":"You are a rigorous enterprise oncology grant-writing system. Obey output schemas exactly. Never invent evidence, citations, approvals, clinical results, or institutional capabilities. Explicitly preserve uncertainty."},{"role":"user","content":t.prompt}],"temperature":0.1,"max_tokens":max_tokens})).send().await?.error_for_status()?;
        let v:serde_json::Value=r.json().await?;
        if v.pointer("/choices/0/finish_reason").and_then(|x|x.as_str())==Some("length") {
            anyhow::bail!("local OLMo response reached OLMO_MAX_TOKENS={max_tokens}; increase the limit or reduce the source/context size and retry");
        }
        let text=v.pointer("/choices/0/message/content").and_then(|x|x.as_str()).context("missing local model response")?.to_string();
        Ok(ModelOutput{model:self.olmo_model.clone(),text})
    }
    async fn claude(&self,t:ModelTask)->Result<ModelOutput>{
        let key=self.anthropic_key.as_ref().context("ANTHROPIC_API_KEY missing")?;
        let max_tokens=std::env::var("CLAUDE_MAX_TOKENS").ok().and_then(|x|x.parse().ok()).unwrap_or(6000usize);
        let r=self.client.post("https://api.anthropic.com/v1/messages")
            .header("x-api-key",key).header("anthropic-version","2023-06-01")
            .json(&json!({"model":self.claude_model,"max_tokens":max_tokens,"temperature":0.1,"system":"You are the senior scientific grant strategist. Never fabricate evidence or citations. Preserve uncertainty and distinguish facts, investigator estimates, assumptions, and unknowns. When strict JSON is requested, return strict JSON only.","messages":[{"role":"user","content":t.prompt}]}))
            .send().await?.error_for_status()?;
        let v:serde_json::Value=r.json().await?;
        if v.get("stop_reason").and_then(|x|x.as_str())==Some("max_tokens") {
            anyhow::bail!("Claude response reached CLAUDE_MAX_TOKENS={max_tokens}; increase the limit and retry");
        }
        let text=v.get("content").and_then(|x|x.as_array()).and_then(|a|a.iter().find_map(|x|x.get("text").and_then(|t|t.as_str()))).context("missing Claude response")?.to_string();
        Ok(ModelOutput{model:self.claude_model.clone(),text})
    }
}

use anyhow::{bail, Context, Result};
use futures::{stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::{BTreeMap, BTreeSet, HashMap, VecDeque}, sync::Arc, time::Duration};
use tokio::time::sleep;

use crate::{
    embedding::EmbeddingClient,
    hpc,
    json_extract::parse_json_from_model,
    models::{ModelRouter, ModelTask},
    research::ResearchClient,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitiveConfig {
    pub schema_version: u32,
    pub endpoints: CompetitiveEndpoints,
    pub providers: ProviderConfig,
    pub limits: CompetitiveLimits,
    pub rate_limits: RateLimits,
    pub scoring: ScoringConfig,
    pub updates: CompetitiveUpdateConfig,
    #[serde(default)]
    pub ip_search_domains: Vec<String>,
    #[serde(default)]
    pub technology_search_domains: Vec<String>,
    #[serde(default)]
    pub organization_suffixes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitiveEndpoints {
    pub nih_reporter_projects: String,
    pub clinical_trials_studies: String,
    pub openalex_works: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub nih_reporter: bool,
    pub clinical_trials: bool,
    pub openalex: bool,
    pub ip_web: bool,
    pub technology_web: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitiveLimits {
    pub max_profile_search_queries: usize,
    pub nih_reporter_results_per_query: usize,
    pub clinical_trials_results_per_query: usize,
    pub openalex_results_per_query: usize,
    pub max_candidates: usize,
    pub enrich_top_candidates: usize,
    pub max_assets_per_candidate: usize,
    pub max_assets_total: usize,
    pub ip_web_results_per_candidate: usize,
    pub technology_web_results_per_candidate: usize,
    pub strategy_candidates: usize,
    pub strategy_assets_per_candidate: usize,
    pub strategy_max_chars: usize,
    pub web_enrichment_concurrency: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimits {
    pub nih_reporter_min_interval_ms: u64,
    pub clinical_trials_min_interval_ms: u64,
    pub openalex_min_interval_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringConfig {
    pub asset_type_weights: BTreeMap<String, f32>,
    pub breadth_weight: f32,
    pub top_assets_per_type: usize,
    pub count_saturation: f32,
    pub minimum_asset_relevance: f32,
    #[serde(default)]
    pub provider_reliability: BTreeMap<String, f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitiveUpdateConfig {
    pub auto_revise_sections: bool,
    pub max_sections_per_refresh: usize,
    pub candidate_score_delta: f32,
    #[serde(default)]
    pub update_all_sections_on_strategy_change: bool,
    #[serde(default)]
    pub update_all_sections_on_material_change: bool,
    #[serde(default)]
    pub section_refresh_high_value: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityDimension {
    pub id: String,
    pub label: String,
    pub description: String,
    pub weight: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitiveSearchSpec {
    pub dimension_id: String,
    pub query: String,
    #[serde(default)]
    pub source_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitiveProfile {
    pub summary: String,
    #[serde(default)]
    pub likely_applicant_types: Vec<String>,
    #[serde(default)]
    pub capability_dimensions: Vec<CapabilityDimension>,
    #[serde(default)]
    pub disease_terms: Vec<String>,
    #[serde(default)]
    pub technology_terms: Vec<String>,
    #[serde(default)]
    pub clinical_terms: Vec<String>,
    #[serde(default)]
    pub ip_terms: Vec<String>,
    #[serde(default)]
    pub grant_terms: Vec<String>,
    #[serde(default)]
    pub search_queries: Vec<CompetitiveSearchSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitiveProfileEnvelope { pub profile: CompetitiveProfile }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicAsset {
    pub asset_key: String,
    pub candidate_key: String,
    pub candidate_name: String,
    pub provider: String,
    pub asset_type: String,
    pub external_id: String,
    pub title: String,
    pub summary: String,
    pub url: Option<String>,
    pub year: Option<i32>,
    pub amount: Option<f64>,
    pub dimension_id: Option<String>,
    pub metadata: Value,
    #[serde(default)]
    pub relevance: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DimensionCoverage {
    pub dimension_id: String,
    pub label: String,
    pub score: f32,
    pub supporting_asset_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateScore {
    pub candidate_key: String,
    pub name: String,
    pub overall_score: f32,
    pub grant_score: f32,
    pub publication_score: f32,
    pub clinical_trial_score: f32,
    pub patent_ip_score: f32,
    pub technology_score: f32,
    pub breadth_score: f32,
    pub asset_count: usize,
    pub asset_counts: BTreeMap<String, usize>,
    pub dimension_coverage: Vec<DimensionCoverage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub provider: String,
    pub ok: bool,
    pub records: usize,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitiveStrategy {
    pub market_summary: String,
    #[serde(default)]
    pub positioning_principles: Vec<String>,
    #[serde(default)]
    pub differentiators: Vec<StrategyDifferentiator>,
    #[serde(default)]
    pub gaps_to_close: Vec<StrategyGap>,
    #[serde(default)]
    pub do_not_claim: Vec<String>,
    #[serde(default)]
    pub candidate_notes: Vec<CandidateNote>,
    #[serde(default)]
    pub section_guidance: Vec<SectionGuidance>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitiveStrategyEnvelope { pub strategy: CompetitiveStrategy }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyDifferentiator {
    pub theme: String,
    pub our_advantage: String,
    pub public_competitor_signal: String,
    #[serde(default)] pub candidate_keys: Vec<String>,
    #[serde(default)] pub asset_keys: Vec<String>,
    pub confidence: String,
    #[serde(default)] pub section_targets: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyGap {
    pub gap: String,
    pub why_it_matters: String,
    pub recommended_action: String,
    #[serde(default)] pub candidate_keys: Vec<String>,
    #[serde(default)] pub asset_keys: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateNote {
    pub candidate_key: String,
    pub why_relevant: String,
    pub how_to_outposition: String,
    #[serde(default)] pub asset_keys: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionGuidance {
    pub section_key: String,
    pub guidance: String,
    #[serde(default)] pub asset_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitiveRunOutput {
    pub candidates: Vec<CandidateScore>,
    pub assets: Vec<PublicAsset>,
    pub provider_status: Vec<ProviderStatus>,
    pub strategy: CompetitiveStrategy,
    pub strategy_model: String,
}

#[derive(Clone)]
pub struct CompetitiveEngine {
    client: Client,
    research: Arc<ResearchClient>,
    embedding: Arc<EmbeddingClient>,
    router: Arc<ModelRouter>,
    config: CompetitiveConfig,
    home_aliases: Vec<String>,
    openalex_mailto: Option<String>,
    openalex_api_key: Option<String>,
}

impl CompetitiveEngine {
    pub fn from_env(research: Arc<ResearchClient>, embedding: Arc<EmbeddingClient>, router: Arc<ModelRouter>) -> Result<Self> {
        let path=std::env::var("COMPETITIVE_CONFIG_PATH").unwrap_or_else(|_|"/app/config/competitive_intelligence.json".into());
        let raw=std::fs::read_to_string(&path).with_context(||format!("read competitive intelligence config {path}"))?;
        let mut config:CompetitiveConfig=serde_json::from_str(&raw).context("parse competitive intelligence config")?;
        validate_config(&mut config)?;
        let timeout=std::env::var("COMPETITIVE_HTTP_TIMEOUT_SECONDS").ok().and_then(|v|v.parse().ok()).unwrap_or(45u64);
        let user_agent=std::env::var("COMPETITIVE_USER_AGENT").unwrap_or_else(|_|format!("GrantWriterCompetitive/{}",env!("CARGO_PKG_VERSION")));
        let client=Client::builder().timeout(Duration::from_secs(timeout)).user_agent(user_agent).build()?;
        let mut aliases=std::env::var("COMPETITIVE_HOME_ORGANIZATION_ALIASES").ok().unwrap_or_else(||std::env::var("ORGANIZATION_NAME").unwrap_or_default()).split(',').map(str::trim).filter(|x|!x.is_empty()).map(ToOwned::to_owned).collect::<Vec<_>>();
        if let Ok(home)=std::env::var("ORGANIZATION_NAME") { if !home.trim().is_empty(){ aliases.push(home); } }
        aliases.sort(); aliases.dedup();
        Ok(Self{client,research,embedding,router,config,home_aliases:aliases,openalex_mailto:std::env::var("OPENALEX_MAILTO").ok().filter(|x|!x.trim().is_empty()),openalex_api_key:std::env::var("OPENALEX_API_KEY").ok().filter(|x|!x.trim().is_empty())})
    }
    pub fn config_sha256(&self)->Result<String>{Ok(sha256_hex(&serde_json::to_vec(&self.config)?))}
    pub fn config(&self)->&CompetitiveConfig{&self.config}

    pub async fn generate_profile(&self, context:&str)->Result<(CompetitiveProfile,String)> {
        let prompt=format!(r#"Build a likely strong-applicant capability profile for this funding opportunity. This is NOT a prediction of confidential applicants. It is a public-intelligence search plan describing what organizations or teams would plausibly be competitive based on the grant requirements and the authoritative clinical/scientific design.

Return STRICT JSON only with this shape:
{{"profile":{{
  "summary":"compact description of the strongest plausible applicant profile",
  "likely_applicant_types":["academic cancer center","biotechnology company"],
  "capability_dimensions":[{{"id":"DIM-01","label":"short label","description":"specific capability that would make an applicant competitive","weight":0.2}}],
  "disease_terms":["terms"],
  "technology_terms":["terms"],
  "clinical_terms":["terms"],
  "ip_terms":["terms"],
  "grant_terms":["terms"],
  "search_queries":[{{"dimension_id":"DIM-01","query":"precise public search query","source_types":["nih_grants","publications","clinical_trials"]}}]
}}}}
Rules:
- Capability dimensions must be specific, observable from public evidence, and collectively represent what would make an applicant competitive.
- Dimension weights must be positive; they will be normalized by the backend.
- Search queries must map to an existing dimension ID and use only source types nih_grants, publications, clinical_trials.
- Use disease, technology, modality, biomarker, clinical, translational, implementation, and commercialization concepts actually present in the project context.
- Do not name organizations unless they are already explicitly present in the supplied context.
- Do not infer applicant intent or confidential information.

PROJECT CONTEXT:
{}"#,context);
        let out=self.router.generate(ModelTask{kind:"competitor_profile".into(),prompt,high_value:false}).await?;
        let mut env:CompetitiveProfileEnvelope=parse_json_from_model(&out.text)?;
        validate_profile(&mut env.profile,self.config.limits.max_profile_search_queries)?;
        Ok((env.profile,out.model))
    }

    pub async fn run(&self, profile:&CompetitiveProfile, own_context:&str)->Result<CompetitiveRunOutput> {
        let mut provider_status=Vec::new();
        let mut assets=Vec::new();
        let specs=profile.search_queries.iter().take(self.config.limits.max_profile_search_queries).cloned().collect::<Vec<_>>();
        if self.config.providers.nih_reporter {
            let before=assets.len(); let mut errors=Vec::new();
            for spec in specs.iter().filter(|s|s.source_types.iter().any(|x|x=="nih_grants")) {
                match self.nih_reporter(spec).await {Ok(mut x)=>assets.append(&mut x),Err(e)=>errors.push(e.to_string())}
                sleep(Duration::from_millis(self.config.rate_limits.nih_reporter_min_interval_ms)).await;
            }
            provider_status.push(status("nih_reporter",assets.len()-before,errors));
        }
        if self.config.providers.clinical_trials {
            let before=assets.len(); let mut errors=Vec::new();
            for spec in specs.iter().filter(|s|s.source_types.iter().any(|x|x=="clinical_trials")) {
                match self.clinical_trials(spec).await {Ok(mut x)=>assets.append(&mut x),Err(e)=>errors.push(e.to_string())}
                sleep(Duration::from_millis(self.config.rate_limits.clinical_trials_min_interval_ms)).await;
            }
            provider_status.push(status("clinical_trials",assets.len()-before,errors));
        }
        if self.config.providers.openalex {
            let before=assets.len(); let mut errors=Vec::new();
            if self.openalex_api_key.is_none() {
                errors.push("OPENALEX_API_KEY is not configured; publication discovery was skipped".into());
            } else {
                for spec in specs.iter().filter(|s|s.source_types.iter().any(|x|x=="publications")) {
                    match self.openalex(spec).await {Ok(mut x)=>assets.append(&mut x),Err(e)=>errors.push(e.to_string())}
                    sleep(Duration::from_millis(self.config.rate_limits.openalex_min_interval_ms)).await;
                }
            }
            provider_status.push(status("openalex",assets.len()-before,errors));
        }
        dedup_assets(&mut assets);
        assets.retain(|a|!self.is_home_organization(&a.candidate_name));
        let keep=preselect_candidates(&assets,self.config.limits.max_candidates);
        assets.retain(|a|keep.contains(&a.candidate_key));
        cap_assets(&mut assets,self.config.limits.max_assets_per_candidate,self.config.limits.max_assets_total);

        let top_for_enrichment=preselect_candidates(&assets,self.config.limits.enrich_top_candidates);
        let enrichment_candidates=top_for_enrichment.iter().filter_map(|key|{
            assets.iter().find(|a|&a.candidate_key==key).map(|a|(key.clone(),a.candidate_name.clone()))
        }).collect::<Vec<_>>();
        if self.config.providers.ip_web {
            let before=assets.len(); let (mut enriched,errors)=self.web_enrich_many(profile,&enrichment_candidates,true).await;
            assets.append(&mut enriched);
            provider_status.push(status("ip_web",assets.len()-before,errors));
        }
        if self.config.providers.technology_web {
            let before=assets.len(); let (mut enriched,errors)=self.web_enrich_many(profile,&enrichment_candidates,false).await;
            assets.append(&mut enriched);
            provider_status.push(status("technology_web",assets.len()-before,errors));
        }
        dedup_assets(&mut assets);
        cap_assets(&mut assets,self.config.limits.max_assets_per_candidate,self.config.limits.max_assets_total);
        self.score_assets(profile,&mut assets).await?;
        assets.retain(|a|a.relevance>=self.config.scoring.minimum_asset_relevance);
        let candidates=aggregate_candidates(profile,&assets,&self.config.scoring,self.config.limits.max_candidates);
        if candidates.is_empty(){bail!("public competitive-intelligence providers returned no capability-matched candidate organizations above the configured relevance threshold");}
        let (strategy,strategy_model)=self.synthesize_strategy(profile,&candidates,&assets,own_context).await?;
        Ok(CompetitiveRunOutput{candidates,assets,provider_status,strategy,strategy_model})
    }

    async fn nih_reporter(&self,spec:&CompetitiveSearchSpec)->Result<Vec<PublicAsset>>{
        let limit=self.config.limits.nih_reporter_results_per_query.clamp(1,500);
        let payload=json!({
            "criteria":{"advanced_text_search":{"operator":"and","search_field":"projecttitle,abstracttext,terms","search_text":spec.query},"exclude_subprojects":true,"use_relevance":true},
            "include_fields":["ApplId","ActivityCode","ProjectNum","CoreProjectNum","ProjectTitle","AbstractText","FiscalYear","Organization","PrincipalInvestigators","TotalCost","OpportunityNumber","ProjectDetailUrl","ProjectStartDate","ProjectEndDate"],
            "offset":0,"limit":limit,"sort_field":"fiscal_year","sort_order":"desc"
        });
        let v:Value=self.client.post(&self.config.endpoints.nih_reporter_projects).json(&payload).send().await?.error_for_status()?.json().await?;
        let rows=v.get("results").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut out=Vec::new();
        for r in rows {
            let org=r.pointer("/organization/org_name").and_then(Value::as_str).or_else(||r.pointer("/organization/name").and_then(Value::as_str)).unwrap_or("").trim().to_string();
            if org.is_empty(){continue;}
            let title=r.get("project_title").and_then(Value::as_str).unwrap_or("NIH-funded project").to_string();
            let abstract_text=r.get("abstract_text").and_then(Value::as_str).unwrap_or("").to_string();
            let external=r.get("appl_id").map(value_string).filter(|x|!x.is_empty()).or_else(||r.get("project_num").and_then(Value::as_str).map(ToOwned::to_owned)).unwrap_or_else(||sha256_hex(title.as_bytes()));
            let url=r.get("project_detail_url").and_then(Value::as_str).map(ToOwned::to_owned);
            let year=r.get("fiscal_year").and_then(Value::as_i64).map(|x|x as i32);
            let amount=r.get("total_cost").and_then(Value::as_f64).or_else(||r.get("total_cost").and_then(Value::as_i64).map(|x|x as f64));
            let key=self.organization_key(&org); if key.is_empty(){continue;}
            out.push(make_asset(&key,&org,"nih_reporter","grant",&external,&title,&abstract_text,url,year,amount,Some(spec.dimension_id.clone()),r));
        }
        Ok(out)
    }

    async fn clinical_trials(&self,spec:&CompetitiveSearchSpec)->Result<Vec<PublicAsset>>{
        let page_size=self.config.limits.clinical_trials_results_per_query.clamp(1,1000);
        let params=vec![("query.term".to_string(),spec.query.clone()),("pageSize".to_string(),page_size.to_string()),("format".to_string(),"json".to_string())];
        let v:Value=self.client.get(&self.config.endpoints.clinical_trials_studies).query(&params).send().await?.error_for_status()?.json().await?;
        let rows=v.get("studies").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut out=Vec::new();
        for study in rows {
            let nct=study.pointer("/protocolSection/identificationModule/nctId").and_then(Value::as_str).unwrap_or("");
            if nct.is_empty(){continue;}
            let title=study.pointer("/protocolSection/identificationModule/briefTitle").and_then(Value::as_str).or_else(||study.pointer("/protocolSection/identificationModule/officialTitle").and_then(Value::as_str)).unwrap_or("Clinical study");
            let brief=study.pointer("/protocolSection/descriptionModule/briefSummary").and_then(Value::as_str).unwrap_or("");
            let detail=study.pointer("/protocolSection/descriptionModule/detailedDescription").and_then(Value::as_str).unwrap_or("");
            let interventions=study.pointer("/protocolSection/armsInterventionsModule/interventions").and_then(Value::as_array).map(|a|a.iter().filter_map(|x|x.get("name").and_then(Value::as_str)).collect::<Vec<_>>().join(", ")).unwrap_or_default();
            let summary=format!("{}\n{}\nInterventions: {}",brief,detail,interventions);
            let url=Some(format!("https://clinicaltrials.gov/study/{nct}"));
            let mut orgs=Vec::<String>::new();
            if let Some(x)=study.pointer("/protocolSection/sponsorCollaboratorsModule/leadSponsor/name").and_then(Value::as_str){orgs.push(x.to_string());}
            if let Some(a)=study.pointer("/protocolSection/sponsorCollaboratorsModule/collaborators").and_then(Value::as_array){for x in a{if let Some(n)=x.get("name").and_then(Value::as_str){orgs.push(n.to_string());}}}
            orgs.sort(); orgs.dedup();
            for org in orgs {
                let key=self.organization_key(&org); if key.is_empty(){continue;}
                out.push(make_asset(&key,&org,"clinical_trials","clinical_trial",nct,title,&summary,url.clone(),None,None,Some(spec.dimension_id.clone()),study.clone()));
            }
        }
        Ok(out)
    }

    async fn openalex(&self,spec:&CompetitiveSearchSpec)->Result<Vec<PublicAsset>>{
        let per_page=self.config.limits.openalex_results_per_query.clamp(1,200);
        let mut params=vec![("search".to_string(),spec.query.clone()),("per-page".to_string(),per_page.to_string()),("select".to_string(),"id,doi,title,publication_year,cited_by_count,authorships,primary_topic,type".to_string())];
        if let Some(mail)=self.openalex_mailto.as_deref(){params.push(("mailto".to_string(),mail.to_string()));}
        let key=self.openalex_api_key.as_deref().context("OPENALEX_API_KEY is required by OpenAlex for API requests (required since February 13, 2026)")?;
        params.push(("api_key".to_string(),key.to_string()));
        let req=self.client.get(&self.config.endpoints.openalex_works).query(&params);
        let v:Value=req.send().await?.error_for_status()?.json().await?;
        let rows=v.get("results").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut out=Vec::new();
        for work in rows {
            let id=work.get("id").and_then(Value::as_str).unwrap_or(""); if id.is_empty(){continue;}
            let title=work.get("title").and_then(Value::as_str).unwrap_or("Publication");
            let year=work.get("publication_year").and_then(Value::as_i64).map(|x|x as i32);
            let doi=work.get("doi").and_then(Value::as_str).map(ToOwned::to_owned);
            let topic=work.pointer("/primary_topic/display_name").and_then(Value::as_str).unwrap_or("");
            let cited=work.get("cited_by_count").and_then(Value::as_i64).unwrap_or(0);
            let summary=format!("Primary topic: {topic}. OpenAlex cited-by count: {cited}.");
            let mut institutions=BTreeMap::<String,String>::new();
            if let Some(authorships)=work.get("authorships").and_then(Value::as_array){
                for auth in authorships { if let Some(insts)=auth.get("institutions").and_then(Value::as_array){for inst in insts{
                    if let Some(name)=inst.get("display_name").and_then(Value::as_str){let iid=inst.get("id").and_then(Value::as_str).unwrap_or(name);institutions.entry(iid.to_string()).or_insert_with(||name.to_string());}
                }}}
            }
            for (_iid,org) in institutions {
                let key=self.organization_key(&org); if key.is_empty(){continue;}
                out.push(make_asset(&key,&org,"openalex","publication",id,title,&summary,doi.clone().or_else(||Some(id.to_string())),year,None,Some(spec.dimension_id.clone()),work.clone()));
            }
        }
        Ok(out)
    }

    async fn web_enrich_many(&self,profile:&CompetitiveProfile,candidates:&[(String,String)],ip:bool)->(Vec<PublicAsset>,Vec<String>){
        if !self.research.search_available() {
            return (Vec::new(),vec!["BRAVE_SEARCH_API_KEY is not configured; public IP/technology web enrichment was skipped".into()]);
        }
        let concurrency=self.config.limits.web_enrichment_concurrency.clamp(1,16);
        let jobs=candidates.iter().cloned().map(|(key,name)| async move {
            let result=self.web_enrich(profile,&key,&name,ip).await;
            (name,result)
        });
        let results=stream::iter(jobs).buffer_unordered(concurrency).collect::<Vec<_>>().await;
        let mut assets=Vec::new(); let mut errors=Vec::new();
        for (name,result) in results {match result{Ok(mut x)=>assets.append(&mut x),Err(e)=>errors.push(format!("{name}: {e}"))}}
        (assets,errors)
    }

    async fn web_enrich(&self,profile:&CompetitiveProfile,candidate_key:&str,candidate_name:&str,ip:bool)->Result<Vec<PublicAsset>>{
        let terms=if ip{&profile.ip_terms}else{&profile.technology_terms};
        let term_text=terms.iter().take(6).map(|x|format!("\"{}\"",x.replace('"',""))).collect::<Vec<_>>().join(" OR ");
        let query=if ip {format!("\"{}\" (patent OR patents OR intellectual property OR licensing) {}",candidate_name,term_text)} else {format!("\"{}\" (technology OR platform OR translational OR licensing OR partnership) {}",candidate_name,term_text)};
        let domains=if ip{&self.config.ip_search_domains}else{&self.config.technology_search_domains};
        let count=if ip{self.config.limits.ip_web_results_per_candidate}else{self.config.limits.technology_web_results_per_candidate};
        let hits=self.research.search(&query,domains,count).await?;
        let kind=if ip{"patent_ip"}else{"technology"};
        let provider=if ip{"ip_web"}else{"technology_web"};
        Ok(hits.into_iter().enumerate().map(|(i,h)|{
            let external=sha256_hex(format!("{}|{}",h.url,h.title).as_bytes());
            let association=if ip{"public_ip_search_signal_not_verified_ownership"}else{"public_technology_search_signal"};
            make_asset(candidate_key,candidate_name,provider,kind,&external,&h.title,&h.snippet,Some(h.url),None,None,None,json!({"search_query":query,"rank":i+1,"source":h.source,"association":association}))
        }).collect())
    }

    async fn score_assets(&self,profile:&CompetitiveProfile,assets:&mut [PublicAsset])->Result<()> {
        if assets.is_empty(){return Ok(());}
        let target=profile_search_text(profile);
        let mut q=self.embedding.embed_query(&target).await?;
        let query_dims=q.len();
        hpc::normalize_rows(&mut q,1,query_dims);
        let texts=assets.iter().map(asset_embedding_text).collect::<Vec<_>>();
        let vectors=self.embedding.embed_documents(&texts).await?;
        let dims=vectors.first().context("embedding endpoint returned zero public asset vectors")?.len();
        if q.len()!=dims{bail!("competitive embedding dimension mismatch: query {} vs assets {}",q.len(),dims);}
        if vectors.iter().any(|v|v.len()!=dims){bail!("competitive asset embedding dimensions are inconsistent");}
        let mut matrix=vectors.into_iter().flatten().collect::<Vec<f32>>();
        hpc::normalize_rows(&mut matrix,assets.len(),dims);
        let scores=hpc::scores(&matrix,&q,assets.len(),dims);
        for (asset,score) in assets.iter_mut().zip(scores){
            let semantic=((score+1.0)*0.5).clamp(0.0,1.0);
            let reliability=self.config.scoring.provider_reliability.get(&asset.provider).copied().unwrap_or(0.5).clamp(0.0,1.0);
            asset.relevance=(semantic*reliability).clamp(0.0,1.0);
        }
        Ok(())
    }

    async fn synthesize_strategy(&self,profile:&CompetitiveProfile,candidates:&[CandidateScore],assets:&[PublicAsset],own_context:&str)->Result<(CompetitiveStrategy,String)> {
        let selected=candidates.iter().take(self.config.limits.strategy_candidates).cloned().collect::<Vec<_>>();
        let selected_keys=selected.iter().map(|x|x.candidate_key.as_str()).collect::<BTreeSet<_>>();
        let mut chosen_assets=Vec::new();
        for c in &selected {
            let mut a=assets.iter().filter(|x|x.candidate_key==c.candidate_key).cloned().collect::<Vec<_>>();
            a.sort_by(|x,y|y.relevance.total_cmp(&x.relevance)); a.truncate(self.config.limits.strategy_assets_per_candidate); chosen_assets.extend(a);
        }
        let packet=json!({"profile":profile,"potential_competitors":selected,"public_assets":chosen_assets});
        let mut packet_text=serde_json::to_string_pretty(&packet)?;
        if packet_text.len()>self.config.limits.strategy_max_chars{packet_text.truncate(self.config.limits.strategy_max_chars);}
        let prompt=format!(r#"Develop an evidence-bounded grant positioning strategy from public competitive intelligence. The listed organizations are POTENTIAL capability-matched competitors, not confirmed applicants. Never state or imply that they will apply. Never invent confidential information, unpublished IP, private technology, unpublished trial data, or applicant intent.

Return STRICT JSON only:
{{"strategy":{{
  "market_summary":"what the public competitive landscape appears to contain",
  "positioning_principles":["principles for writing a stronger proposal"],
  "differentiators":[{{"theme":"theme","our_advantage":"how our proposal can defensibly distinguish itself using supplied project facts","public_competitor_signal":"what the cited public assets show","candidate_keys":["candidate-key"],"asset_keys":["asset-key"],"confidence":"high|medium|low","section_targets":["Specific Aims","Significance"]}}],
  "gaps_to_close":[{{"gap":"capability/evidence gap","why_it_matters":"why competitors appear stronger here","recommended_action":"specific action before submission","candidate_keys":["candidate-key"],"asset_keys":["asset-key"]}}],
  "do_not_claim":["claims the proposal should avoid because public evidence does not support them"],
  "candidate_notes":[{{"candidate_key":"candidate-key","why_relevant":"public capability overlap","how_to_outposition":"specific proposal positioning","asset_keys":["asset-key"]}}],
  "section_guidance":[{{"section_key":"specific_aims|significance|innovation|approach|human_subjects|environment","guidance":"how this section should express defensible differentiation without naming competitors unless explicitly appropriate","asset_keys":["asset-key"]}}]
}}}}
Rules:
- Every competitor-specific factual statement must be traceable to supplied asset_keys.
- Patent/IP web-search assets are public discovery signals, not proof that the candidate owns or exclusively controls the referenced IP. Phrase ownership/licensing claims only when the supplied public evidence explicitly establishes them.
- Use our project context to identify strengths, but do not invent our institutional capabilities either.
- Favor concrete superiority/differentiation dimensions: evidence depth, clinical access, recruitment feasibility, unique datasets, platform maturity, IP position, translational infrastructure, implementation readiness, partnerships, and measurable outcomes only when present in the supplied data.
- Translate competitor observations into proposal strengths; normally do not name potential competitors in grant prose.

OUR PROJECT CONTEXT:
{}

PUBLIC COMPETITIVE INTELLIGENCE:
{}"#,own_context,packet_text);
        let out=self.router.generate(ModelTask{kind:"competitive_positioning".into(),prompt,high_value:true}).await?;
        let mut env:CompetitiveStrategyEnvelope=parse_json_from_model(&out.text)?;
        validate_strategy(&mut env.strategy,&selected_keys,&chosen_assets)?;
        Ok((env.strategy,out.model))
    }

    fn organization_key(&self,name:&str)->String{normalize_organization(name,&self.config.organization_suffixes)}
    fn is_home_organization(&self,name:&str)->bool{
        let key=self.organization_key(name); if key.len()<5{return false;}
        self.home_aliases.iter().any(|a|{let x=self.organization_key(a); x.len()>=5 && (key==x || key.contains(&x) || x.contains(&key))})
    }
}

fn validate_config(c:&mut CompetitiveConfig)->Result<()> {
    if c.schema_version==0{bail!("competitive intelligence config schema_version must be positive");}
    if c.limits.max_candidates==0 || c.limits.max_assets_total==0 || c.limits.max_assets_per_candidate==0 || c.limits.web_enrichment_concurrency==0{bail!("competitive intelligence limits must be positive");}
    if c.scoring.top_assets_per_type==0 || c.scoring.count_saturation<=0.0{bail!("competitive scoring configuration is invalid");}
    if c.scoring.asset_type_weights.values().any(|x|*x<0.0){bail!("competitive asset weights must be non-negative");}
    if c.scoring.provider_reliability.values().any(|x|*x<0.0||*x>1.0){bail!("competitive provider reliability values must be between 0 and 1");}
    if c.updates.max_sections_per_refresh==0{bail!("competitive updates max_sections_per_refresh must be positive");}
    if !c.updates.candidate_score_delta.is_finite() || c.updates.candidate_score_delta<0.0 || c.updates.candidate_score_delta>1.0{bail!("competitive updates candidate_score_delta must be between 0 and 1");}
    for u in [&c.endpoints.nih_reporter_projects,&c.endpoints.clinical_trials_studies,&c.endpoints.openalex_works]{let parsed=reqwest::Url::parse(u)?;if parsed.scheme()!="https"{bail!("competitive public API endpoint must use https: {u}");}}
    Ok(())
}

fn validate_profile(p:&mut CompetitiveProfile,max_queries:usize)->Result<()> {
    if p.summary.trim().is_empty(){bail!("competitive profile summary is empty");}
    if p.capability_dimensions.is_empty(){bail!("competitive profile contains no capability dimensions");}
    let mut ids=BTreeSet::new(); let mut sum=0.0f32;
    for d in &mut p.capability_dimensions {
        d.id=d.id.trim().to_string(); d.label=d.label.trim().to_string(); d.description=d.description.trim().to_string();
        if d.id.is_empty()||d.label.is_empty()||d.description.is_empty()||d.weight<=0.0{bail!("competitive capability dimensions require id, label, description and positive weight");}
        if !ids.insert(d.id.clone()){bail!("duplicate competitive capability dimension id {}",d.id);}
        sum+=d.weight;
    }
    for d in &mut p.capability_dimensions{d.weight/=sum.max(f32::EPSILON);}
    let allowed=BTreeSet::from(["nih_grants","publications","clinical_trials"]);
    p.search_queries.retain(|q|!q.query.trim().is_empty()&&ids.contains(&q.dimension_id));
    for q in &mut p.search_queries {q.source_types.retain(|x|allowed.contains(x.as_str())); q.source_types.sort();q.source_types.dedup();}
    p.search_queries.retain(|q|!q.source_types.is_empty());
    p.search_queries.truncate(max_queries);
    if p.search_queries.is_empty(){bail!("competitive profile contains no valid public search queries");}
    Ok(())
}

fn validate_strategy(s:&mut CompetitiveStrategy,candidates:&BTreeSet<&str>,assets:&[PublicAsset])->Result<()> {
    let asset_keys=assets.iter().map(|x|x.asset_key.as_str()).collect::<BTreeSet<_>>();
    let valid_conf=BTreeSet::from(["high","medium","low"]);
    for d in &mut s.differentiators {
        d.candidate_keys.retain(|x|candidates.contains(x.as_str())); d.asset_keys.retain(|x|asset_keys.contains(x.as_str())); d.candidate_keys.sort();d.candidate_keys.dedup();d.asset_keys.sort();d.asset_keys.dedup();
        if !valid_conf.contains(d.confidence.as_str()){d.confidence="low".into();}
    }
    s.differentiators.retain(|d|d.candidate_keys.is_empty() || !d.asset_keys.is_empty());
    for g in &mut s.gaps_to_close {g.candidate_keys.retain(|x|candidates.contains(x.as_str()));g.asset_keys.retain(|x|asset_keys.contains(x.as_str()));g.candidate_keys.sort();g.candidate_keys.dedup();g.asset_keys.sort();g.asset_keys.dedup();}
    s.gaps_to_close.retain(|g|g.candidate_keys.is_empty() || !g.asset_keys.is_empty());
    s.candidate_notes.retain(|n|candidates.contains(n.candidate_key.as_str())); for n in &mut s.candidate_notes{n.asset_keys.retain(|x|asset_keys.contains(x.as_str()));n.asset_keys.sort();n.asset_keys.dedup();}
    s.candidate_notes.retain(|n|!n.asset_keys.is_empty());
    for g in &mut s.section_guidance{g.asset_keys.retain(|x|asset_keys.contains(x.as_str()));g.asset_keys.sort();g.asset_keys.dedup();}
    Ok(())
}

fn make_asset(candidate_key:&str,candidate_name:&str,provider:&str,asset_type:&str,external_id:&str,title:&str,summary:&str,url:Option<String>,year:Option<i32>,amount:Option<f64>,dimension_id:Option<String>,metadata:Value)->PublicAsset{
    let asset_key=sha256_hex(format!("{provider}|{asset_type}|{external_id}|{candidate_key}").as_bytes());
    PublicAsset{asset_key,candidate_key:candidate_key.to_string(),candidate_name:candidate_name.trim().to_string(),provider:provider.into(),asset_type:asset_type.into(),external_id:external_id.into(),title:title.trim().to_string(),summary:summary.trim().to_string(),url,year,amount,dimension_id,metadata,relevance:0.0}
}
fn status(provider:&str,records:usize,errors:Vec<String>)->ProviderStatus{ProviderStatus{provider:provider.into(),ok:errors.is_empty(),records,detail:if errors.is_empty(){"ok".into()}else{errors.into_iter().take(5).collect::<Vec<_>>().join(" | ")}}}
fn value_string(v:&Value)->String{match v{Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),_=>String::new()}}
fn sha256_hex(bytes:&[u8])->String{let mut h=Sha256::new();h.update(bytes);hex::encode(h.finalize())}

pub fn normalize_organization(name:&str,suffixes:&[String])->String{
    let suffix=suffixes.iter().map(|x|x.to_ascii_uppercase()).collect::<BTreeSet<_>>();
    let cleaned=name.chars().map(|c|if c.is_ascii_alphanumeric(){c.to_ascii_uppercase()}else{' '}).collect::<String>();
    let mut tokens=cleaned.split_whitespace().map(ToOwned::to_owned).collect::<Vec<_>>();
    while tokens.last().is_some_and(|x|suffix.contains(x)){tokens.pop();}
    tokens.join(" ")
}
fn dedup_assets(assets:&mut Vec<PublicAsset>){let mut seen=BTreeSet::new();assets.retain(|a|seen.insert((a.asset_key.clone(),a.candidate_key.clone())));}
fn preselect_candidates(assets:&[PublicAsset],limit:usize)->BTreeSet<String>{
    #[derive(Default)] struct Pre{count:usize,types:BTreeSet<String>,dimensions:BTreeSet<String>}
    let mut by=HashMap::<String,Pre>::new();
    for a in assets{let e=by.entry(a.candidate_key.clone()).or_default();e.count+=1;e.types.insert(a.asset_type.clone());if let Some(d)=a.dimension_id.as_ref(){e.dimensions.insert(d.clone());}}
    let mut x=by.into_iter().collect::<Vec<_>>();
    x.sort_by(|a,b|b.1.dimensions.len().cmp(&a.1.dimensions.len()).then_with(||b.1.types.len().cmp(&a.1.types.len())).then_with(||b.1.count.cmp(&a.1.count)).then_with(||a.0.cmp(&b.0)));
    x.truncate(limit);x.into_iter().map(|x|x.0).collect()
}
fn cap_assets(assets:&mut Vec<PublicAsset>,per_candidate:usize,total:usize){
    let input=std::mem::take(assets);
    let mut by_candidate=BTreeMap::<String,BTreeMap<String,VecDeque<PublicAsset>>>::new();
    for a in input{by_candidate.entry(a.candidate_key.clone()).or_default().entry(a.asset_type.clone()).or_default().push_back(a);}
    let mut balanced=BTreeMap::<String,VecDeque<PublicAsset>>::new();
    for (candidate,mut types) in by_candidate {
        let mut keep=VecDeque::new();
        while keep.len()<per_candidate {
            let mut added=false;
            for queue in types.values_mut(){if keep.len()>=per_candidate{break;}if let Some(a)=queue.pop_front(){keep.push_back(a);added=true;}}
            if !added{break;}
        }
        balanced.insert(candidate,keep);
    }
    while assets.len()<total {
        let mut added=false;
        for queue in balanced.values_mut(){if assets.len()>=total{break;}if let Some(a)=queue.pop_front(){assets.push(a);added=true;}}
        if !added{break;}
    }
}
fn profile_search_text(p:&CompetitiveProfile)->String{format!("{}\nApplicant types: {}\nCapabilities: {}\nDisease: {}\nTechnology: {}\nClinical: {}\nIP: {}\nGrant/program terms: {}",p.summary,p.likely_applicant_types.join(", "),p.capability_dimensions.iter().map(|d|format!("{}: {}",d.label,d.description)).collect::<Vec<_>>().join("; "),p.disease_terms.join(", "),p.technology_terms.join(", "),p.clinical_terms.join(", "),p.ip_terms.join(", "),p.grant_terms.join(", "))}
fn asset_embedding_text(a:&PublicAsset)->String{format!("Organization: {}\nAsset type: {}\nTitle: {}\nSummary: {}",a.candidate_name,a.asset_type,a.title,a.summary)}

fn aggregate_candidates(profile:&CompetitiveProfile,assets:&[PublicAsset],cfg:&ScoringConfig,limit:usize)->Vec<CandidateScore>{
    let mut by=HashMap::<String,Vec<&PublicAsset>>::new();for a in assets{by.entry(a.candidate_key.clone()).or_default().push(a);}
    let mut out=Vec::new();
    for (key,aa) in by {
        let name=aa.first().map(|x|x.candidate_name.clone()).unwrap_or_else(||key.clone());
        let mut type_scores=BTreeMap::<String,f32>::new(); let mut counts=BTreeMap::<String,usize>::new();
        for typ in cfg.asset_type_weights.keys(){let mut vals=aa.iter().filter(|a|&a.asset_type==typ).map(|a|a.relevance).collect::<Vec<_>>();vals.sort_by(|a,b|b.total_cmp(a));let count=vals.len();counts.insert(typ.clone(),count);let top=vals.iter().take(cfg.top_assets_per_type).copied().collect::<Vec<_>>();let mean=if top.is_empty(){0.0}else{top.iter().sum::<f32>()/(top.len() as f32)};let sat=1.0-(-(count as f32)/cfg.count_saturation).exp();type_scores.insert(typ.clone(),(mean*(0.75+0.25*sat)).clamp(0.0,1.0));}
        let weight_sum=cfg.asset_type_weights.values().copied().sum::<f32>().max(f32::EPSILON);let base=cfg.asset_type_weights.iter().map(|(t,w)|w*type_scores.get(t).copied().unwrap_or(0.0)).sum::<f32>()/weight_sum;
        let active=cfg.asset_type_weights.keys().filter(|t|counts.get(*t).copied().unwrap_or(0)>0).count();let breadth=(active as f32)/(cfg.asset_type_weights.len().max(1) as f32);let bw=cfg.breadth_weight.clamp(0.0,0.5);let overall=((1.0-bw)*base+bw*breadth).clamp(0.0,1.0);
        let dimensions=profile.capability_dimensions.iter().map(|d|{let mut x=aa.iter().filter(|a|a.dimension_id.as_deref()==Some(d.id.as_str())).collect::<Vec<_>>();x.sort_by(|a,b|b.relevance.total_cmp(&a.relevance));DimensionCoverage{dimension_id:d.id.clone(),label:d.label.clone(),score:x.first().map(|a|a.relevance).unwrap_or(0.0),supporting_asset_keys:x.into_iter().take(3).map(|a|a.asset_key.clone()).collect()}}).collect();
        out.push(CandidateScore{candidate_key:key,name,overall_score:overall,grant_score:type_scores.get("grant").copied().unwrap_or(0.0),publication_score:type_scores.get("publication").copied().unwrap_or(0.0),clinical_trial_score:type_scores.get("clinical_trial").copied().unwrap_or(0.0),patent_ip_score:type_scores.get("patent_ip").copied().unwrap_or(0.0),technology_score:type_scores.get("technology").copied().unwrap_or(0.0),breadth_score:breadth,asset_count:aa.len(),asset_counts:counts,dimension_coverage:dimensions});
    }
    out.sort_by(|a,b|b.overall_score.total_cmp(&a.overall_score).then_with(||a.name.cmp(&b.name)));out.truncate(limit);out
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test] fn organization_normalization(){let s=vec!["INC".into(),"LLC".into()];assert_eq!(normalize_organization("Acme Bio, Inc.",&s),"ACME BIO");assert_eq!(normalize_organization("University of Example",&s),"UNIVERSITY OF EXAMPLE");}
    #[test] fn profile_validation_normalizes_weights(){let mut p=CompetitiveProfile{summary:"x".into(),likely_applicant_types:vec![],capability_dimensions:vec![CapabilityDimension{id:"A".into(),label:"A".into(),description:"a".into(),weight:2.0},CapabilityDimension{id:"B".into(),label:"B".into(),description:"b".into(),weight:1.0}],disease_terms:vec![],technology_terms:vec![],clinical_terms:vec![],ip_terms:vec![],grant_terms:vec![],search_queries:vec![CompetitiveSearchSpec{dimension_id:"A".into(),query:"query".into(),source_types:vec!["nih_grants".into()]}]};validate_profile(&mut p,12).unwrap();assert!((p.capability_dimensions.iter().map(|x|x.weight).sum::<f32>()-1.0).abs()<1e-6);}
    #[test] fn asset_cap_preserves_type_breadth(){
        let mut a=Vec::new();
        for i in 0..10{a.push(make_asset("ORG","Org","nih_reporter","grant",&format!("g{i}"),"g","",None,None,None,None,serde_json::json!({})));}
        a.push(make_asset("ORG","Org","ip_web","patent_ip","p1","p","",None,None,None,None,serde_json::json!({})));
        a.push(make_asset("ORG","Org","technology_web","technology","t1","t","",None,None,None,None,serde_json::json!({})));
        cap_assets(&mut a,4,4);
        let types=a.iter().map(|x|x.asset_type.as_str()).collect::<BTreeSet<_>>();
        assert!(types.contains("grant")&&types.contains("patent_ip")&&types.contains("technology"));
    }
}

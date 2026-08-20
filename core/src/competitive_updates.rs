use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateScoreChange {
    pub candidate_key: String,
    pub name: String,
    pub previous_score: f64,
    pub current_score: f64,
    pub delta: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitiveDelta {
    pub from_run_id: Option<i64>,
    pub to_run_id: i64,
    pub material: bool,
    pub public_data_changed: bool,
    pub provider_degraded: bool,
    pub strategy_changed: bool,
    pub broad_strategy_change: bool,
    pub changed_section_keys: Vec<String>,
    pub new_candidates: Vec<String>,
    pub removed_candidates: Vec<String>,
    pub score_changes: Vec<CandidateScoreChange>,
    pub new_asset_keys: Vec<String>,
    pub removed_asset_keys: Vec<String>,
    pub summary: String,
}

fn key(value:&str)->String {
    let mut out=String::with_capacity(value.len());
    let mut sep=false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric(){out.push(ch.to_ascii_lowercase());sep=false;}
        else if !out.is_empty() && !sep {out.push('_');sep=true;}
    }
    out.trim_matches('_').to_string()
}

fn candidate_map(v:&Value)->BTreeMap<String,(String,f64)> {
    v.get("candidates").and_then(Value::as_array).into_iter().flatten().filter_map(|c|{
        let k=c.get("candidate_key")?.as_str()?.to_string();
        let name=c.get("name").and_then(Value::as_str).unwrap_or(&k).to_string();
        let score=c.get("overall_score").and_then(Value::as_f64).unwrap_or(0.0);
        Some((k,(name,score)))
    }).collect()
}

fn asset_map(v:&Value)->BTreeMap<String,String> {
    v.get("assets").and_then(Value::as_array).into_iter().flatten().filter_map(|a|{
        let k=a.get("asset_key")?.as_str()?.to_string();
        let provider=a.get("provider").and_then(Value::as_str).unwrap_or("").to_string();
        Some((k,provider))
    }).collect()
}

fn provider_health(v:&Value)->BTreeMap<String,bool> {
    v.get("provider_status").and_then(Value::as_array).into_iter().flatten().filter_map(|p|{
        Some((p.get("provider")?.as_str()?.to_string(),p.get("ok").and_then(Value::as_bool).unwrap_or(false)))
    }).collect()
}

fn strategy_section_guidance(v:&Value)->BTreeMap<String,String> {
    let mut out=BTreeMap::new();
    for x in v.get("strategy").and_then(|s|s.get("section_guidance")).and_then(Value::as_array).into_iter().flatten() {
        let section=key(x.get("section_key").and_then(Value::as_str).unwrap_or(""));
        let guidance=x.get("guidance").and_then(Value::as_str).unwrap_or("").trim().to_string();
        if !section.is_empty(){out.insert(section,guidance);}
    }
    out
}

fn normalized_strategy_component(v:&Value,name:&str)->String {
    v.get("strategy").and_then(|s|s.get(name)).map(|x|serde_json::to_string(x).unwrap_or_default()).unwrap_or_default()
}

pub fn diff(previous:&Value,current:&Value,candidate_score_delta:f32)->CompetitiveDelta {
    let old_run=previous.get("run_id").and_then(Value::as_i64);
    let new_run=current.get("run_id").and_then(Value::as_i64).unwrap_or(0);
    let old_candidates=candidate_map(previous);
    let new_candidates_map=candidate_map(current);
    let old_keys=old_candidates.keys().cloned().collect::<BTreeSet<_>>();
    let new_keys=new_candidates_map.keys().cloned().collect::<BTreeSet<_>>();
    let added=new_keys.difference(&old_keys).cloned().collect::<Vec<_>>();

    // Missing results from a degraded public provider are not evidence that a competitor,
    // patent, award, or trial disappeared. Suppress destructive deltas while any provider
    // is unhealthy so transient public-API failures do not rewrite grant prose.
    let current_health=provider_health(current);
    let provider_degraded=current_health.values().any(|ok|!*ok);
    let removed=if provider_degraded { Vec::new() } else { old_keys.difference(&new_keys).cloned().collect::<Vec<_>>() };

    let threshold=(candidate_score_delta as f64).abs();
    let mut score_changes=Vec::new();
    for k in old_keys.intersection(&new_keys) {
        let (name_old,a)=&old_candidates[k];
        let (name_new,b)=&new_candidates_map[k];
        let d=*b-*a;
        // Downward score shifts can be artifacts of an unhealthy provider; new/upward
        // evidence remains actionable because it is actually present in the current run.
        if d.abs()>=threshold && (!provider_degraded || d>0.0) {
            score_changes.push(CandidateScoreChange{candidate_key:k.clone(),name:if name_new.is_empty(){name_old.clone()}else{name_new.clone()},previous_score:*a,current_score:*b,delta:d});
        }
    }
    score_changes.sort_by(|a,b|b.delta.abs().total_cmp(&a.delta.abs()).then_with(||a.name.cmp(&b.name)));

    let old_assets=asset_map(previous);
    let new_assets=asset_map(current);
    let old_asset_keys=old_assets.keys().cloned().collect::<BTreeSet<_>>();
    let new_asset_keys_set=new_assets.keys().cloned().collect::<BTreeSet<_>>();
    let new_asset_keys=new_asset_keys_set.difference(&old_asset_keys).cloned().collect::<Vec<_>>();
    let removed_asset_keys=if provider_degraded {
        Vec::new()
    } else {
        old_asset_keys.difference(&new_asset_keys_set).cloned().collect::<Vec<_>>()
    };

    let old_guidance=strategy_section_guidance(previous);
    let new_guidance=strategy_section_guidance(current);
    let guidance_keys=old_guidance.keys().chain(new_guidance.keys()).cloned().collect::<BTreeSet<_>>();
    let mut changed_section_keys=guidance_keys.into_iter().filter(|k|old_guidance.get(k)!=new_guidance.get(k)).collect::<Vec<_>>();
    changed_section_keys.sort();
    changed_section_keys.dedup();

    let broad_components=["market_summary","positioning_principles","differentiators","gaps_to_close","candidate_notes","do_not_claim"];
    let broad_strategy_change=broad_components.iter().any(|name|normalized_strategy_component(previous,name)!=normalized_strategy_component(current,name));
    let strategy_changed=previous.get("strategy_sha256")!=current.get("strategy_sha256") || broad_strategy_change || !changed_section_keys.is_empty();

    // LLM strategy wording can vary even when the underlying public data is identical.
    // Only observable public-intelligence changes trigger automatic section rewrites.
    // Strategy deltas are still recorded for audit/UI context and are acted upon when
    // the public data changed materially in the same refresh.
    let public_data_changed=!added.is_empty() || !removed.is_empty() || !score_changes.is_empty() || !new_asset_keys.is_empty() || !removed_asset_keys.is_empty();
    let material=old_run.is_none() || public_data_changed;

    let summary=if old_run.is_none(){
        format!("Initial public competitive intelligence established with {} capability-matched organizations and {} public assets.",new_candidates_map.len(),new_assets.len())
    } else {
        let mut parts=Vec::new();
        if !added.is_empty(){parts.push(format!("{} new capability-matched organization(s)",added.len()));}
        if !removed.is_empty(){parts.push(format!("{} organization(s) no longer ranked",removed.len()));}
        if !new_asset_keys.is_empty(){parts.push(format!("{} new public competitive asset(s)",new_asset_keys.len()));}
        if !removed_asset_keys.is_empty(){parts.push(format!("{} previously observed asset(s) no longer returned",removed_asset_keys.len()));}
        if !score_changes.is_empty(){parts.push(format!("{} material competitor score change(s)",score_changes.len()));}
        if strategy_changed && public_data_changed{parts.push("positioning strategy changed".into());}
        if provider_degraded{parts.push("one or more public providers were degraded; missing/downward signals were ignored".into());}
        if parts.is_empty(){
            if strategy_changed{"No new public competitor evidence was detected; strategy wording changed but no grant text was auto-revised.".into()}
            else{"No material competitive change detected.".into()}
        }else{format!("Competitive intelligence refresh detected {}.",parts.join(", "))}
    };

    CompetitiveDelta{
        from_run_id:old_run,to_run_id:new_run,material,public_data_changed,provider_degraded,
        strategy_changed,broad_strategy_change,changed_section_keys,new_candidates:added,
        removed_candidates:removed,score_changes,new_asset_keys,removed_asset_keys,summary
    }
}

#[cfg(test)]
mod tests{
    use super::*;

    #[test]
    fn detects_new_candidate_asset_and_guidance(){
        let a=serde_json::json!({"run_id":1,"strategy_sha256":"a","provider_status":[{"provider":"openalex","ok":true}],"candidates":[{"candidate_key":"A","name":"A","overall_score":0.5}],"assets":[{"asset_key":"x","provider":"openalex"}],"strategy":{"section_guidance":[{"section_key":"Significance","guidance":"old"}]}});
        let b=serde_json::json!({"run_id":2,"strategy_sha256":"b","provider_status":[{"provider":"openalex","ok":true}],"candidates":[{"candidate_key":"A","name":"A","overall_score":0.7},{"candidate_key":"B","name":"B","overall_score":0.4}],"assets":[{"asset_key":"x","provider":"openalex"},{"asset_key":"y","provider":"openalex"}],"strategy":{"section_guidance":[{"section_key":"Significance","guidance":"new"}]}});
        let d=diff(&a,&b,0.05);
        assert!(d.material);
        assert!(d.public_data_changed);
        assert_eq!(d.new_candidates,vec!["B"]);
        assert_eq!(d.new_asset_keys,vec!["y"]);
        assert!(d.changed_section_keys.contains(&"significance".to_string()));
        assert_eq!(d.score_changes.len(),1);
    }

    #[test]
    fn strategy_wording_alone_does_not_trigger_rewrite(){
        let a=serde_json::json!({"run_id":1,"strategy_sha256":"a","provider_status":[{"provider":"nih_reporter","ok":true}],"candidates":[{"candidate_key":"A","name":"A","overall_score":0.5}],"assets":[{"asset_key":"x","provider":"nih_reporter"}],"strategy":{"market_summary":"old"}});
        let b=serde_json::json!({"run_id":2,"strategy_sha256":"b","provider_status":[{"provider":"nih_reporter","ok":true}],"candidates":[{"candidate_key":"A","name":"A","overall_score":0.5}],"assets":[{"asset_key":"x","provider":"nih_reporter"}],"strategy":{"market_summary":"new wording"}});
        let d=diff(&a,&b,0.05);
        assert!(d.strategy_changed);
        assert!(!d.public_data_changed);
        assert!(!d.material);
    }

    #[test]
    fn degraded_provider_does_not_treat_missing_data_as_competitive_change(){
        let a=serde_json::json!({"run_id":1,"strategy_sha256":"a","provider_status":[{"provider":"openalex","ok":true}],"candidates":[{"candidate_key":"A","name":"A","overall_score":0.8}],"assets":[{"asset_key":"x","provider":"openalex"}],"strategy":{}});
        let b=serde_json::json!({"run_id":2,"strategy_sha256":"b","provider_status":[{"provider":"openalex","ok":false}],"candidates":[],"assets":[],"strategy":{}});
        let d=diff(&a,&b,0.05);
        assert!(d.provider_degraded);
        assert!(d.removed_candidates.is_empty());
        assert!(d.removed_asset_keys.is_empty());
        assert!(!d.public_data_changed);
        assert!(!d.material);
    }
}

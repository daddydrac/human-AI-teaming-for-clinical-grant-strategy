use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use time::OffsetDateTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceRule {
    pub rule_id: String,
    pub category: String,
    pub rule_type: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub mandatory: bool,
    #[serde(default)]
    pub numeric_value: Option<f64>,
    #[serde(default)]
    pub text_value: Option<String>,
    #[serde(default)]
    pub list_value: Vec<String>,
    #[serde(default)]
    pub source_excerpt: String,
    #[serde(default)]
    pub source_locator: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceProfile {
    #[serde(default)]
    pub sponsor: Option<String>,
    #[serde(default)]
    pub mechanism: Option<String>,
    #[serde(default)]
    pub submission_system: Option<String>,
    #[serde(default)]
    pub deadline_iso: Option<String>,
    #[serde(default)]
    pub rules: Vec<ComplianceRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceEnvelope {
    pub profile: ComplianceProfile,
}

#[derive(Debug, Clone)]
pub struct ComplianceFacts {
    pub approved_sections: Vec<(String, String, String)>, // key,title,body
    pub artifacts: Vec<(String, String, String)>,          // slot,filename,extension
    pub design_profile: Value,
    pub measurements: Option<Value>,
    pub project_period_months: Option<f64>,
}

fn norm(v: &str) -> String {
    v.trim().to_ascii_lowercase().replace([' ', '-'], "_")
}

pub fn validate_profile(profile: &ComplianceProfile) -> Result<()> {
    if profile.rules.is_empty() { bail!("compliance profile contains no rules"); }
    let allowed = [
        "required_section", "required_form", "max_words", "min_words", "required_attachment",
        "allowed_extensions", "min_font_size_pt", "min_margin_in", "max_pages",
        "deadline", "required_letter_count", "manual_requirement", "submission_system",
        "max_budget", "project_period_max_months",
    ];
    let mut ids = std::collections::HashSet::new();
    for r in &profile.rules {
        if r.rule_id.trim().is_empty() { bail!("compliance rule ID cannot be empty"); }
        if !ids.insert(r.rule_id.clone()) { bail!("duplicate compliance rule ID {}", r.rule_id); }
        if !allowed.contains(&r.rule_type.as_str()) { bail!("unsupported compliance rule type {}", r.rule_type); }
        if !matches!(r.severity.as_str(), "hard"|"warning"|"info") { bail!("invalid compliance severity {}", r.severity); }
        if r.source_excerpt.trim().is_empty() { bail!("rule {} is missing source_excerpt", r.rule_id); }
    }
    Ok(())
}


pub fn validate_source_excerpts(profile: &ComplianceProfile, source: &str) -> Result<()> {
    let normalized_source = source.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized_source.trim().is_empty() { bail!("funding opportunity source is empty"); }
    for rule in &profile.rules {
        let excerpt = rule.source_excerpt.split_whitespace().collect::<Vec<_>>().join(" ");
        if excerpt.is_empty() || !normalized_source.contains(&excerpt) {
            bail!("rule {} source_excerpt cannot be verified verbatim in the funding opportunity", rule.rule_id);
        }
    }
    Ok(())
}

fn word_count(s: &str) -> usize { s.split_whitespace().count() }
fn num(v: &Value, key: &str) -> Option<f64> { v.get(key).and_then(Value::as_f64) }

fn today_ymd() -> (i32, u8, u8) {
    let d=OffsetDateTime::now_utc().date();
    (d.year(), d.month() as u8, d.day())
}
fn parse_ymd(s:&str)->Option<(i32,u8,u8)> {
    let mut p=s.split('-');
    Some((p.next()?.parse().ok()?,p.next()?.parse().ok()?,p.next()?.parse().ok()?))
}

pub fn evaluate(
    profile: &ComplianceProfile,
    facts: &ComplianceFacts,
    resolutions: &HashMap<String, (String,String)>,
) -> Value {
    let sections = facts.approved_sections.iter().map(|(k,t,b)|(norm(k),norm(t),b)).collect::<Vec<_>>();
    let artifacts = facts.artifacts.iter().map(|(s,n,e)|(norm(s),n.to_ascii_lowercase(),e.to_ascii_lowercase())).collect::<Vec<_>>();
    let mut findings=Vec::new();
    let mut hard_failures=0usize; let mut warnings=0usize; let mut passed=0usize; let mut deferred=0usize;

    for rule in &profile.rules {
        let resolution=resolutions.get(&rule.rule_id);
        // Human resolutions are for rules that are inherently non-deterministic.
        // Never let a stale checkbox suppress checks we can recompute from the
        // approved document, rendered measurements, or registered artifacts.
        let manual_rule=matches!(rule.rule_type.as_str(),"manual_requirement"|"max_budget");
        let manually_satisfied=manual_rule && resolution.map(|x|matches!(x.0.as_str(),"satisfied"|"not_applicable"|"waived")).unwrap_or(false);
        let mut status="pass".to_string(); let mut detail=String::new(); let mut observed=Value::Null;
        if manually_satisfied {
            detail=format!("Human resolution: {}{}",resolution.unwrap().0,if resolution.unwrap().1.is_empty(){""}else{" — "});
            if !resolution.unwrap().1.is_empty(){detail.push_str(&resolution.unwrap().1);}
        } else {
            match rule.rule_type.as_str() {
                "required_section" => {
                    let target=norm(&rule.target);
                    let ok=sections.iter().any(|(k,t,b)|(k==&target||t==&target)&&!b.trim().is_empty());
                    observed=json!({"approved":ok}); if !ok {status="fail".into();detail="Required section is not human-approved.".into();}
                },
                "max_words"|"min_words" => {
                    let target=norm(&rule.target); let expected=rule.numeric_value.unwrap_or(0.0).max(0.0) as usize;
                    let count=if target.is_empty()||target=="document"||target=="full_document" { sections.iter().map(|x|word_count(x.2)).sum() } else { sections.iter().find(|(k,t,_)|k==&target||t==&target).map(|x|word_count(x.2)).unwrap_or(0) };
                    observed=json!({"words":count,"limit":expected});
                    let ok=if rule.rule_type=="max_words"{count<=expected}else{count>=expected}; if !ok {status="fail".into();detail=format!("Observed {count} words; sponsor rule requires {} {expected} words.",if rule.rule_type=="max_words"{"at most"}else{"at least"});}
                },
                "required_attachment"|"required_form" => {
                    let target=norm(&rule.target); let ok=artifacts.iter().any(|(s,n,_)|s==&target||n.contains(&target.replace('_'," "))||n.contains(&target)); observed=json!({"registered":ok}); if !ok{status="fail".into();detail="Required submission artifact is not registered.".into();}
                },
                "allowed_extensions" => {
                    let target=norm(&rule.target); let allowed=rule.list_value.iter().map(|x|x.trim().trim_start_matches('.').to_ascii_lowercase()).collect::<Vec<_>>();
                    let matching=artifacts.iter().filter(|(s,_,_)|target.is_empty()||s==&target).collect::<Vec<_>>(); let bad=matching.iter().filter(|(_,_,e)|!allowed.iter().any(|a|a.as_str()==e.as_str())).map(|(_,n,_)|(*n).clone()).collect::<Vec<_>>(); observed=json!({"matching":matching.len(),"invalid":bad}); if !bad.is_empty(){status="fail".into();detail="One or more registered submission artifacts use a disallowed extension.".into();}
                },
                "min_font_size_pt" => {
                    let min=rule.numeric_value.unwrap_or(0.0); let actual=num(&facts.design_profile,"body_size_pt").unwrap_or(0.0); observed=json!({"body_size_pt":actual,"minimum":min}); if actual<min{status="fail".into();detail=format!("Body font is {actual} pt; minimum is {min} pt.");}
                },
                "min_margin_in" => {
                    let min=rule.numeric_value.unwrap_or(0.0); let keys=["margin_top_in","margin_right_in","margin_bottom_in","margin_left_in"]; let vals=keys.iter().map(|k|num(&facts.design_profile,k).unwrap_or(0.0)).collect::<Vec<_>>(); observed=json!({"margins_in":vals,"minimum":min}); if vals.iter().any(|v|*v<min){status="fail".into();detail=format!("At least one configured margin is smaller than {min} inches.");}
                },
                "max_pages" => {
                    let limit=rule.numeric_value.unwrap_or(0.0);
                    let target=norm(&rule.target);
                    let pages=if target.is_empty()||target=="document"||target=="full_document"||target=="proposal" {
                        facts.measurements.as_ref().and_then(|m|m.get("page_count")).and_then(Value::as_f64)
                    } else {
                        facts.measurements.as_ref().and_then(|m|m.get("sections")).and_then(|s|s.get(&target)).and_then(|s|s.get("pages")).and_then(Value::as_f64)
                    };
                    observed=json!({"page_count":pages,"limit":limit,"target":target}); match pages {Some(p) if p<=limit=>{},Some(p)=>{status="fail".into();detail=format!("Rendered target '{}' is {p} pages; maximum is {limit}.",rule.target);},None=>{status="deferred".into();detail=format!("Rendered page measurement for '{}' is required before export.",rule.target);}}
                },
                "deadline" => {
                    let value=rule.text_value.as_deref().or(profile.deadline_iso.as_deref()).unwrap_or(""); let parsed=parse_ymd(value); observed=json!({"deadline":value}); if let Some(d)=parsed {if today_ymd()>d{status="fail".into();detail="Sponsor deadline has passed in UTC date terms.".into();}} else {status="deferred".into();detail="Deadline could not be normalized to YYYY-MM-DD; human confirmation is required.".into();}
                },
                "required_letter_count" => {
                    let need=rule.numeric_value.unwrap_or(0.0).max(0.0) as usize; let target=norm(&rule.target); let count=artifacts.iter().filter(|(s,n,_)|s==&target||s.contains("letter")||n.contains("letter")).count(); observed=json!({"count":count,"required":need}); if count<need{status="fail".into();detail=format!("{count} matching letters registered; {need} required.");}
                },
                "submission_system" => { observed=json!({"submission_system":rule.text_value.clone().or(profile.submission_system.clone())}); },
                "project_period_max_months" => {
                    let limit=rule.numeric_value.unwrap_or(0.0);
                    let observed_months=facts.project_period_months;
                    observed=json!({"project_period_months":observed_months,"limit":limit});
                    match observed_months {
                        Some(v) if v<=limit=>{},
                        Some(v)=>{status="fail".into();detail=format!("Project period is {v} months; sponsor maximum is {limit} months.");},
                        None=>{status="deferred".into();detail="Project-period measurement is required before export.".into();}
                    }
                },
                "manual_requirement"|"max_budget" => {status="deferred".into();detail="This sponsor rule requires explicit human resolution because the current deterministic project model does not contain the authoritative field needed to prove it.".into();},
                _ => {status="deferred".into();detail="Unsupported rule requires human resolution.".into();}
            }
        }
        if status=="pass"{passed+=1;} else if status=="deferred"{deferred+=1;if rule.severity=="hard"&&rule.mandatory{hard_failures+=1;}} else if rule.severity=="hard"&&rule.mandatory{hard_failures+=1;} else {warnings+=1;}
        findings.push(json!({"rule_id":rule.rule_id,"category":rule.category,"rule_type":rule.rule_type,"target":rule.target,"severity":rule.severity,"mandatory":rule.mandatory,"status":status,"detail":detail,"observed":observed,"source_excerpt":rule.source_excerpt,"source_locator":rule.source_locator,"resolution":resolution.map(|x|json!({"status":x.0,"notes":x.1})).unwrap_or(Value::Null)}));
    }
    json!({"ready":hard_failures==0,"hard_failures":hard_failures,"warnings":warnings,"deferred":deferred,"passed":passed,"total":profile.rules.len(),"findings":findings})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn required_section_and_words_are_deterministic(){
        let p=ComplianceProfile{sponsor:None,mechanism:None,submission_system:None,deadline_iso:None,rules:vec![
            ComplianceRule{rule_id:"C1".into(),category:"section".into(),rule_type:"required_section".into(),scope:"proposal".into(),target:"specific_aims".into(),severity:"hard".into(),mandatory:true,numeric_value:None,text_value:None,list_value:vec![],source_excerpt:"Specific Aims required".into(),source_locator:"p1".into(),notes:String::new()},
            ComplianceRule{rule_id:"C2".into(),category:"format".into(),rule_type:"max_words".into(),scope:"section".into(),target:"specific_aims".into(),severity:"hard".into(),mandatory:true,numeric_value:Some(3.0),text_value:None,list_value:vec![],source_excerpt:"3 words".into(),source_locator:"p1".into(),notes:String::new()}
        ]};
        let f=ComplianceFacts{approved_sections:vec![("specific_aims".into(),"Specific Aims".into(),"one two three".into())],artifacts:vec![],design_profile:json!({}),measurements:None,project_period_months:None};
        assert!(evaluate(&p,&f,&HashMap::new()).get("ready").unwrap().as_bool().unwrap());
    }
}

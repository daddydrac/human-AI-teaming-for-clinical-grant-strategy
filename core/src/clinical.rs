use anyhow::{bail, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Population {
    #[serde(default)]
    pub disease: String,
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub biomarker_criteria: String,
    #[serde(default)]
    pub inclusion_criteria: Vec<String>,
    #[serde(default)]
    pub exclusion_criteria: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StudyDesign {
    #[serde(default)]
    pub design_type: String,
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub randomization: String,
    #[serde(default)]
    pub allocation_ratio: String,
    #[serde(default)]
    pub blinding: String,
    #[serde(default)]
    pub follow_up_months: Option<f64>,
    #[serde(default)]
    pub sites: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RecruitmentPlan {
    #[serde(default)]
    pub available_patients_per_site_month: Option<f64>,
    #[serde(default)]
    pub eligibility_rate_pct: Option<f64>,
    #[serde(default)]
    pub biomarker_positive_rate_pct: Option<f64>,
    #[serde(default)]
    pub consent_rate_pct: Option<f64>,
    #[serde(default)]
    pub target_enrollment: Option<u32>,
    #[serde(default)]
    pub accrual_months: Option<f64>,
    #[serde(default)]
    pub sites: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatisticsPlan {
    #[serde(default)]
    pub test_type: String,
    #[serde(default)]
    pub alpha: Option<f64>,
    #[serde(default)]
    pub power: Option<f64>,
    #[serde(default)]
    pub attrition_pct: Option<f64>,
    #[serde(default)]
    pub control_rate: Option<f64>,
    #[serde(default)]
    pub treatment_rate: Option<f64>,
    #[serde(default)]
    pub null_rate: Option<f64>,
    #[serde(default)]
    pub alternative_rate: Option<f64>,
    #[serde(default)]
    pub mean_delta: Option<f64>,
    #[serde(default)]
    pub std_dev: Option<f64>,
    #[serde(default)]
    pub hazard_ratio: Option<f64>,
    #[serde(default)]
    pub event_probability: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SpecificAim {
    pub aim_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub hypothesis: String,
    #[serde(default)]
    pub expected_endpoint_type: String,
    #[serde(default)]
    pub endpoint_ids: Vec<String>,
    #[serde(default)]
    pub expected_result: String,
    #[serde(default)]
    pub risk: String,
    #[serde(default)]
    pub alternative_strategy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StudyArm {
    pub arm_id: String,
    pub name: String,
    #[serde(default)]
    pub intervention: String,
    #[serde(default)]
    pub comparator: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Endpoint {
    pub endpoint_id: String,
    pub name: String,
    pub endpoint_type: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub analysis_family: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TimelineTask {
    pub task_id: String,
    pub name: String,
    pub start_month: f64,
    pub duration_months: f64,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResourceNeed {
    pub resource_id: String,
    pub name: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClinicalStudy {
    #[serde(default)]
    pub clinical_problem: String,
    #[serde(default)]
    pub knowledge_gap: String,
    #[serde(default)]
    pub central_hypothesis: String,
    #[serde(default)]
    pub population: Population,
    #[serde(default)]
    pub design: StudyDesign,
    #[serde(default)]
    pub recruitment: RecruitmentPlan,
    #[serde(default)]
    pub statistics: StatisticsPlan,
    #[serde(default)]
    pub aims: Vec<SpecificAim>,
    #[serde(default)]
    pub arms: Vec<StudyArm>,
    #[serde(default)]
    pub endpoints: Vec<Endpoint>,
    #[serde(default)]
    pub timeline: Vec<TimelineTask>,
    #[serde(default)]
    pub resources: Vec<ResourceNeed>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScenarioSweepInput {
    #[serde(default)]
    pub sites: Vec<u32>,
    #[serde(default)]
    pub consent_rates_pct: Vec<f64>,
    #[serde(default)]
    pub biomarker_positive_rates_pct: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecruitmentAssessment {
    pub complete: bool,
    pub eligible_patients_per_month: Option<f64>,
    pub expected_enrollments_per_month: Option<f64>,
    pub required_enrollments_per_month: Option<f64>,
    pub estimated_accrual_months: Option<f64>,
    pub feasible_within_planned_window: Option<bool>,
    pub shortfall_per_month: Option<f64>,
}

pub fn validate_study(study: &ClinicalStudy) -> Result<()> {
    validate_pct(
        study.recruitment.eligibility_rate_pct,
        "eligibility_rate_pct",
    )?;
    validate_pct(
        study.recruitment.biomarker_positive_rate_pct,
        "biomarker_positive_rate_pct",
    )?;
    validate_pct(study.recruitment.consent_rate_pct, "consent_rate_pct")?;
    validate_pct(study.statistics.attrition_pct, "attrition_pct")?;
    if let Some(a) = study.statistics.alpha {
        if !(0.0 < a && a < 1.0) {
            bail!("alpha must be between 0 and 1");
        }
    }
    if let Some(p) = study.statistics.power {
        if !(0.0 < p && p < 1.0) {
            bail!("power must be between 0 and 1");
        }
    }
    if let Some(v) = study.recruitment.available_patients_per_site_month {
        if v < 0.0 {
            bail!("available_patients_per_site_month cannot be negative");
        }
    }
    if let Some(v) = study.recruitment.accrual_months {
        if v <= 0.0 {
            bail!("accrual_months must be positive");
        }
    }
    if let Some(v) = study.design.follow_up_months {
        if v < 0.0 {
            bail!("follow_up_months cannot be negative");
        }
    }
    if let Some(v) = study.design.sites {
        if v == 0 {
            bail!("design sites must be positive when specified");
        }
    }
    if let Some(v) = study.recruitment.sites {
        if v == 0 {
            bail!("recruitment sites must be positive when specified");
        }
    }
    if let Some(v) = study.recruitment.target_enrollment {
        if v == 0 {
            bail!("target_enrollment must be positive when specified");
        }
    }
    for t in &study.timeline {
        if t.task_id.trim().is_empty() || t.name.trim().is_empty() {
            bail!("timeline task_id and name are required");
        }
        if t.start_month < 0.0 || t.duration_months <= 0.0 {
            bail!("timeline months must be non-negative start and positive duration");
        }
    }
    unique_nonempty(study.aims.iter().map(|x| x.aim_id.as_str()), "aim_id")?;
    unique_nonempty(study.arms.iter().map(|x| x.arm_id.as_str()), "arm_id")?;
    unique_nonempty(
        study.endpoints.iter().map(|x| x.endpoint_id.as_str()),
        "endpoint_id",
    )?;
    unique_nonempty(study.timeline.iter().map(|x| x.task_id.as_str()), "task_id")?;
    unique_nonempty(
        study.resources.iter().map(|x| x.resource_id.as_str()),
        "resource_id",
    )?;
    Ok(())
}

fn validate_pct(v: Option<f64>, name: &str) -> Result<()> {
    if let Some(x) = v {
        if !(0.0..=100.0).contains(&x) {
            bail!("{name} must be between 0 and 100");
        }
    }
    Ok(())
}
fn unique_nonempty<'a>(vals: impl Iterator<Item = &'a str>, name: &str) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for v in vals {
        let x = v.trim();
        if x.is_empty() {
            bail!("{name} cannot be empty");
        }
        if !seen.insert(x.to_string()) {
            bail!("duplicate {name}: {x}");
        }
    }
    Ok(())
}

pub fn assess(study: &ClinicalStudy, approved_sections: &Value) -> Value {
    let recruitment = assess_recruitment(&study.recruitment);
    let mut warnings = Vec::<Value>::new();
    let mut errors = Vec::<Value>::new();

    if study.central_hypothesis.trim().is_empty() {
        warnings.push(issue(
            "missing_hypothesis",
            "Central hypothesis has not been defined",
            None,
        ));
    }
    if study.aims.is_empty() {
        errors.push(issue(
            "missing_aims",
            "No structured Specific Aims have been defined",
            None,
        ));
    }
    if !study.endpoints.iter().any(|e| e.primary) {
        errors.push(issue(
            "missing_primary_endpoint",
            "No primary endpoint is marked",
            None,
        ));
    }
    if !study.design.randomization.trim().is_empty() && study.arms.len() < 2 {
        warnings.push(issue(
            "randomization_without_multiple_arms",
            "Randomization is configured but fewer than two study arms are defined",
            None,
        ));
    }

    let endpoint_map: std::collections::HashMap<&str, &Endpoint> = study
        .endpoints
        .iter()
        .map(|e| (e.endpoint_id.as_str(), e))
        .collect();
    for aim in &study.aims {
        if aim.endpoint_ids.is_empty() {
            errors.push(issue(
                "aim_without_endpoint",
                &format!("{} has no linked endpoint", aim.aim_id),
                Some(&aim.aim_id),
            ));
        }
        for eid in &aim.endpoint_ids {
            match endpoint_map.get(eid.as_str()) {
                None => errors.push(issue(
                    "missing_endpoint_reference",
                    &format!("{} references unknown endpoint {}", aim.aim_id, eid),
                    Some(&aim.aim_id),
                )),
                Some(ep) => {
                    if !aim.expected_endpoint_type.trim().is_empty()
                        && normalize_type(&aim.expected_endpoint_type)
                            != normalize_type(&ep.endpoint_type)
                    {
                        errors.push(issue(
                            "aim_endpoint_type_mismatch",
                            &format!(
                                "{} expects endpoint type '{}' but {} is '{}'",
                                aim.aim_id, aim.expected_endpoint_type, eid, ep.endpoint_type
                            ),
                            Some(&aim.aim_id),
                        ));
                    }
                }
            }
        }
    }
    for ep in &study.endpoints {
        if !analysis_compatible(&ep.endpoint_type, &ep.analysis_family) {
            errors.push(issue(
                "endpoint_analysis_mismatch",
                &format!(
                    "Endpoint {} type '{}' is not compatible with analysis family '{}'",
                    ep.endpoint_id, ep.endpoint_type, ep.analysis_family
                ),
                Some(&ep.endpoint_id),
            ));
        }
    }

    let timeline_issues = timeline_issues(&study.timeline);
    for x in timeline_issues {
        errors.push(x);
    }
    for r in study
        .resources
        .iter()
        .filter(|r| r.required && !r.available)
    {
        warnings.push(issue(
            "missing_resource",
            &format!("Required resource '{}' is not marked available", r.name),
            Some(&r.resource_id),
        ));
    }
    if recruitment.feasible_within_planned_window == Some(false) {
        warnings.push(issue(
            "accrual_infeasible",
            "Recruitment assumptions do not support the planned accrual window",
            None,
        ));
    }

    let statistics = sample_size(&study.statistics)
        .unwrap_or_else(|e| json!({"complete":false,"error":e.to_string()}));
    if let (Some(target), Some(required)) = (
        study.recruitment.target_enrollment,
        statistics.get("adjusted_total_n").and_then(Value::as_u64),
    ) {
        if (target as u64) < required {
            warnings.push(issue(
                "sample_size_shortfall",
                &format!(
                    "Target enrollment {} is below calculated adjusted sample size {}",
                    target, required
                ),
                None,
            ));
        }
    }

    let consistency = cross_section_consistency(study, approved_sections);
    let valid_for_context = errors.is_empty();
    json!({
        "recruitment":recruitment,
        "statistics":statistics,
        "errors":errors,
        "warnings":warnings,
        "timeline":study.timeline,
        "resource_readiness":{"required":study.resources.iter().filter(|r|r.required).count(),"missing":study.resources.iter().filter(|r|r.required&&!r.available).count()},
        "cross_section_consistency":consistency,
        "valid_for_context":valid_for_context
    })
}

fn issue(code: &str, message: &str, object_id: Option<&str>) -> Value {
    json!({"code":code,"message":message,"object_id":object_id})
}

pub fn assess_recruitment(p: &RecruitmentPlan) -> RecruitmentAssessment {
    let sites = p.sites.filter(|x| *x > 0).or(Some(1));
    let factors = (
        p.available_patients_per_site_month,
        p.eligibility_rate_pct,
        p.biomarker_positive_rate_pct,
        p.consent_rate_pct,
        sites,
    );
    if let (Some(base), Some(el), Some(bio), Some(cons), Some(sites)) = factors {
        let eligible = base * (sites as f64) * (el / 100.0) * (bio / 100.0);
        let enrolled = eligible * (cons / 100.0);
        let required = match (p.target_enrollment, p.accrual_months) {
            (Some(n), Some(m)) if m > 0.0 => Some(n as f64 / m),
            _ => None,
        };
        let est = match (p.target_enrollment, enrolled > 0.0) {
            (Some(n), true) => Some(n as f64 / enrolled),
            _ => None,
        };
        let feasible = match (est, p.accrual_months) {
            (Some(e), Some(plan)) => Some(e <= plan + 1e-9),
            _ => None,
        };
        let shortfall = match required {
            Some(r) => Some((r - enrolled).max(0.0)),
            None => None,
        };
        RecruitmentAssessment {
            complete: true,
            eligible_patients_per_month: Some(eligible),
            expected_enrollments_per_month: Some(enrolled),
            required_enrollments_per_month: required,
            estimated_accrual_months: est,
            feasible_within_planned_window: feasible,
            shortfall_per_month: shortfall,
        }
    } else {
        RecruitmentAssessment {
            complete: false,
            eligible_patients_per_month: None,
            expected_enrollments_per_month: None,
            required_enrollments_per_month: None,
            estimated_accrual_months: None,
            feasible_within_planned_window: None,
            shortfall_per_month: None,
        }
    }
}

pub fn scenario_sweep(
    study: &ClinicalStudy,
    input: &ScenarioSweepInput,
    max_combinations: usize,
) -> Result<Value> {
    let default_sites = study.recruitment.sites.or(study.design.sites).unwrap_or(1);
    let sites = if input.sites.is_empty() {
        vec![default_sites]
    } else {
        input.sites.clone()
    };
    let consent = if input.consent_rates_pct.is_empty() {
        vec![study.recruitment.consent_rate_pct.ok_or_else(||anyhow::anyhow!("consent_rate_pct is required in the saved clinical study when scenario consent rates are omitted"))?]
    } else {
        input.consent_rates_pct.clone()
    };
    let biomarker = if input.biomarker_positive_rates_pct.is_empty() {
        vec![study.recruitment.biomarker_positive_rate_pct.ok_or_else(||anyhow::anyhow!("biomarker_positive_rate_pct is required in the saved clinical study when scenario biomarker rates are omitted"))?]
    } else {
        input.biomarker_positive_rates_pct.clone()
    };
    for x in &sites {
        if *x == 0 {
            bail!("scenario sites must be positive");
        }
    }
    for x in &consent {
        validate_pct(Some(*x), "scenario consent rate")?;
    }
    for x in &biomarker {
        validate_pct(Some(*x), "scenario biomarker rate")?;
    }
    let combinations = sites
        .len()
        .saturating_mul(consent.len())
        .saturating_mul(biomarker.len());
    if combinations == 0 {
        bail!("scenario sweep produced no combinations");
    }
    if combinations > max_combinations {
        bail!("scenario sweep has {combinations} combinations, above configured maximum {max_combinations}");
    }
    let mut params = Vec::with_capacity(combinations);
    for &s in &sites {
        for &c in &consent {
            for &b in &biomarker {
                params.push((s, c, b));
            }
        }
    }
    let mut rows:Vec<Value> = params.par_iter().map(|(sites,consent,bio)|{
        let mut p=study.recruitment.clone(); p.sites=Some(*sites); p.consent_rate_pct=Some(*consent); p.biomarker_positive_rate_pct=Some(*bio);
        let a=assess_recruitment(&p);
        json!({"sites":sites,"consent_rate_pct":consent,"biomarker_positive_rate_pct":bio,"eligible_patients_per_month":a.eligible_patients_per_month,"expected_enrollments_per_month":a.expected_enrollments_per_month,"required_enrollments_per_month":a.required_enrollments_per_month,"estimated_accrual_months":a.estimated_accrual_months,"feasible":a.feasible_within_planned_window,"shortfall_per_month":a.shortfall_per_month})
    }).collect();
    rows.sort_by(|a, b| {
        let af = a.get("feasible").and_then(Value::as_bool).unwrap_or(false);
        let bf = b.get("feasible").and_then(Value::as_bool).unwrap_or(false);
        bf.cmp(&af).then_with(|| {
            a.get("estimated_accrual_months")
                .and_then(Value::as_f64)
                .unwrap_or(f64::INFINITY)
                .partial_cmp(
                    &b.get("estimated_accrual_months")
                        .and_then(Value::as_f64)
                        .unwrap_or(f64::INFINITY),
                )
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    Ok(json!({"combinations":combinations,"rows":rows}))
}

pub fn sample_size(p: &StatisticsPlan) -> Result<Value> {
    if p.test_type.trim().is_empty() {
        return Ok(json!({"complete":false,"reason":"test_type not configured"}));
    }
    let alpha = p
        .alpha
        .ok_or_else(|| anyhow::anyhow!("alpha is required"))?;
    let power = p
        .power
        .ok_or_else(|| anyhow::anyhow!("power is required"))?;
    if !(0.0 < alpha && alpha < 1.0 && 0.0 < power && power < 1.0) {
        bail!("alpha and power must be between 0 and 1");
    }
    let za = inv_norm(1.0 - alpha / 2.0);
    let zb = inv_norm(power);
    let (raw_total,method)=match p.test_type.trim().to_ascii_lowercase().as_str() {
        "two_proportions"=>{
            let p1=req_prob(p.control_rate,"control_rate")?; let p2=req_prob(p.treatment_rate,"treatment_rate")?;
            let d=(p2-p1).abs(); if d<=f64::EPSILON{bail!("control_rate and treatment_rate must differ");}
            let pm=(p1+p2)/2.0;
            let n=((za*(2.0*pm*(1.0-pm)).sqrt()+zb*(p1*(1.0-p1)+p2*(1.0-p2)).sqrt()).powi(2)/(d*d)).ceil();
            (2.0*n,"normal approximation for two independent proportions")
        },
        "one_proportion"=>{
            let p0=req_prob(p.null_rate,"null_rate")?; let p1=req_prob(p.alternative_rate,"alternative_rate")?;
            let d=(p1-p0).abs(); if d<=f64::EPSILON{bail!("null_rate and alternative_rate must differ");}
            let n=((za*(p0*(1.0-p0)).sqrt()+zb*(p1*(1.0-p1)).sqrt()).powi(2)/(d*d)).ceil();
            (n,"normal approximation for one-sample proportion against a null rate")
        },
        "two_means"=>{
            let delta=p.mean_delta.ok_or_else(||anyhow::anyhow!("mean_delta is required"))?.abs();
            let sd=p.std_dev.ok_or_else(||anyhow::anyhow!("std_dev is required"))?;
            if delta<=0.0||sd<=0.0{bail!("mean_delta and std_dev must be positive");}
            let n=(2.0*(za+zb).powi(2)*sd.powi(2)/delta.powi(2)).ceil();
            (2.0*n,"normal approximation for two independent means with equal allocation")
        },
        "log_rank"=>{
            let hr=p.hazard_ratio.ok_or_else(||anyhow::anyhow!("hazard_ratio is required"))?;
            let event=p.event_probability.ok_or_else(||anyhow::anyhow!("event_probability is required"))?;
            if hr<=0.0 || (hr-1.0).abs()<f64::EPSILON {bail!("hazard_ratio must be positive and not equal to 1");}
            if !(0.0<event&&event<=1.0){bail!("event_probability must be in (0,1]");}
            let events=(4.0*(za+zb).powi(2)/hr.ln().powi(2)).ceil();
            ((events/event).ceil(),"equal-allocation Schoenfeld/log-rank event approximation")
        },
        other=>bail!("unsupported test_type: {other}; supported: two_proportions, one_proportion, two_means, log_rank")
    };
    let attrition = p.attrition_pct.unwrap_or(0.0);
    validate_pct(Some(attrition), "attrition_pct")?;
    if attrition >= 100.0 {
        bail!("attrition_pct must be less than 100");
    }
    let adjusted = (raw_total / (1.0 - attrition / 100.0)).ceil();
    Ok(
        json!({"complete":true,"test_type":p.test_type,"method":method,"raw_total_n":raw_total as u64,"adjusted_total_n":adjusted as u64,"alpha":alpha,"power":power,"attrition_pct":attrition}),
    )
}

fn req_prob(v: Option<f64>, name: &str) -> Result<f64> {
    let x = v.ok_or_else(|| anyhow::anyhow!("{name} is required"))?;
    if !(0.0 < x && x < 1.0) {
        bail!("{name} must be expressed as a proportion between 0 and 1");
    }
    Ok(x)
}

fn normalize_type(s: &str) -> String {
    s.trim()
        .to_ascii_lowercase()
        .replace(' ', "_")
        .replace('-', "_")
}
fn analysis_compatible(endpoint_type: &str, analysis: &str) -> bool {
    let e = normalize_type(endpoint_type);
    let a = normalize_type(analysis);
    if a.is_empty() {
        return true;
    }
    match e.as_str() {
        "binary" => matches!(
            a.as_str(),
            "chi_square" | "fisher_exact" | "logistic_regression" | "two_proportions"
        ),
        "continuous" => matches!(
            a.as_str(),
            "t_test" | "anova" | "linear_regression" | "mixed_model"
        ),
        "count" => matches!(a.as_str(), "poisson" | "negative_binomial"),
        "time_to_event" => matches!(a.as_str(), "log_rank" | "cox" | "cox_regression"),
        "ordinal" => matches!(a.as_str(), "ordinal_logistic" | "wilcoxon"),
        _ => true,
    }
}

fn timeline_issues(tasks: &[TimelineTask]) -> Vec<Value> {
    let map: std::collections::HashMap<&str, &TimelineTask> =
        tasks.iter().map(|t| (t.task_id.as_str(), t)).collect();
    let mut out = Vec::new();
    for t in tasks {
        for dep in &t.dependencies {
            if dep == &t.task_id {
                out.push(issue(
                    "timeline_self_dependency",
                    &format!("{} depends on itself", t.task_id),
                    Some(&t.task_id),
                ));
                continue;
            }
            if let Some(d) = map.get(dep.as_str()) {
                let finish = d.start_month + d.duration_months;
                if finish > t.start_month + 1e-9 {
                    out.push(issue(
                        "timeline_dependency_overlap",
                        &format!(
                            "{} starts at month {:.1} before dependency {} finishes at month {:.1}",
                            t.task_id, t.start_month, dep, finish
                        ),
                        Some(&t.task_id),
                    ));
                }
            } else {
                out.push(issue(
                    "timeline_missing_dependency",
                    &format!("{} references unknown dependency {}", t.task_id, dep),
                    Some(&t.task_id),
                ));
            }
        }
    }
    out
}

fn cross_section_consistency(study: &ClinicalStudy, sections: &Value) -> Value {
    let mut conflicts = Vec::<Value>::new();
    let target = study.recruitment.target_enrollment.map(|x| x as i64);
    let sites = study
        .recruitment
        .sites
        .or(study.design.sites)
        .map(|x| x as i64);
    if let Some(arr) = sections.as_array() {
        for s in arr {
            let body = s.get("body").and_then(Value::as_str).unwrap_or("");
            let title = s.get("title").and_then(Value::as_str).unwrap_or("Section");
            if let Some(expected) = target {
                for found in contextual_numbers(
                    body,
                    &["enroll", "enrollment", "participants", "subjects"],
                    5,
                ) {
                    if found >= 10 && found != expected {
                        conflicts.push(json!({"field":"target_enrollment","authoritative":expected,"section":title,"observed":found,"severity":"warning"}));
                    }
                }
            }
            if let Some(expected) = sites {
                for found in contextual_numbers(body, &["site", "sites", "centers", "centres"], 3) {
                    if found > 0 && found != expected {
                        conflicts.push(json!({"field":"sites","authoritative":expected,"section":title,"observed":found,"severity":"warning"}));
                    }
                }
            }
        }
    }
    conflicts.sort_by_key(|x| {
        (
            x.get("field")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            x.get("section")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            x.get("observed").and_then(Value::as_i64).unwrap_or(0),
        )
    });
    conflicts.dedup();
    json!({"conflicts":conflicts,"count":conflicts.len()})
}

fn contextual_numbers(text: &str, terms: &[&str], window: usize) -> Vec<i64> {
    let tokens: Vec<String> = text
        .split_whitespace()
        .map(|t| {
            t.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.')
                .to_ascii_lowercase()
        })
        .collect();
    let mut out = Vec::new();
    for (i, tok) in tokens.iter().enumerate() {
        if terms.iter().any(|term| tok.starts_with(term)) {
            let lo = i.saturating_sub(window);
            let hi = (i + window + 1).min(tokens.len());
            for x in &tokens[lo..hi] {
                let clean = x.replace(',', "");
                if let Ok(v) = clean.parse::<i64>() {
                    out.push(v);
                }
            }
        }
    }
    out
}

// Acklam-style rational approximation to the inverse standard-normal CDF.
// Accurate enough for deterministic grant-planning sample-size calculations.
fn inv_norm(p: f64) -> f64 {
    let a = [
        -3.969683028665376e1,
        2.209460984245205e2,
        -2.759285104469687e2,
        1.383577518672690e2,
        -3.066479806614716e1,
        2.506628277459239,
    ];
    let b = [
        -5.447609879822406e1,
        1.615858368580409e2,
        -1.556989798598866e2,
        6.680131188771972e1,
        -1.328068155288572e1,
    ];
    let c = [
        -7.784894002430293e-3,
        -3.223964580411365e-1,
        -2.400758277161838,
        -2.549732539343734,
        4.374664141464968,
        2.938163982698783,
    ];
    let d = [
        7.784695709041462e-3,
        3.224671290700398e-1,
        2.445134137142996,
        3.754408661907416,
    ];
    let pl = 0.02425;
    let ph = 1.0 - pl;
    if p < pl {
        let q = (-2.0 * p.ln()).sqrt();
        return (((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5])
            / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0);
    }
    if p > ph {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        return -(((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5])
            / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0);
    }
    let q = p - 0.5;
    let r = q * q;
    (((((a[0] * r + a[1]) * r + a[2]) * r + a[3]) * r + a[4]) * r + a[5]) * q
        / (((((b[0] * r + b[1]) * r + b[2]) * r + b[3]) * r + b[4]) * r + 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recruitment_math() {
        let p = RecruitmentPlan {
            available_patients_per_site_month: Some(14.0),
            eligibility_rate_pct: Some(100.0),
            biomarker_positive_rate_pct: Some(35.0),
            consent_rate_pct: Some(70.0),
            target_enrollment: Some(180),
            accrual_months: Some(24.0),
            sites: Some(1),
        };
        let x = assess_recruitment(&p);
        assert!((x.expected_enrollments_per_month.unwrap() - 3.43).abs() < 0.01);
        assert_eq!(x.feasible_within_planned_window, Some(false));
    }
    #[test]
    fn two_proportion_sample_size() {
        let p = StatisticsPlan {
            test_type: "two_proportions".into(),
            alpha: Some(0.05),
            power: Some(0.8),
            control_rate: Some(0.25),
            treatment_rate: Some(0.40),
            attrition_pct: Some(10.0),
            ..Default::default()
        };
        let x = sample_size(&p).unwrap();
        assert!(x["adjusted_total_n"].as_u64().unwrap() > x["raw_total_n"].as_u64().unwrap());
    }
    #[test]
    fn sweep_parallel() {
        let mut s = ClinicalStudy::default();
        s.recruitment = RecruitmentPlan {
            available_patients_per_site_month: Some(10.0),
            eligibility_rate_pct: Some(80.0),
            biomarker_positive_rate_pct: Some(50.0),
            consent_rate_pct: Some(70.0),
            target_enrollment: Some(100),
            accrual_months: Some(24.0),
            sites: Some(1),
        };
        let x = scenario_sweep(
            &s,
            &ScenarioSweepInput {
                sites: vec![1, 2],
                consent_rates_pct: vec![60.0, 80.0],
                biomarker_positive_rates_pct: vec![40.0, 60.0],
            },
            100,
        )
        .unwrap();
        assert_eq!(x["combinations"].as_u64(), Some(8));
    }
}

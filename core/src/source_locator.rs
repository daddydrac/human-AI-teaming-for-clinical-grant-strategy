use anyhow::{bail, Result};
use rayon::prelude::*;
use std::collections::{BTreeSet, HashSet};

use crate::compliance::{
    ComplianceDraftEnvelope, ComplianceProfile, ComplianceProfileDraft, ComplianceRule, ComplianceRuleDraft,
    SOURCE_NOT_LOCATED,
};

#[derive(Debug, Clone)]
pub struct SourceDocument {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub text: String,
}

#[derive(Debug)]
struct Projection {
    text: String,
    // One original UTF-8 byte range for every byte in normalized ASCII `text`.
    original: Vec<(usize, usize)>,
}

impl Projection {
    fn original_range(&self,start:usize,end:usize)->Option<(usize,usize)>{
        if start>=end{return None;}
        Some((self.original.get(start)?.0,self.original.get(end-1)?.1))
    }
}

#[derive(Debug)]
struct Candidate {
    start: usize,
    end: usize,
    normalized: String,
    terms: HashSet<String>,
}

struct PreparedDocument<'a> {
    source: &'a SourceDocument,
    name_normalized: String,
    projection: Projection,
    candidates: Vec<Candidate>,
}

#[derive(Debug)]
struct Match {
    document_id: i64,
    document_name: String,
    start: usize,
    end: usize,
    page: Option<u32>,
    score: f64,
}

fn normalized_projection(input: &str) -> Projection {
    let chars=input.char_indices().collect::<Vec<_>>();
    let mut text=String::new();
    let mut original=Vec::new();
    let mut i=0usize;
    while i<chars.len() {
        let (start,ch)=chars[i];
        let end=chars.get(i+1).map(|x|x.0).unwrap_or(input.len());
        // Join PDF line-break hyphenation only in the search projection. The
        // authoritative source buffer remains completely unchanged.
        if ch=='-' {
            let mut j=i+1; let mut newline=false;
            while j<chars.len() && chars[j].1.is_whitespace() {newline|=chars[j].1=='\n'||chars[j].1=='\r';j+=1;}
            if newline && j<chars.len() && chars[j].1.is_alphanumeric() {i=j;continue;}
        }
        if ch.is_ascii_alphanumeric() {
            text.push(ch.to_ascii_lowercase()); original.push((start,end));
        } else if !text.is_empty() && !text.ends_with(' ') {
            text.push(' '); original.push((start,end));
        }
        i+=1;
    }
    while text.ends_with(' ') {text.pop();original.pop();}
    Projection{text,original}
}

fn canonical_term(term:&str)->String {
    let mut s=term.to_ascii_lowercase();
    if s.len()>4 && s.ends_with("ies") {s.truncate(s.len()-3);s.push('y');}
    else if s.len()>4 && s.ends_with('s') && !s.ends_with("ss") {s.pop();}
    s
}

fn terms(input:&str)->HashSet<String> {
    const STOP:&[&str]=&["a","an","and","are","as","at","be","by","for","from","in","is","it","of","on","or","the","this","to","with","requirement","limitation","rule","section"];
    normalized_projection(input).text.split_whitespace().map(canonical_term)
        .filter(|x|x.len()>1&&!STOP.contains(&x.as_str())).collect()
}

fn trim_span(text:&str,mut start:usize,mut end:usize)->Option<(usize,usize)> {
    while start<end {let ch=text[start..].chars().next()?;if !ch.is_whitespace(){break;}start+=ch.len_utf8();}
    while end>start {let ch=text[..end].chars().next_back()?;if !ch.is_whitespace(){break;}end-=ch.len_utf8();}
    (start<end).then_some((start,end))
}

fn candidate_spans(text:&str)->Vec<(usize,usize)> {
    let mut spans=BTreeSet::new();
    let mut sentence_start=0usize; let mut paragraph_start=0usize; let mut previous_newline=false;
    for (idx,ch) in text.char_indices() {
        let end=idx+ch.len_utf8();
        if matches!(ch,'.'|'?'|'!'|';'|'\n'|'\u{000c}') {
            if let Some(span)=trim_span(text,sentence_start,end) {if span.1-span.0>=12&&span.1-span.0<=1600{spans.insert(span);}}
            sentence_start=end;
        }
        if ch=='\u{000c}' || (ch=='\n'&&previous_newline) {
            if let Some(span)=trim_span(text,paragraph_start,idx) {if span.1-span.0>=12&&span.1-span.0<=2400{spans.insert(span);}}
            paragraph_start=end;
        }
        previous_newline=ch=='\n';
    }
    for start in [sentence_start,paragraph_start] {
        if let Some(span)=trim_span(text,start,text.len()) {if span.1-span.0>=12&&span.1-span.0<=2400{spans.insert(span);}}
    }
    // Lines and short paragraphs cover bullets whose punctuation is inconsistent.
    let mut offset=0usize;
    for line in text.split_inclusive('\n') {
        let end=offset+line.len();if let Some(span)=trim_span(text,offset,end){if span.1-span.0>=12&&span.1-span.0<=1600{spans.insert(span);}}offset=end;
    }
    spans.into_iter().collect()
}

fn prepared_candidates(text:&str)->Vec<Candidate> {
    candidate_spans(text).into_iter().filter_map(|(start,end)|{
        let normalized=normalized_projection(&text[start..end]).text;
        if normalized.is_empty(){None}else{let terms=terms(&normalized);Some(Candidate{start,end,normalized,terms})}
    }).collect()
}

fn numeric_token(value:f64)->String {
    if value.fract().abs()<f64::EPSILON {format!("{}",value as i64)} else {value.to_string()}
}

fn type_terms(rule_type:&str)->HashSet<String> {
    let text=match rule_type {
        "max_pages"=>"page pages exceed maximum limit",
        "max_words"|"min_words"=>"word words maximum minimum limit",
        "min_font_size_pt"=>"font point points size",
        "min_margin_in"=>"margin margins inch inches",
        "deadline"=>"deadline due date",
        "required_attachment"=>"attachment attach upload include",
        "required_form"=>"form forms application package",
        "required_section"=>"section plan strategy narrative required",
        "required_letter_count"=>"letter letters support reference",
        "max_budget"=>"budget cost costs maximum direct",
        "project_period_max_months"=>"project period month months year years",
        "allowed_extensions"=>"file format extension upload",
        "submission_system"=>"submit submission portal system",
        _=>"must shall required may should",
    };
    terms(text)
}

fn ratio(found:usize,total:usize)->f64 {if total==0{0.0}else{found as f64/total as f64}}

fn derived_page(text:&str,start:usize)->Option<u32> {
    text.contains('\u{000c}').then(||text[..start].chars().filter(|c|*c=='\u{000c}').count() as u32+1)
}

fn locate_rule(rule:&ComplianceRuleDraft,documents:&[PreparedDocument<'_>])->Option<Match> {
    let hint_norm=normalized_projection(&rule.source_hint).text;
    let hint_terms=terms(&rule.source_hint);
    let target_terms=terms(&rule.target);
    let kind_terms=type_terms(&rule.rule_type);
    let numeric=rule.numeric_value.map(numeric_token);
    let document_hint=rule.source_document_hint.as_deref().map(|x|normalized_projection(x).text).filter(|x|!x.is_empty());
    let mut ranked=Vec::<Match>::new();
    for prepared in documents {
        let doc=prepared.source;
        let hint_anchor=(!hint_norm.is_empty()).then(||prepared.projection.text.find(&hint_norm)).flatten()
            .and_then(|start|prepared.projection.original_range(start,start+hint_norm.len()));
        for candidate in &prepared.candidates {
            let hint_found=hint_terms.intersection(&candidate.terms).count();
            let target_found=target_terms.intersection(&candidate.terms).count();
            let kind_found=kind_terms.intersection(&candidate.terms).count();
            let exact_hint=hint_norm.split_whitespace().count()>=3 && candidate.normalized.contains(&hint_norm);
            if hint_found==0 || (!target_terms.is_empty()&&target_found==0) {continue;}
            if let Some(n)=numeric.as_deref() {if !candidate.terms.contains(&canonical_term(n)){continue;}}
            let page=derived_page(&doc.text,candidate.start);
            if let Some(wanted)=rule.source_page_hint {if page.is_some()&&page!=Some(wanted){continue;}}
            let mut score=ratio(hint_found,hint_terms.len())*0.48+ratio(target_found,target_terms.len())*0.24;
            score+=ratio(kind_found,kind_terms.len()).min(0.5)*0.20;
            if numeric.is_some(){score+=0.20;}
            if exact_hint{score+=0.30;}
            if hint_anchor.is_some_and(|(start,end)|candidate.start<=start&&candidate.end>=end){score+=0.12;}
            if document_hint.as_ref().is_some_and(|h|prepared.name_normalized.contains(h)){score+=0.10;}
            // Prefer a concise supporting span when two candidates cover the same terms.
            score-=(candidate.end-candidate.start).saturating_sub(500).min(1900) as f64/1900.0*0.08;
            if score>=0.48 {ranked.push(Match{document_id:doc.id,document_name:doc.name.clone(),start:candidate.start,end:candidate.end,page,score});}
        }
    }
    ranked.sort_by(|a,b|b.score.total_cmp(&a.score).then_with(||(a.end-a.start).cmp(&(b.end-b.start))).then_with(||a.document_id.cmp(&b.document_id)).then_with(||a.start.cmp(&b.start)));
    let best=ranked.first()?;
    // Close competing passages are ambiguous: fail closed for human review.
    // The deterministic tie ordering is for reproducibility, not permission to guess.
    if ranked.iter().skip(1).any(|other|{
        let overlaps=other.document_id==best.document_id&&other.start<best.end&&best.start<other.end;
        !overlaps&&other.score+0.04>=best.score
    }){return None;}
    Some(Match{document_id:best.document_id,document_name:best.document_name.clone(),start:best.start,end:best.end,page:best.page,score:best.score})
}

fn not_located(rule:ComplianceRuleDraft)->ComplianceRule {
    ComplianceRule{rule_id:rule.rule_id,category:rule.category,rule_type:rule.rule_type,scope:rule.scope,target:rule.target,severity:rule.severity,mandatory:rule.mandatory,numeric_value:rule.numeric_value,text_value:rule.text_value,list_value:rule.list_value,source_hint:rule.source_hint,source_document_hint:rule.source_document_hint,source_page_hint:rule.source_page_hint,source_excerpt:SOURCE_NOT_LOCATED.into(),source_locator:SOURCE_NOT_LOCATED.into(),source_start_offset:None,source_end_offset:None,source_document_id:None,source_page:None,source_status:"not_located".into(),notes:rule.notes}
}

pub fn locate_profile(draft:ComplianceProfileDraft,documents:&[SourceDocument])->ComplianceProfile {
    let prepared=documents.iter().map(|source|PreparedDocument{
        source,
        name_normalized:normalized_projection(&format!("{} {} {}",source.id,source.kind,source.name)).text,
        projection:normalized_projection(&source.text),
        candidates:prepared_candidates(&source.text),
    }).collect::<Vec<_>>();
    // Rules are independent searches over immutable source buffers, so Rayon
    // scales large opportunities without changing deterministic output order.
    let rules=draft.rules.into_par_iter().map(|rule|{
        let Some(found)=locate_rule(&rule,&prepared) else{return not_located(rule);};
        let Some(doc)=documents.iter().find(|d|d.id==found.document_id) else{return not_located(rule);};
        let Some(excerpt)=doc.text.get(found.start..found.end) else{return not_located(rule);};
        if excerpt.is_empty() || !doc.text.contains(excerpt){return not_located(rule);}
        ComplianceRule{rule_id:rule.rule_id,category:rule.category,rule_type:rule.rule_type,scope:rule.scope,target:rule.target,severity:rule.severity,mandatory:rule.mandatory,numeric_value:rule.numeric_value,text_value:rule.text_value,list_value:rule.list_value,source_hint:rule.source_hint,source_document_hint:rule.source_document_hint,source_page_hint:rule.source_page_hint,source_excerpt:excerpt.to_string(),source_locator:format!("document {} ({}), bytes {}..{}{}",found.document_id,found.document_name,found.start,found.end,found.page.map(|p|format!(", page {p}")).unwrap_or_default()),source_start_offset:Some(found.start),source_end_offset:Some(found.end),source_document_id:Some(found.document_id),source_page:found.page,source_status:"located".into(),notes:rule.notes}
    }).collect();
    ComplianceProfile{sponsor:draft.sponsor,mechanism:draft.mechanism,submission_system:draft.submission_system,deadline_iso:draft.deadline_iso,rules}
}

/// Compile the exact model-output contract through deterministic provenance.
/// Both real providers and provider-faithful mocks enter through this function.
pub fn compile_model_output(text:&str,documents:&[SourceDocument])->Result<ComplianceProfile>{
    let parsed:ComplianceDraftEnvelope=crate::json_extract::parse_json_from_model(text)?;
    let profile=locate_profile(parsed.profile,documents);
    crate::compliance::validate_profile(&profile)?;
    validate_exact_sources(&profile,documents)?;
    Ok(profile)
}

pub fn validate_exact_sources(profile:&ComplianceProfile,documents:&[SourceDocument])->Result<()> {
    for rule in &profile.rules {
        if rule.source_status=="not_located" {continue;}
        let document_id=rule.source_document_id.ok_or_else(||anyhow::anyhow!("rule {} has no source document",rule.rule_id))?;
        let doc=documents.iter().find(|d|d.id==document_id).ok_or_else(||anyhow::anyhow!("rule {} references unknown funding-opportunity document {}",rule.rule_id,document_id))?;
        let start=rule.source_start_offset.ok_or_else(||anyhow::anyhow!("rule {} has no source start offset",rule.rule_id))?;
        let end=rule.source_end_offset.ok_or_else(||anyhow::anyhow!("rule {} has no source end offset",rule.rule_id))?;
        let exact=doc.text.get(start..end).ok_or_else(||anyhow::anyhow!("rule {} source offsets are not valid UTF-8 boundaries",rule.rule_id))?;
        if exact!=rule.source_excerpt || !doc.text.contains(&rule.source_excerpt) {bail!("rule {} source_excerpt is not the exact source characters at its persisted offsets",rule.rule_id);}
        // This assertion is deliberately retained in release builds: the
        // preceding checked error path makes it non-panicking for bad input,
        // while documenting the invariant at the backend boundary.
        assert!(doc.text.contains(&rule.source_excerpt));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(hint:&str)->ComplianceProfileDraft {ComplianceProfileDraft{sponsor:None,mechanism:None,submission_system:None,deadline_iso:None,rules:vec![ComplianceRuleDraft{rule_id:"C-001".into(),category:"format".into(),rule_type:"max_pages".into(),scope:"section".into(),target:"Research Strategy".into(),severity:"hard".into(),mandatory:true,numeric_value:Some(12.0),text_value:None,list_value:vec![],source_hint:hint.into(),source_document_hint:Some("notice.html".into()),source_page_hint:None,notes:String::new()}]}}

    #[test]
    fn copies_exact_characters_from_url_extracted_buffer(){
        let text="Overview\nThe Research Strategy may not exceed 12 pages.\nOther instructions.";
        let docs=vec![SourceDocument{id:42,name:"notice.html".into(),kind:"funding_url".into(),text:text.into()}];
        let profile=locate_profile(draft("Research Strategy page limitation"),&docs);
        let rule=&profile.rules[0];
        assert_eq!(rule.source_excerpt,"The Research Strategy may not exceed 12 pages.");
        assert_eq!(&text[rule.source_start_offset.unwrap()..rule.source_end_offset.unwrap()],rule.source_excerpt);
        assert_eq!(rule.source_document_id,Some(42));assert_eq!(rule.source_page,None);
        validate_exact_sources(&profile,&docs).unwrap();
    }

    #[test]
    fn normalized_hyphenation_is_only_used_to_locate(){
        let text="The Research Strategy may not ex-\nceed 12 pages.";
        let docs=vec![SourceDocument{id:7,name:"notice.pdf".into(),kind:"funding_opportunity".into(),text:text.into()}];
        let profile=locate_profile(draft("Research Strategy may not exceed 12 pages"),&docs);
        assert_eq!(profile.rules[0].source_excerpt,text);
        assert!(profile.rules[0].source_excerpt.contains("ex-\nceed"));
    }

    #[test]
    fn fails_closed_when_source_cannot_be_located(){
        let docs=vec![SourceDocument{id:1,name:"notice.html".into(),kind:"funding_url".into(),text:"No page limit appears here.".into()}];
        let profile=locate_profile(draft("Research Strategy page limitation"),&docs);
        assert_eq!(profile.rules[0].source_status,"not_located");
        assert_eq!(profile.rules[0].source_excerpt,SOURCE_NOT_LOCATED);
        assert!(profile.rules[0].source_document_id.is_none());
    }
}

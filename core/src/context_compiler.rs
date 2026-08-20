use anyhow::Result;
use crate::{retrieval::RetrievalService,storage::Store};

pub struct CompiledContext {
    pub text:String,
    pub retrieved:serde_json::Value,
}

pub async fn compile(store:&Store,retrieval:&RetrievalService,project:&str,task:&str,token_budget_chars:usize)->Result<CompiledContext>{
    let k=std::env::var("CONTEXT_RETRIEVAL_K").ok().and_then(|v|v.parse().ok()).unwrap_or(24usize).clamp(4,128);
    let hits=retrieval.search(project,task,k).await?;
    let mut out=String::new();
    let mut rows=Vec::new();
    let requirement_json=store.requirements_json(project)?;
    let req=serde_json::to_string_pretty(&requirement_json)?;
    out.push_str("APPROVED GRANT REQUIREMENTS:\n");
    out.push_str(&req);
    let clinical=store.clinical_context(project)?;
    out.push_str("\n\n"); out.push_str(&clinical);
    let remaining_for_compliance=token_budget_chars.saturating_sub(out.len()).min(18_000);
    if remaining_for_compliance>2_000 {
        let compliance=store.compliance_context(project,remaining_for_compliance)?;
        out.push_str("

"); out.push_str(&compliance);
    }
    let remaining_for_competitive=token_budget_chars.saturating_sub(out.len()).min(32_000);
    if remaining_for_competitive>2_000 {
        let competitive=store.competitive_context(project,remaining_for_competitive)?;
        out.push_str("\n\n"); out.push_str(&competitive);
    }
    out.push_str("\n\nRETRIEVED RUN-SPECIFIC CONTEXT:\n");
    for hit in &hits {
        let r=&hit.record;
        let block=format!("\n--- {} | {} | score={:.4} semantic={:.4} lexical={:.4} evidence={:.4} freshness={:.4} ---\nSOURCE: {}\nREQUIREMENT: {}\nSTATUS: {} confidence={:.2}\n{}\n",
            r.kind,r.item_id,hit.score,hit.semantic,hit.lexical,hit.evidence,hit.freshness,r.source_ref,r.requirement_id.as_deref().unwrap_or("none"),r.status,r.confidence,r.text);
        if out.len()+block.len()>token_budget_chars {break;} out.push_str(&block);
        rows.push(serde_json::json!({"row":hit.row,"score":hit.score,"semantic":hit.semantic,"lexical":hit.lexical,"evidence":hit.evidence,"freshness":hit.freshness,"graph_boost":hit.graph_boost,"item_id":r.item_id,"kind":r.kind,"requirement_id":r.requirement_id,"source_ref":r.source_ref,"status":r.status}));
    }
    Ok(CompiledContext{text:out,retrieved:serde_json::json!(rows)})
}

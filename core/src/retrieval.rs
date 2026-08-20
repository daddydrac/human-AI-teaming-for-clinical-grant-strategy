use anyhow::{Context, Result};
use parking_lot::Mutex as ParkingMutex;
use serde::{Deserialize, Serialize};
use std::{collections::{BTreeSet, HashMap}, path::{Path, PathBuf}, sync::Arc};
use tokio::sync::Mutex;

use crate::{
    csr::{self, RequirementCsr},
    domain::RetrievalRecord,
    embedding::EmbeddingClient,
    hpc,
    lexical::{self, LexicalIndex},
    parquet_store,
    record_store::{self, MmapRecordStore},
    storage::Store,
    vector_store::{MmapMatrix, MmapMatrixWriter},
};

#[derive(Debug,Clone,Serialize,Deserialize)]
pub struct IndexManifest {
    pub version:u32,
    pub fingerprint:String,
    pub embedding_model:String,
    pub dimensions:usize,
    pub rows:usize,
    pub created_at:String,
}

#[derive(Debug,Clone,Serialize)]
pub struct RetrievalHit {
    pub row:u32,
    pub score:f32,
    pub semantic:f32,
    pub lexical:f32,
    pub evidence:f32,
    pub freshness:f32,
    pub graph_boost:f32,
    pub record:RetrievalRecord,
}

#[derive(Debug,Clone)]
struct RetrievalConfig {
    weights:[f32;4],
    graph_boost:f32,
    candidate_multiplier:usize,
    openmp_threshold:usize,
    bm25_k1:f32,
    bm25_b:f32,
    freshness_half_life_days:f32,
}
impl RetrievalConfig {
    fn env_f32(name:&str,default:f32)->f32{std::env::var(name).ok().and_then(|v|v.parse().ok()).unwrap_or(default)}
    fn from_env()->Self{
        let mut w=[
            Self::env_f32("RETRIEVAL_WEIGHT_SEMANTIC",0.45).max(0.0),
            Self::env_f32("RETRIEVAL_WEIGHT_LEXICAL",0.25).max(0.0),
            Self::env_f32("RETRIEVAL_WEIGHT_EVIDENCE",0.20).max(0.0),
            Self::env_f32("RETRIEVAL_WEIGHT_FRESHNESS",0.10).max(0.0),
        ];
        let sum=w.iter().sum::<f32>(); if sum>0.0 {for x in &mut w{*x/=sum;}}
        Self{
            weights:w,
            graph_boost:Self::env_f32("RETRIEVAL_GRAPH_BOOST",0.08).max(0.0),
            candidate_multiplier:std::env::var("RETRIEVAL_CANDIDATE_MULTIPLIER").ok().and_then(|v|v.parse().ok()).unwrap_or(4usize).clamp(1,32),
            openmp_threshold:std::env::var("RETRIEVAL_OPENMP_THRESHOLD").ok().and_then(|v|v.parse().ok()).unwrap_or(4096usize).max(256),
            bm25_k1:Self::env_f32("BM25_K1",1.2).max(0.01),
            bm25_b:Self::env_f32("BM25_B",0.75).clamp(0.0,1.0),
            freshness_half_life_days:Self::env_f32("RETRIEVAL_FRESHNESS_HALF_LIFE_DAYS",365.0).max(1.0),
        }
    }
}

pub struct RetrievalEngine {
    dir:PathBuf,
    manifest:IndexManifest,
    vectors:MmapMatrix,
    lexical:LexicalIndex,
    records:MmapRecordStore,
    csr:RequirementCsr,
    config:RetrievalConfig,
}

impl RetrievalEngine {
    pub async fn build(store:&Store,embed:&EmbeddingClient,workspace:&Path,project:&str)->Result<IndexManifest>{
        // Bind the build to one authoritative knowledge-state fingerprint. If source
        // state changes while embeddings/index files are being produced, discard the
        // staging build rather than publishing an index whose manifest claims newer data.
        let input_fingerprint=store.retrieval_fingerprint(project)?;
        let mut records=store.retrieval_records(project)?;
        if records.is_empty(){anyhow::bail!("project has no retrieval records to index");}
        for (i,r) in records.iter_mut().enumerate(){r.row=i as u32;}
        let texts:Vec<String>=records.iter().map(|r|r.text.clone()).collect();
        let base=workspace.join("projects").join(project);
        let project_dir=base.join("index");
        let staging=base.join(format!("index.staging.{}",uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&staging)?;
        let result=async {
            let write_batch=std::env::var("INDEX_EMBED_BATCH_RECORDS").ok().and_then(|v|v.parse().ok()).unwrap_or(64usize).clamp(1,512);
            let first_end=write_batch.min(texts.len());
            let first=embed.embed_documents(&texts[..first_end]).await?;
            let dims=first.first().context("embedding endpoint returned zero vectors")?.len();
            if first.iter().any(|v|v.len()!=dims){anyhow::bail!("inconsistent embedding dimensions");}
            let mut writer=MmapMatrixWriter::create(&staging.join("vectors.f32"),records.len(),dims)?;
            let first_flat:Vec<f32>=first.into_iter().flatten().collect(); writer.write_rows(0,&first_flat)?;
            let mut start=first_end;
            while start<texts.len(){
                let end=(start+write_batch).min(texts.len());
                let batch=embed.embed_documents(&texts[start..end]).await?;
                if batch.len()!=end-start || batch.iter().any(|v|v.len()!=dims){anyhow::bail!("embedding batch shape mismatch at rows {start}..{end}");}
                let flat:Vec<f32>=batch.into_iter().flatten().collect(); writer.write_rows(start,&flat)?; start=end;
            }
            writer.finish()?;
            lexical::build(&staging,&texts)?;
            record_store::build(&staging,&records)?;
            let requirement_ids=store.requirement_ids(project)?;
            let record_req:Vec<Option<String>>=records.iter().map(|r|r.requirement_id.clone()).collect();
            csr::build(&staging,&requirement_ids,&record_req)?;
            parquet_store::write_retrieval_parquet(&staging.join("retrieval.parquet"),&records)?;
            let final_fingerprint=store.retrieval_fingerprint(project)?;
            if final_fingerprint!=input_fingerprint{
                anyhow::bail!("project knowledge changed during index build; discard staging index and retry");
            }
            let manifest=IndexManifest{
                version:2,
                fingerprint:input_fingerprint.clone(),
                embedding_model:embed.model().to_string(),
                dimensions:dims,
                rows:records.len(),
                created_at:time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?,
            };
            std::fs::write(staging.join("manifest.json"),serde_json::to_vec_pretty(&manifest)?)?;
            Ok::<IndexManifest,anyhow::Error>(manifest)
        }.await;
        let manifest=match result {Ok(m)=>m,Err(e)=>{let _=std::fs::remove_dir_all(&staging);return Err(e);}};
        if project_dir.exists(){
            let old=base.join(format!("index.old.{}",uuid::Uuid::new_v4()));
            std::fs::rename(&project_dir,&old)?;
            if let Err(e)=std::fs::rename(&staging,&project_dir){let _=std::fs::rename(&old,&project_dir);return Err(e.into());}
            let _=std::fs::remove_dir_all(old);
        } else { std::fs::rename(&staging,&project_dir)?; }
        Ok(manifest)
    }

    pub fn open(workspace:&Path,project:&str)->Result<Self>{
        let dir=workspace.join("projects").join(project).join("index");
        let manifest:IndexManifest=serde_json::from_slice(&std::fs::read(dir.join("manifest.json"))?)?;
        Ok(Self{
            vectors:MmapMatrix::open(&dir.join("vectors.f32"))?,
            lexical:LexicalIndex::open(&dir)?,
            records:MmapRecordStore::open(&dir)?,
            csr:RequirementCsr::open(&dir)?,
            dir,manifest,config:RetrievalConfig::from_env(),
        })
    }
    pub fn manifest(&self)->&IndexManifest{&self.manifest}
    pub fn dir(&self)->&Path{&self.dir}
    pub fn record_count(&self)->usize{self.records.len()}
    pub fn requirement_count(&self)->usize{self.csr.requirement_count()}

    pub fn retrieve(&self,query_embedding:&[f32],query_text:&str,k:usize)->Result<Vec<RetrievalHit>>{
        let semantic=self.vectors.scores(query_embedding)?;
        let lexical=self.lexical.scores(query_text,self.config.bm25_k1,self.config.bm25_b);
        let n=semantic.len();
        let now=time::OffsetDateTime::now_utc().unix_timestamp();
        let mut evidence=vec![0.0f32;n]; let mut freshness=vec![0.0f32;n];
        for i in 0..n {
            let r=self.records.get(i)?;
            evidence[i]=match r.status.as_str(){"supported"=>1.0,"partially_supported"=>0.75,"verified_fact"=>0.9,"approved"=>0.9,"candidate"=>0.35,"contradicted"=>0.05,_=>0.5}*r.confidence.clamp(0.0,1.0).max(0.2);
            freshness[i]=match r.created_unix {
                Some(ts)=>{let age_days=((now-ts).max(0) as f32)/86_400.0; 2.0f32.powf(-age_days/self.config.freshness_half_life_days).clamp(0.05,1.0)},
                None=>0.5,
            };
        }
        let fused=hpc::fuse(&semantic,&lexical,&evidence,&freshness,self.config.weights);
        let candidate_k=(k*self.config.candidate_multiplier).max(k).min(n);
        let pre=if n>=self.config.openmp_threshold {hpc::openmp_topk(&fused,candidate_k)} else {hpc::parallel_topk(&fused,candidate_k)};
        let mut selected:BTreeSet<usize>=pre.iter().map(|x|x.0).collect();
        for (row,_) in pre.iter().take(k.max(1)){
            let r=self.records.get(*row)?;
            if r.kind=="requirement" {if let Some(id)=r.requirement_id.as_deref(){for linked in self.csr.rows_for(id){selected.insert(linked as usize);}}}
        }
        let pre_rows:BTreeSet<usize>=pre.iter().map(|x|x.0).collect();
        let mut hits=Vec::new();
        for row in selected {
            if row>=n{continue;}
            let record=self.records.get(row)?;
            let graph_boost=if pre_rows.contains(&row){0.0}else{self.config.graph_boost};
            hits.push(RetrievalHit{row:row as u32,score:fused[row]+graph_boost,semantic:semantic[row],lexical:lexical[row],evidence:evidence[row],freshness:freshness[row],graph_boost,record});
        }
        hits.sort_by(|a,b|b.score.total_cmp(&a.score)); hits.truncate(k.min(hits.len())); Ok(hits)
    }
}

pub struct RetrievalService {
    store:Arc<Store>,
    embed:Arc<EmbeddingClient>,
    workspace:PathBuf,
    locks:ParkingMutex<HashMap<String,Arc<Mutex<()>>>>,
}
impl RetrievalService {
    pub fn new(store:Arc<Store>,embed:Arc<EmbeddingClient>,workspace:PathBuf)->Self{Self{store,embed,workspace,locks:ParkingMutex::new(HashMap::new())}}
    fn project_lock(&self,project:&str)->Arc<Mutex<()>>{
        let mut locks=self.locks.lock(); locks.entry(project.to_string()).or_insert_with(||Arc::new(Mutex::new(()))).clone()
    }
    pub async fn rebuild(&self,project:&str)->Result<IndexManifest>{
        let lock=self.project_lock(project); let _guard=lock.lock().await;
        RetrievalEngine::build(&self.store,&self.embed,&self.workspace,project).await
    }
    pub async fn ensure_index(&self,project:&str)->Result<IndexManifest>{
        let fingerprint=self.store.retrieval_fingerprint(project)?;
        if let Ok(engine)=RetrievalEngine::open(&self.workspace,project){if engine.manifest().fingerprint==fingerprint{return Ok(engine.manifest().clone());}}
        let lock=self.project_lock(project); let _guard=lock.lock().await;
        let fingerprint=self.store.retrieval_fingerprint(project)?;
        if let Ok(engine)=RetrievalEngine::open(&self.workspace,project){if engine.manifest().fingerprint==fingerprint{return Ok(engine.manifest().clone());}}
        RetrievalEngine::build(&self.store,&self.embed,&self.workspace,project).await
    }
    pub async fn search(&self,project:&str,query:&str,k:usize)->Result<Vec<RetrievalHit>>{
        self.ensure_index(project).await?; let q=self.embed.embed_query(query).await?; let engine=RetrievalEngine::open(&self.workspace,project)?; engine.retrieve(&q,query,k)
    }
    pub fn status(&self,project:&str)->Result<serde_json::Value>{
        let fingerprint=self.store.retrieval_fingerprint(project)?;
        match RetrievalEngine::open(&self.workspace,project){
            Ok(e)=>Ok(serde_json::json!({"ready":true,"fresh":e.manifest().fingerprint==fingerprint,"manifest":e.manifest(),"requirement_nodes":e.requirement_count(),"record_rows":e.record_count()})),
            Err(_)=>Ok(serde_json::json!({"ready":false,"fresh":false,"fingerprint":fingerprint}))
        }
    }
}

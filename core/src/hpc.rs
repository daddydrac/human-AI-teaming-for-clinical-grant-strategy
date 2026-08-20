use serde_json::json;
use std::time::Instant;
use rayon::prelude::*;

extern "C" {
    fn hpc_normalize_rows(data: *mut f32, rows: usize, cols: usize);
    fn hpc_sgemv_scores(matrix: *const f32, query: *const f32, scores: *mut f32, rows: usize, cols: usize);
    fn hpc_weighted_fuse(semantic:*const f32, lexical:*const f32, evidence:*const f32, freshness:*const f32, out:*mut f32, n:usize, ws:f32, wl:f32, we:f32, wf:f32);
    fn hpc_topk_indices(scores:*const f32, n:usize, k:usize, out:*mut u32);
}

pub fn normalize_rows(data:&mut [f32], rows:usize, cols:usize) {
    assert_eq!(data.len(), rows*cols);
    unsafe { hpc_normalize_rows(data.as_mut_ptr(), rows, cols); }
}

pub fn scores(matrix:&[f32], query:&[f32], rows:usize, cols:usize)->Vec<f32> {
    assert_eq!(matrix.len(), rows*cols); assert_eq!(query.len(), cols);
    let mut out=vec![0.0f32;rows];
    unsafe { hpc_sgemv_scores(matrix.as_ptr(), query.as_ptr(), out.as_mut_ptr(), rows, cols); }
    out
}

pub fn fuse(a:&[f32],b:&[f32],c:&[f32],d:&[f32],weights:[f32;4])->Vec<f32>{
    let n=a.len(); assert!(b.len()==n&&c.len()==n&&d.len()==n);
    let mut out=vec![0.0;n];
    unsafe{hpc_weighted_fuse(a.as_ptr(),b.as_ptr(),c.as_ptr(),d.as_ptr(),out.as_mut_ptr(),n,weights[0],weights[1],weights[2],weights[3])};
    out
}


pub fn openmp_topk(scores:&[f32],k:usize)->Vec<(usize,f32)> {
    if scores.is_empty() || k==0 { return Vec::new(); }
    let kk=k.min(scores.len()); let mut idx=vec![0u32;kk];
    unsafe{hpc_topk_indices(scores.as_ptr(),scores.len(),kk,idx.as_mut_ptr())};
    idx.into_iter().map(|i|(i as usize,scores[i as usize])).collect()
}

pub fn parallel_topk(scores:&[f32], k:usize)->Vec<(usize,f32)> {
    if scores.is_empty() || k==0 { return Vec::new(); }
    let chunks=std::thread::available_parallelism().map(|n|n.get()).unwrap_or(1).max(1);
    let chunk_size=(scores.len()+chunks-1)/chunks;
    let mut candidates:Vec<(usize,f32)>=scores.par_chunks(chunk_size).enumerate().flat_map_iter(|(ci,ch)| {
        let base=ci*chunk_size; let mut v:Vec<_>=ch.iter().copied().enumerate().map(|(i,s)|(base+i,s)).collect();
        v.sort_unstable_by(|a,b|b.1.total_cmp(&a.1)); v.truncate(k.min(v.len())); v
    }).collect();
    candidates.sort_unstable_by(|a,b|b.1.total_cmp(&a.1)); candidates.truncate(k.min(candidates.len())); candidates
}

pub fn max_threads()->i32 { std::env::var("OMP_NUM_THREADS").ok().and_then(|x|x.parse().ok()).unwrap_or_else(|| std::thread::available_parallelism().map(|n|n.get() as i32).unwrap_or(1)) }

pub fn self_benchmark()->serde_json::Value {
    let rows=50_000usize; let cols=384usize;
    let mut m=vec![0.001f32;rows*cols]; let mut q=vec![0.01f32;cols];
    let t=Instant::now(); normalize_rows(&mut m,rows,cols); normalize_rows(&mut q,1,cols); let norm_ms=t.elapsed().as_secs_f64()*1000.0;
    let t=Instant::now(); let s=scores(&m,&q,rows,cols); let score_ms=t.elapsed().as_secs_f64()*1000.0;
    let t=Instant::now(); let f=fuse(&s,&s,&s,&s,[0.45,0.25,0.20,0.10]); let fuse_ms=t.elapsed().as_secs_f64()*1000.0;
    let t=Instant::now(); let top_rayon=parallel_topk(&f,10); let rayon_topk_ms=t.elapsed().as_secs_f64()*1000.0;
    let t=Instant::now(); let top_openmp=openmp_topk(&f,10); let openmp_topk_ms=t.elapsed().as_secs_f64()*1000.0;
    json!({"rows":rows,"dims":cols,"normalize_ms":norm_ms,"sgemv_ms":score_ms,"fuse_ms":fuse_ms,"rayon_topk_ms":rayon_topk_ms,"openmp_topk_ms":openmp_topk_ms,"threads_hint":max_threads(),"checksum":f.iter().take(32).sum::<f32>(),"top0_rayon":top_rayon.first().map(|x|x.0),"top0_openmp":top_openmp.first().map(|x|x.0)})
}

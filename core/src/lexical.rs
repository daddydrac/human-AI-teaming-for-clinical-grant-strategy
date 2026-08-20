use anyhow::{bail, Result};
use memmap2::{Mmap, MmapOptions};
use std::{collections::{BTreeMap, HashMap}, fs::{File, OpenOptions}, io::Write, path::Path};

const MAGIC_LEX: u64 = 0x4752414E544C4558; // GRANTLEX
const MAGIC_POST: u64 = 0x4752414E54504F53; // GRANTPOS
const MAGIC_LEN: u64 = 0x4752414E544C454E; // GRANTLEN
const HEADER: usize = 16;
const LEX_ENTRY: usize = 24; // hash u64, offset u64, len u32, df u32
const POST_ENTRY: usize = 8; // row u32, tf u16, pad u16

#[derive(Debug, Clone, Copy)]
struct LexEntry { hash:u64, offset:u64, len:u32, df:u32 }

pub struct LexicalIndex {
    lex: Mmap,
    postings: Mmap,
    lengths: Mmap,
    docs: usize,
    avg_len: f32,
}

fn fnv1a64(s:&str)->u64 {
    let mut h=0xcbf29ce484222325u64;
    for b in s.as_bytes(){ h^=*b as u64; h=h.wrapping_mul(0x100000001b3); }
    h
}

pub fn tokenize(text:&str)->Vec<u64>{
    let mut out=Vec::new(); let mut buf=String::new();
    for ch in text.chars(){
        if ch.is_alphanumeric(){ for c in ch.to_lowercase(){buf.push(c);} }
        else if !buf.is_empty(){ if buf.len()>1 {out.push(fnv1a64(&buf));} buf.clear(); }
    }
    if !buf.is_empty() && buf.len()>1 {out.push(fnv1a64(&buf));}
    out
}

pub fn build(dir:&Path, texts:&[String])->Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut inverted:BTreeMap<u64,Vec<(u32,u16)>>=BTreeMap::new();
    let mut lengths=Vec::<u32>::with_capacity(texts.len());
    for (row,text) in texts.iter().enumerate(){
        let toks=tokenize(text); lengths.push(toks.len().min(u32::MAX as usize) as u32);
        let mut tf:HashMap<u64,u32>=HashMap::new();
        for t in toks { *tf.entry(t).or_default()+=1; }
        for (term,count) in tf { inverted.entry(term).or_default().push((row as u32,count.min(u16::MAX as u32) as u16)); }
    }
    let mut post=OpenOptions::new().create(true).truncate(true).write(true).open(dir.join("bm25.postings"))?;
    post.write_all(&MAGIC_POST.to_le_bytes())?; post.write_all(&(0u64).to_le_bytes())?;
    let mut entries=Vec::<LexEntry>::with_capacity(inverted.len()); let mut offset=0u64;
    for (hash,list) in &inverted {
        entries.push(LexEntry{hash:*hash,offset,len:list.len() as u32,df:list.len() as u32});
        for (row,tf) in list { post.write_all(&row.to_le_bytes())?; post.write_all(&tf.to_le_bytes())?; post.write_all(&0u16.to_le_bytes())?; offset+=1; }
    }
    post.flush()?;
    let mut lex=OpenOptions::new().create(true).truncate(true).write(true).open(dir.join("bm25.lexicon"))?;
    lex.write_all(&MAGIC_LEX.to_le_bytes())?; lex.write_all(&(entries.len() as u64).to_le_bytes())?;
    for e in entries { lex.write_all(&e.hash.to_le_bytes())?; lex.write_all(&e.offset.to_le_bytes())?; lex.write_all(&e.len.to_le_bytes())?; lex.write_all(&e.df.to_le_bytes())?; }
    lex.flush()?;
    let mut lf=OpenOptions::new().create(true).truncate(true).write(true).open(dir.join("bm25.lengths"))?;
    lf.write_all(&MAGIC_LEN.to_le_bytes())?; lf.write_all(&(lengths.len() as u64).to_le_bytes())?;
    for x in lengths {lf.write_all(&x.to_le_bytes())?;} lf.flush()?;
    Ok(())
}

impl LexicalIndex {
    pub fn open(dir:&Path)->Result<Self>{
        let lex=unsafe{MmapOptions::new().map(&File::open(dir.join("bm25.lexicon"))?)?};
        let postings=unsafe{MmapOptions::new().map(&File::open(dir.join("bm25.postings"))?)?};
        let lengths=unsafe{MmapOptions::new().map(&File::open(dir.join("bm25.lengths"))?)?};
        if lex.len()<HEADER || postings.len()<HEADER || lengths.len()<HEADER {bail!("short lexical index");}
        let u64at=|m:&[u8],o:usize|u64::from_le_bytes(m[o..o+8].try_into().unwrap());
        if u64at(&lex,0)!=MAGIC_LEX || u64at(&postings,0)!=MAGIC_POST || u64at(&lengths,0)!=MAGIC_LEN {bail!("bad lexical index magic");}
        let docs=u64at(&lengths,8) as usize;
        if lengths.len()!=HEADER+docs*4 {bail!("bad length table size");}
        let mut total=0u64; for i in 0..docs {total+=u32::from_le_bytes(lengths[HEADER+i*4..HEADER+i*4+4].try_into().unwrap()) as u64;}
        let avg_len=if docs==0{1.0}else{(total as f32/docs as f32).max(1.0)};
        Ok(Self{lex,postings,lengths,docs,avg_len})
    }
    fn count(&self)->usize {u64::from_le_bytes(self.lex[8..16].try_into().unwrap()) as usize}
    fn entry(&self,i:usize)->LexEntry{
        let o=HEADER+i*LEX_ENTRY; LexEntry{
            hash:u64::from_le_bytes(self.lex[o..o+8].try_into().unwrap()),
            offset:u64::from_le_bytes(self.lex[o+8..o+16].try_into().unwrap()),
            len:u32::from_le_bytes(self.lex[o+16..o+20].try_into().unwrap()),
            df:u32::from_le_bytes(self.lex[o+20..o+24].try_into().unwrap()),
        }
    }
    fn find(&self,h:u64)->Option<LexEntry>{
        let mut lo=0usize; let mut hi=self.count(); while lo<hi {let mid=(lo+hi)/2; let e=self.entry(mid); if e.hash<h{lo=mid+1}else{hi=mid;}}
        if lo<self.count(){let e=self.entry(lo); if e.hash==h{return Some(e);}} None
    }
    fn doc_len(&self,row:usize)->f32{ if row>=self.docs{return self.avg_len;} let o=HEADER+row*4; u32::from_le_bytes(self.lengths[o..o+4].try_into().unwrap()) as f32 }
    pub fn scores(&self,query:&str,k1:f32,b:f32)->Vec<f32>{
        let mut scores=vec![0.0f32;self.docs]; let mut qtf=HashMap::<u64,u32>::new(); for t in tokenize(query){*qtf.entry(t).or_default()+=1;}
        let n=self.docs.max(1) as f32; let k1=k1.max(0.01); let b=b.clamp(0.0,1.0);
        for (term,_) in qtf { if let Some(e)=self.find(term){
            let df=e.df.max(1) as f32; let idf=((n-df+0.5)/(df+0.5)+1.0).ln();
            for j in 0..e.len as usize {let o=HEADER+(e.offset as usize+j)*POST_ENTRY; if o+POST_ENTRY>self.postings.len(){break;} let row=u32::from_le_bytes(self.postings[o..o+4].try_into().unwrap()) as usize; let tf=u16::from_le_bytes(self.postings[o+4..o+6].try_into().unwrap()) as f32; if row>=scores.len(){continue;} let dl=self.doc_len(row); scores[row]+=idf*(tf*(k1+1.0))/(tf+k1*(1.0-b+b*dl/self.avg_len)); }
        }}
        let max=scores.iter().copied().fold(0.0f32,f32::max); if max>0.0 {for s in &mut scores{*s/=max;}} scores
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bm25_prefers_matching_document(){
        let dir=std::env::temp_dir().join(format!("grant-lex-{}",uuid::Uuid::new_v4()));
        build(&dir,&["pancreatic cancer biomarker response".into(),"budget administrative timeline".into()]).unwrap();
        let idx=LexicalIndex::open(&dir).unwrap(); let s=idx.scores("cancer biomarker",1.2,0.75); assert!(s[0]>s[1]); let _=std::fs::remove_dir_all(dir);
    }
}

use anyhow::{bail, Result};
use memmap2::{Mmap,MmapOptions};
use std::{collections::{BTreeMap,HashMap},fs::{File,OpenOptions},io::Write,path::Path};

const MAGIC_OFF:u64=0x4752414E54435352; // GRANTCSR
const MAGIC_EDG:u64=0x4752414E54454447; // GRANTEDG
const HEADER:usize=16;

pub fn build(dir:&Path, requirement_ids:&[String], record_requirement_ids:&[Option<String>])->Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut id_to_idx=HashMap::new(); for (i,id) in requirement_ids.iter().enumerate(){id_to_idx.insert(id.clone(),i);}
    let mut buckets:Vec<Vec<u32>>=vec![Vec::new();requirement_ids.len()];
    for (row,rid) in record_requirement_ids.iter().enumerate(){if let Some(r)=rid{if let Some(&i)=id_to_idx.get(r){buckets[i].push(row as u32);}}}
    let mut off=OpenOptions::new().create(true).truncate(true).write(true).open(dir.join("requirement.offsets"))?;
    off.write_all(&MAGIC_OFF.to_le_bytes())?; off.write_all(&(requirement_ids.len() as u64).to_le_bytes())?;
    let mut cursor=0u64; off.write_all(&cursor.to_le_bytes())?; for b in &buckets {cursor+=b.len() as u64;off.write_all(&cursor.to_le_bytes())?;} off.flush()?;
    let mut ed=OpenOptions::new().create(true).truncate(true).write(true).open(dir.join("requirement.edges"))?;
    ed.write_all(&MAGIC_EDG.to_le_bytes())?; ed.write_all(&cursor.to_le_bytes())?; for b in buckets {for row in b{ed.write_all(&row.to_le_bytes())?;}} ed.flush()?;
    std::fs::write(dir.join("requirement.ids.json"),serde_json::to_vec(requirement_ids)?)?; Ok(())
}

pub struct RequirementCsr { offsets:Mmap, edges:Mmap, ids:Vec<String>, map:BTreeMap<String,usize> }
impl RequirementCsr {
    pub fn open(dir:&Path)->Result<Self>{
        let offsets=unsafe{MmapOptions::new().map(&File::open(dir.join("requirement.offsets"))?)?}; let edges=unsafe{MmapOptions::new().map(&File::open(dir.join("requirement.edges"))?)?};
        if offsets.len()<HEADER || edges.len()<HEADER {bail!("short CSR");}
        let magic=|m:&[u8]|u64::from_le_bytes(m[0..8].try_into().unwrap()); if magic(&offsets)!=MAGIC_OFF||magic(&edges)!=MAGIC_EDG{bail!("bad CSR magic");}
        let ids:Vec<String>=serde_json::from_slice(&std::fs::read(dir.join("requirement.ids.json"))?)?; let map=ids.iter().enumerate().map(|(i,s)|(s.clone(),i)).collect(); Ok(Self{offsets,edges,ids,map})
    }
    pub fn rows_for(&self,id:&str)->Vec<u32>{
        let Some(&i)=self.map.get(id) else{return Vec::new()}; let o=HEADER+i*8; let start=u64::from_le_bytes(self.offsets[o..o+8].try_into().unwrap()) as usize; let end=u64::from_le_bytes(self.offsets[o+8..o+16].try_into().unwrap()) as usize; let mut out=Vec::with_capacity(end.saturating_sub(start)); for j in start..end {let p=HEADER+j*4; out.push(u32::from_le_bytes(self.edges[p..p+4].try_into().unwrap()));} out
    }
    pub fn requirement_count(&self)->usize{self.ids.len()}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expands_requirement_rows(){
        let dir=std::env::temp_dir().join(format!("grant-csr-{}",uuid::Uuid::new_v4()));
        build(&dir,&["R-1".into(),"R-2".into()],&[Some("R-1".into()),None,Some("R-1".into()),Some("R-2".into())]).unwrap();
        let g=RequirementCsr::open(&dir).unwrap(); assert_eq!(g.rows_for("R-1"),vec![0,2]); assert_eq!(g.rows_for("R-2"),vec![3]); let _=std::fs::remove_dir_all(dir);
    }
}

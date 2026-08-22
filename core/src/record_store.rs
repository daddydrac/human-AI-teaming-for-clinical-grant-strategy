use crate::domain::RetrievalRecord;
use anyhow::{bail, Result};
use memmap2::{Mmap, MmapOptions};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
};

const MAGIC_BLOB: u64 = 0x4752414E54524543; // GRANTREC
const MAGIC_OFF: u64 = 0x4752414E544F4646; // GRANTOFF
const HEADER: usize = 16;

pub fn build(dir: &Path, records: &[RetrievalRecord]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut blob = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(dir.join("records.blob"))?;
    blob.write_all(&MAGIC_BLOB.to_le_bytes())?;
    blob.write_all(&(records.len() as u64).to_le_bytes())?;
    let mut offsets = Vec::<u64>::with_capacity(records.len() + 1);
    let mut cursor = 0u64;
    offsets.push(cursor);
    for r in records {
        let bytes = serde_json::to_vec(r)?;
        blob.write_all(&bytes)?;
        cursor += bytes.len() as u64;
        offsets.push(cursor);
    }
    blob.flush()?;
    let mut of = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(dir.join("records.offsets"))?;
    of.write_all(&MAGIC_OFF.to_le_bytes())?;
    of.write_all(&(records.len() as u64).to_le_bytes())?;
    for x in offsets {
        of.write_all(&x.to_le_bytes())?;
    }
    of.flush()?;
    Ok(())
}

pub struct MmapRecordStore {
    blob: Mmap,
    offsets: Mmap,
    rows: usize,
}
impl MmapRecordStore {
    pub fn open(dir: &Path) -> Result<Self> {
        let blob = unsafe { MmapOptions::new().map(&File::open(dir.join("records.blob"))?)? };
        let offsets = unsafe { MmapOptions::new().map(&File::open(dir.join("records.offsets"))?)? };
        if blob.len() < HEADER || offsets.len() < HEADER {
            bail!("short record store");
        }
        let u64at = |m: &[u8], o: usize| u64::from_le_bytes(m[o..o + 8].try_into().unwrap());
        if u64at(&blob, 0) != MAGIC_BLOB || u64at(&offsets, 0) != MAGIC_OFF {
            bail!("bad record store magic");
        }
        let rows = u64at(&offsets, 8) as usize;
        if offsets.len() != HEADER + (rows + 1) * 8 {
            bail!("bad record offset table size");
        }
        Ok(Self {
            blob,
            offsets,
            rows,
        })
    }
    pub fn len(&self) -> usize {
        self.rows
    }
    pub fn get(&self, row: usize) -> Result<RetrievalRecord> {
        if row >= self.rows {
            bail!("record row out of range");
        }
        let off = |i: usize| {
            u64::from_le_bytes(
                self.offsets[HEADER + i * 8..HEADER + i * 8 + 8]
                    .try_into()
                    .unwrap(),
            ) as usize
        };
        let s = HEADER + off(row);
        let e = HEADER + off(row + 1);
        if e > self.blob.len() || s > e {
            bail!("corrupt record offsets");
        }
        Ok(serde_json::from_slice(&self.blob[s..e])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mmap_roundtrip() {
        let dir = std::env::temp_dir().join(format!("grant-rec-{}", uuid::Uuid::new_v4()));
        let r = RetrievalRecord {
            row: 0,
            item_id: "x".into(),
            kind: "evidence".into(),
            requirement_id: Some("R-1".into()),
            source_ref: "s".into(),
            source_url: None,
            source_locator: None,
            text: "hello".into(),
            confidence: 1.0,
            status: "supported".into(),
            created_unix: Some(0),
        };
        build(&dir, &[r.clone()]).unwrap();
        let store = MmapRecordStore::open(&dir).unwrap();
        assert_eq!(store.get(0).unwrap().text, "hello");
        let _ = std::fs::remove_dir_all(dir);
    }
}

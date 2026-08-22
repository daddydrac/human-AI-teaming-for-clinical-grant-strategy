use crate::hpc;
use anyhow::{bail, Context, Result};
use memmap2::{Mmap, MmapMut, MmapOptions};
use std::{
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::Path,
};

const MAGIC: u64 = 0x4752414E54564543; // "GRANTVEC"
const HEADER: usize = 24;

pub struct MmapMatrixWriter {
    mmap: MmapMut,
    pub rows: usize,
    pub cols: usize,
}
impl MmapMatrixWriter {
    pub fn create(path: &Path, rows: usize, cols: usize) -> Result<Self> {
        if rows == 0 || cols == 0 {
            bail!("matrix dimensions must be non-zero");
        }
        let bytes = HEADER
            .checked_add(
                rows.checked_mul(cols)
                    .context("matrix size overflow")?
                    .checked_mul(4)
                    .context("matrix byte-size overflow")?,
            )
            .context("matrix byte-size overflow")?;
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path)?;
        f.set_len(bytes as u64)?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&MAGIC.to_le_bytes())?;
        f.write_all(&(rows as u64).to_le_bytes())?;
        f.write_all(&(cols as u64).to_le_bytes())?;
        let mmap = unsafe { MmapOptions::new().map_mut(&f)? };
        Ok(Self { mmap, rows, cols })
    }
    pub fn write_rows(&mut self, start_row: usize, data: &[f32]) -> Result<()> {
        if data.len() % self.cols != 0 {
            bail!("batch does not contain whole rows");
        }
        let batch_rows = data.len() / self.cols;
        if start_row + batch_rows > self.rows {
            bail!("batch exceeds matrix rows");
        }
        let mut tmp = data.to_vec();
        hpc::normalize_rows(&mut tmp, batch_rows, self.cols);
        let byte_start = HEADER + (start_row * self.cols * 4);
        let byte_len = tmp.len() * 4;
        let src = unsafe { std::slice::from_raw_parts(tmp.as_ptr() as *const u8, byte_len) };
        self.mmap[byte_start..byte_start + byte_len].copy_from_slice(src);
        Ok(())
    }
    pub fn finish(self) -> Result<()> {
        self.mmap.flush()?;
        Ok(())
    }
}

pub struct MmapMatrix {
    mmap: Mmap,
    pub rows: usize,
    pub cols: usize,
}
impl MmapMatrix {
    pub fn create_normalized(path: &Path, rows: usize, cols: usize, data: &[f32]) -> Result<()> {
        if data.len() != rows * cols {
            bail!("matrix size mismatch");
        }
        let bytes = HEADER + data.len() * 4;
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path)?;
        f.set_len(bytes as u64)?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&MAGIC.to_le_bytes())?;
        f.write_all(&(rows as u64).to_le_bytes())?;
        f.write_all(&(cols as u64).to_le_bytes())?;
        let mut mmap = unsafe { MmapOptions::new().map_mut(&f)? };
        let dst = unsafe {
            std::slice::from_raw_parts_mut(mmap.as_mut_ptr().add(HEADER) as *mut f32, data.len())
        };
        dst.copy_from_slice(data);
        hpc::normalize_rows(dst, rows, cols);
        mmap.flush()?;
        Ok(())
    }
    pub fn open(path: &Path) -> Result<Self> {
        let f = File::open(path)?;
        let mmap = unsafe { MmapOptions::new().map(&f)? };
        if mmap.len() < HEADER {
            bail!("short vector file");
        }
        let read_u64 = |off: usize| u64::from_le_bytes(mmap[off..off + 8].try_into().unwrap());
        if read_u64(0) != MAGIC {
            bail!("bad vector magic");
        }
        let rows = read_u64(8) as usize;
        let cols = read_u64(16) as usize;
        if mmap.len() != HEADER + rows * cols * 4 {
            bail!("vector file length mismatch");
        }
        Ok(Self { mmap, rows, cols })
    }
    pub fn scores(&self, query: &[f32]) -> Result<Vec<f32>> {
        if query.len() != self.cols {
            bail!("query dimension mismatch");
        }
        let matrix = unsafe {
            std::slice::from_raw_parts(
                self.mmap.as_ptr().add(HEADER) as *const f32,
                self.rows * self.cols,
            )
        };
        let mut q = query.to_vec();
        hpc::normalize_rows(&mut q, 1, self.cols);
        Ok(hpc::scores(matrix, &q, self.rows, self.cols))
    }
}

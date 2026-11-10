//! UltraISO compressed images (ISZ): a header, an obfuscated chunk table, then
//! chunks stored, zeroed or zlib-compressed. Presented as the plain ISO.
//! (Password-protected and bzip2-compressed images are reported as unsupported.)

use crate::sevenzip::Source;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Clone, Copy, Debug)]
enum Kind {
    Zero,
    Stored,
    Zlib,
    Bzip2,
}

pub struct Isz {
    f: File,
    total: u64,
    chunk: u64,
    /// (kind, compressed size, file offset)
    chunks: Vec<(Kind, u64, u64)>,
    cache: Option<(usize, Vec<u8>)>,
    pos: u64,
}

pub fn open(path: &Path) -> Option<Box<dyn Source>> {
    let mut f = File::open(path).ok()?;
    let mut h = [0u8; 48];
    f.read_exact(&mut h).ok()?;
    if &h[0..4] != b"IsZ!" {
        return None;
    }
    let u16_at = |i: usize| u16::from_le_bytes([h[i], h[i + 1]]) as u64;
    let u32_at = |i: usize| u32::from_le_bytes(h[i..i + 4].try_into().unwrap()) as u64;
    let sect = u16_at(10);
    let total_sectors = u32_at(12);
    let password = h[16];
    let nblocks = u32_at(25) as usize;
    let block = u32_at(29);
    let ptr_len = h[33] as usize;
    let ptr_offs = u32_at(35);
    let data_offs = u32_at(43);
    if password != 0 || ptr_len == 0 || ptr_len > 4 || block == 0 || nblocks == 0 || nblocks > 1 << 24 {
        return None;
    }
    let mut table = vec![0u8; nblocks * ptr_len];
    f.seek(SeekFrom::Start(ptr_offs)).ok()?;
    f.read_exact(&mut table).ok()?;
    const KEY: &[u8; 4] = b"IsZ!";
    for (i, b) in table.iter_mut().enumerate() {
        *b ^= !KEY[i & 3];
    }
    let bits = ptr_len * 8;
    let mut chunks = Vec::with_capacity(nblocks);
    let mut off = data_offs;
    for k in 0..nblocks {
        let mut v = 0u64;
        for j in 0..ptr_len {
            v |= (table[k * ptr_len + j] as u64) << (8 * j);
        }
        let kind = match v >> (bits - 2) {
            0 => Kind::Zero,
            1 => Kind::Stored,
            2 => Kind::Zlib,
            _ => Kind::Bzip2,
        };
        let size = v & ((1u64 << (bits - 2)) - 1);
        chunks.push((kind, size, off));
        if !matches!(kind, Kind::Zero) {
            off += size;
        }
    }
    let total = total_sectors * sect.max(1);
    Some(Box::new(Isz { f, total, chunk: block, chunks, cache: None, pos: 0 }))
}

impl Isz {
    fn chunk_data(&mut self, k: usize) -> std::io::Result<&[u8]> {
        if self.cache.as_ref().map(|(i, _)| *i) != Some(k) {
            let (kind, size, off) = self.chunks[k];
            let want = self.chunk.min(self.total - k as u64 * self.chunk) as usize;
            let data = match kind {
                Kind::Zero => vec![0u8; want],
                Kind::Stored | Kind::Zlib => {
                    let mut raw = vec![0u8; size as usize];
                    self.f.seek(SeekFrom::Start(off))?;
                    self.f.read_exact(&mut raw)?;
                    if let Kind::Zlib = kind {
                        miniz_oxide::inflate::decompress_to_vec_zlib(&raw).map_err(|_| std::io::Error::other("corrupt ISZ chunk"))?
                    } else {
                        raw
                    }
                }
                Kind::Bzip2 => return Err(std::io::Error::other("bzip2-compressed ISZ images are not supported")),
            };
            self.cache = Some((k, data));
        }
        Ok(&self.cache.as_ref().unwrap().1)
    }
}

impl Read for Isz {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.total || out.is_empty() {
            return Ok(0);
        }
        let k = (self.pos / self.chunk) as usize;
        let within = (self.pos % self.chunk) as usize;
        let pos = self.pos;
        let total = self.total;
        let data = self.chunk_data(k)?;
        let n = (data.len().saturating_sub(within)).min(out.len()).min((total - pos) as usize);
        out[..n].copy_from_slice(&data[within..within + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Isz {
    fn seek(&mut self, p: SeekFrom) -> std::io::Result<u64> {
        let np = match p {
            SeekFrom::Start(v) => v as i64,
            SeekFrom::Current(d) => self.pos as i64 + d,
            SeekFrom::End(d) => self.total as i64 + d,
        };
        if np < 0 {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        self.pos = np as u64;
        Ok(self.pos)
    }
}

impl Source for Isz {
    fn len(&self) -> u64 {
        self.total
    }
}

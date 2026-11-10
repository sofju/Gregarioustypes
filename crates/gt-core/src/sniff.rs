//! Content sniffing helpers. Extensions are only hints; providers confirm with magic bytes.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// First bytes of a file (up to `n`).
pub fn read_head(path: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; n];
    let mut len = 0;
    while len < n {
        let r = f.read(&mut buf[len..])?;
        if r == 0 {
            break;
        }
        len += r;
    }
    buf.truncate(len);
    Ok(buf)
}

/// Last `n` bytes of a file.
pub fn read_tail(path: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    let start = size.saturating_sub(n as u64);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((size - start) as usize);
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

/// Lower-case extension without the dot ("tar.gz" style double extensions are handled by callers).
pub fn ext(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// MPEG transport stream: 0x47 sync byte every 188 (or 192 for M2TS) bytes.
pub fn is_mpeg_ts(head: &[u8]) -> bool {
    for (stride, off) in [(188usize, 0usize), (192, 4)] {
        let n = (head.len().saturating_sub(off)) / stride;
        if n >= 4 && (0..n.min(16)).all(|i| head[off + i * stride] == 0x47) {
            return true;
        }
    }
    false
}

/// Torrent metainfo: a bencoded dictionary with an "info" key near the start.
pub fn is_torrent(head: &[u8]) -> bool {
    head.first() == Some(&b'd') && head.windows(6).take(64 * 1024).any(|w| w == b"4:info")
}

/// Windows System Information export (msinfo32 .nfo) is XML.
pub fn is_msinfo_xml(head: &[u8]) -> bool {
    let s = String::from_utf8_lossy(&head[..head.len().min(4096)]).to_ascii_lowercase();
    let s16 = if head.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = head[2..head.len().min(8192)].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units).to_ascii_lowercase()
    } else {
        String::new()
    };
    (s.contains("<?xml") && s.contains("<msinfo")) || (s16.contains("<?xml") && s16.contains("<msinfo"))
}

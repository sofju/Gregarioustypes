//! Streaming listers for formats inside compressed streams (tar, cpio): they read
//! headers as the data is decompressed, so `.tar.gz` & co. list without a temp file.

use gt_core::CancelToken;
use std::io::Read;

#[derive(Clone, Debug, Default)]
pub struct StreamEntry {
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
    /// Unix seconds.
    pub mtime: i64,
    pub link: String,
}

fn read_full(r: &mut dyn Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Skip `n` bytes, or copy them into `sink` when given.
fn pass(r: &mut dyn Read, mut n: u64, mut sink: Option<&mut dyn std::io::Write>) -> std::io::Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    while n > 0 {
        let k = (n as usize).min(buf.len());
        let got = read_full(r, &mut buf[..k])?;
        if got == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        if let Some(s) = sink.as_deref_mut() {
            s.write_all(&buf[..got])?;
        }
        n -= got as u64;
    }
    Ok(())
}

fn octal(b: &[u8]) -> u64 {
    if !b.is_empty() && b[0] & 0x80 != 0 {
        // GNU base-256.
        let mut v: u64 = (b[0] & 0x7F) as u64;
        for &c in &b[1..] {
            v = (v << 8) | c as u64;
        }
        return v;
    }
    let mut v = 0u64;
    for &c in b {
        if (b'0'..=b'7').contains(&c) {
            v = v * 8 + (c - b'0') as u64;
        } else if c == 0 || c == b' ' {
            if v > 0 {
                break;
            }
        }
    }
    v
}

fn cstr(b: &[u8]) -> String {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    match std::str::from_utf8(&b[..n]) {
        Ok(s) => s.to_string(),
        Err(_) => gt_core::encoding::decode_1252(&b[..n]),
    }
}

pub fn is_tar(head: &[u8]) -> bool {
    head.len() >= 512 && (&head[257..262] == b"ustar" || {
        // Old V7 tar: valid header checksum.
        let sum: u64 = head[..148].iter().chain([b' '; 8].iter()).chain(head[156..512].iter()).map(|&b| b as u64).sum();
        let stored = octal(&head[148..156]);
        stored != 0 && stored == sum
    })
}

/// Walk a tar stream. With `extract = Some((ordinal, sink))`, copies that entry's
/// data and stops; otherwise lists every entry through `on`.
pub fn tar(r: &mut dyn Read, cancel: &CancelToken, mut on: impl FnMut(StreamEntry), mut extract: Option<(usize, &mut dyn std::io::Write)>) -> Result<(), String> {
    let mut hdr = [0u8; 512];
    let mut long_name: Option<String> = None;
    let mut long_link: Option<String> = None;
    let mut pax_path: Option<String> = None;
    let mut pax_size: Option<u64> = None;
    let mut pax_link: Option<String> = None;
    let mut ordinal = 0usize;
    let mut zeros = 0;
    loop {
        if cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        let n = read_full(r, &mut hdr).map_err(|e| e.to_string())?;
        if n < 512 {
            return Ok(());
        }
        if hdr.iter().all(|&b| b == 0) {
            zeros += 1;
            if zeros >= 2 {
                return Ok(());
            }
            continue;
        }
        zeros = 0;
        let kind = hdr[156];
        let mut size = octal(&hdr[124..136]);
        if let Some(s) = pax_size.take() {
            if !matches!(kind, b'x' | b'g' | b'L' | b'K') {
                size = s;
            }
        }
        let padded = size.div_ceil(512) * 512;
        match kind {
            b'L' | b'K' | b'x' | b'g' => {
                let mut data = vec![0u8; padded as usize];
                read_full(r, &mut data).map_err(|e| e.to_string())?;
                data.truncate(size as usize);
                match kind {
                    b'L' => long_name = Some(cstr(&data)),
                    b'K' => long_link = Some(cstr(&data)),
                    b'x' => {
                        // "len key=value\n" records.
                        let text = String::from_utf8_lossy(&data);
                        for rec in text.split('\n') {
                            let Some((_, kv)) = rec.split_once(' ') else { continue };
                            let Some((k, v)) = kv.split_once('=') else { continue };
                            match k {
                                "path" => pax_path = Some(v.to_string()),
                                "linkpath" => pax_link = Some(v.to_string()),
                                "size" => pax_size = v.parse().ok(),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                continue;
            }
            _ => {}
        }
        let mut path = cstr(&hdr[0..100]);
        if &hdr[257..262] == b"ustar" && hdr[345] != 0 {
            let prefix = cstr(&hdr[345..500]);
            if !prefix.is_empty() {
                path = format!("{prefix}/{path}");
            }
        }
        if let Some(p) = long_name.take().or(pax_path.take()) {
            path = p;
        }
        let link = long_link.take().or(pax_link.take()).unwrap_or_else(|| cstr(&hdr[157..257]));
        let is_dir = kind == b'5' || path.ends_with('/');
        let data_len = if matches!(kind, b'0' | 0 | b'7') { size } else { 0 };
        let e = StreamEntry { path: path.trim_end_matches('/').trim_start_matches("./").to_string(), size: data_len, is_dir, mtime: octal(&hdr[136..148]) as i64, link: if kind == b'2' || kind == b'1' { link } else { String::new() } };
        if let Some((want, sink)) = extract.as_mut() {
            if *want == ordinal {
                pass(r, size, Some(&mut **sink)).map_err(|e| e.to_string())?;
                return Ok(());
            }
        } else if !e.path.is_empty() && e.path != "." {
            on(e);
        }
        ordinal += 1;
        pass(r, padded, None).map_err(|e| e.to_string())?;
    }
}

pub fn is_cpio(head: &[u8]) -> bool {
    head.starts_with(b"070701") || head.starts_with(b"070702") || head.starts_with(b"070707") || head.starts_with(&[0xC7, 0x71]) || head.starts_with(&[0x71, 0xC7])
}

/// Walk a cpio stream (newc/crc, odc, binary).
pub fn cpio(r: &mut dyn Read, cancel: &CancelToken, mut on: impl FnMut(StreamEntry), mut extract: Option<(usize, &mut dyn std::io::Write)>) -> Result<(), String> {
    let mut ordinal = 0usize;
    let mut pos: u64 = 0;
    loop {
        if cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        let mut magic = [0u8; 6];
        if read_full(r, &mut magic[..2]).map_err(|e| e.to_string())? < 2 {
            return Ok(());
        }
        let (namesize, filesize, mode, mtime, align): (u64, u64, u32, i64, u64);
        if magic[0] == 0xC7 || magic[0] == 0x71 {
            let le = magic[0] == 0xC7;
            let mut h = [0u8; 24];
            read_full(r, &mut h).map_err(|e| e.to_string())?;
            let w = |i: usize| -> u32 {
                if le {
                    u16::from_le_bytes([h[i], h[i + 1]]) as u32
                } else {
                    u16::from_be_bytes([h[i], h[i + 1]]) as u32
                }
            };
            mode = w(2);
            mtime = ((w(14) << 16) | w(16)) as i64;
            namesize = w(18) as u64;
            filesize = ((w(20) as u64) << 16) | w(22) as u64;
            align = 2;
            pos += 26;
        } else {
            read_full(r, &mut magic[2..]).map_err(|e| e.to_string())?;
            if &magic == b"070707" {
                let mut h = [0u8; 70];
                read_full(r, &mut h).map_err(|e| e.to_string())?;
                let o = |a: usize, b: usize| u64::from_str_radix(std::str::from_utf8(&h[a..b]).unwrap_or("0"), 8).unwrap_or(0);
                mode = o(12, 18) as u32;
                mtime = o(42, 53) as i64;
                namesize = o(53, 59);
                filesize = o(59, 70);
                align = 1;
                pos += 76;
            } else if &magic == b"070701" || &magic == b"070702" {
                let mut h = [0u8; 104];
                read_full(r, &mut h).map_err(|e| e.to_string())?;
                let x = |i: usize| u64::from_str_radix(std::str::from_utf8(&h[i * 8..i * 8 + 8]).unwrap_or("0"), 16).unwrap_or(0);
                mode = x(1) as u32;
                mtime = x(5) as i64;
                filesize = x(6);
                namesize = x(11);
                align = 4;
                pos += 110;
            } else {
                return Err("not a cpio archive".into());
            }
        }
        let mut name = vec![0u8; namesize as usize];
        read_full(r, &mut name).map_err(|e| e.to_string())?;
        pos += namesize;
        let pad = |p: u64| (align - p % align) % align;
        let p1 = pad(pos);
        pass(r, p1, None).map_err(|e| e.to_string())?;
        pos += p1;
        let name = cstr(&name);
        if name == "TRAILER!!!" {
            return Ok(());
        }
        let kind = mode & 0o170000;
        let is_dir = kind == 0o040000;
        let mut link = String::new();
        if let Some((want, sink)) = extract.as_mut() {
            if *want == ordinal {
                pass(r, filesize, Some(&mut **sink)).map_err(|e| e.to_string())?;
                return Ok(());
            }
            pass(r, filesize, None).map_err(|e| e.to_string())?;
        } else if kind == 0o120000 && filesize < 4096 {
            let mut t = vec![0u8; filesize as usize];
            read_full(r, &mut t).map_err(|e| e.to_string())?;
            link = cstr(&t);
        } else {
            pass(r, filesize, None).map_err(|e| e.to_string())?;
        }
        pos += filesize;
        let p2 = pad(pos);
        pass(r, p2, None).map_err(|e| e.to_string())?;
        pos += p2;
        let path = name.trim_start_matches("./").trim_end_matches('/').to_string();
        if extract.is_none() && !path.is_empty() && path != "." {
            on(StreamEntry { path, size: if kind == 0o100000 { filesize } else { 0 }, is_dir, mtime, link });
        }
        ordinal += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_header(name: &str, size: u64, kind: u8) -> Vec<u8> {
        let mut h = vec![0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        h[136..148].copy_from_slice(b"14000000000\0");
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h[148..156].copy_from_slice(b"        ");
        let sum: u64 = h.iter().map(|&b| b as u64).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        h
    }

    #[test]
    fn tar_list_and_extract() {
        let mut t = tar_header("dir/", 0, b'5');
        t.extend(tar_header("dir/a.txt", 5, b'0'));
        t.extend(b"hello");
        t.extend(vec![0u8; 507]);
        t.extend(vec![0u8; 1024]);
        assert!(is_tar(&t));
        let mut v = Vec::new();
        tar(&mut &t[..], &CancelToken::new(), |e| v.push(e), None).unwrap();
        assert_eq!(v.len(), 2);
        assert!(v[0].is_dir);
        assert_eq!(v[1].path, "dir/a.txt");
        assert_eq!(v[1].size, 5);
        let mut out = Vec::new();
        tar(&mut &t[..], &CancelToken::new(), |_| {}, Some((1, &mut out))).unwrap();
        assert_eq!(out, b"hello");
    }

    #[test]
    fn cpio_newc() {
        let mut c = Vec::new();
        let mut entry = |name: &str, data: &[u8], mode: u32| {
            let n = format!("{name}\0");
            c.extend(format!("070701{:08X}{:08X}{}{:08X}{}{:08X}{}", 1, mode, "00000000".repeat(3), 0, "", data.len(), "00000000".repeat(4)).as_bytes());
            c.extend(format!("{:08X}{:08X}", n.len(), 0).as_bytes());
            c.extend(n.as_bytes());
            while c.len() % 4 != 0 {
                c.push(0);
            }
            c.extend(data);
            while c.len() % 4 != 0 {
                c.push(0);
            }
        };
        entry("a.txt", b"abc", 0o100644);
        entry("TRAILER!!!", b"", 0);
        assert!(is_cpio(&c));
        let mut v = Vec::new();
        cpio(&mut &c[..], &CancelToken::new(), |e| v.push(e), None).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].path, "a.txt");
        assert_eq!(v[0].size, 3);
    }
}

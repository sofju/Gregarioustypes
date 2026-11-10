//! Package metadata: Debian `control` fields and RPM header tags.

/// "Field: value" pairs from a Debian control file (continuation lines folded).
pub fn deb_control(text: &str) -> (Vec<(String, String)>, Option<String>) {
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut description: Option<String> = None;
    let mut in_desc = false;
    for line in text.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if in_desc {
                let l = line.trim();
                let d = description.get_or_insert_with(String::new);
                if !d.is_empty() {
                    d.push('\n');
                }
                d.push_str(if l == "." { "" } else { l });
            } else if let Some(last) = fields.last_mut() {
                last.1.push(' ');
                last.1.push_str(line.trim());
            }
            continue;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        in_desc = k == "Description";
        if in_desc {
            fields.push(("Summary".into(), v.trim().to_string()));
        } else {
            fields.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    (fields, description)
}

fn be32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// Selected tags from an RPM's main header: (fields, description).
pub fn rpm_header(b: &[u8]) -> Option<(Vec<(String, String)>, Option<String>)> {
    if b.len() < 96 || b[0..4] != [0xED, 0xAB, 0xEE, 0xDB] {
        return None;
    }
    // Lead (96 bytes), signature header (8-byte aligned), then the main header.
    let header_at = |off: usize| -> Option<(usize, usize, usize)> {
        if b.get(off..off + 3)? != [0x8E, 0xAD, 0xE8] {
            return None;
        }
        let n = be32(b, off + 8)? as usize;
        let data = be32(b, off + 12)? as usize;
        Some((n, data, off + 16))
    };
    let (sn, sd, sidx) = header_at(96)?;
    let mut main = sidx + sn * 16 + sd;
    main = (main + 7) & !7;
    let (n, _dlen, idx) = header_at(main)?;
    let store = idx + n * 16;
    let mut out = Vec::new();
    let mut description = None;
    let mut size = None;
    for k in 0..n.min(4096) {
        let e = idx + k * 16;
        let tag = be32(b, e)?;
        let ty = be32(b, e + 4)?;
        let off = be32(b, e + 8)? as usize;
        let str_at = |o: usize| -> String {
            let s = &b[(store + o).min(b.len())..];
            let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
            String::from_utf8_lossy(&s[..end]).into_owned()
        };
        let name = match tag {
            1000 => "Name",
            1001 => "Version",
            1002 => "Release",
            1004 => "Summary",
            1005 => "Description",
            1006 => "Build date",
            1009 => "Installed size",
            1014 => "License",
            1015 => "Packager",
            1016 => "Group",
            1020 => "URL",
            1021 => "OS",
            1022 => "Architecture",
            1044 => "Source package",
            1125 => "Payload compression",
            _ => continue,
        };
        let value = match ty {
            6 | 8 | 9 => str_at(off),
            4 => {
                let v = be32(b, store + off).unwrap_or(0);
                match tag {
                    1006 => gt_core::fmt::unix_time(v as i64),
                    1009 => {
                        size = Some(v);
                        gt_core::fmt::size(v as u64)
                    }
                    _ => v.to_string(),
                }
            }
            _ => continue,
        };
        if tag == 1005 {
            description = Some(value);
        } else if !value.is_empty() {
            out.push((name.to_string(), value));
        }
    }
    let _ = size;
    // Name, version and release first.
    let order = ["Name", "Version", "Release", "Architecture", "Summary"];
    out.sort_by_key(|(k, _)| order.iter().position(|o| o == k).unwrap_or(order.len()));
    Some((out, description))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_fields() {
        let (f, d) = deb_control("Package: x\nVersion: 1.0\nDepends: a,\n b\nDescription: short\n long line\n .\n more\n");
        assert_eq!(f[0], ("Package".into(), "x".into()));
        assert_eq!(f[2], ("Depends".into(), "a, b".into()));
        assert_eq!(f[3], ("Summary".into(), "short".into()));
        assert_eq!(d.unwrap(), "long line\n\nmore");
    }

    #[test]
    fn rpm_sample() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/archives/downloaded/libarchive/cpio-gzip.rpm");
        let Ok(b) = std::fs::read(&p) else { return };
        let (f, _) = rpm_header(&b).unwrap();
        assert!(f.iter().any(|(k, v)| k == "Name" && v == "rpmsample"), "{f:?}");
    }
}

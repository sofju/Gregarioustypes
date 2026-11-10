//! Document properties and embedded thumbnails, read without converting:
//! OLE property sets (.doc/.xls/.ppt), ODF `meta.xml`, OOXML `docProps/*.xml`,
//! and iWork bundles.

use crate::pages::Raster;
use std::io::Read;
use std::path::Path;

pub struct Props {
    pub facts: Vec<(String, String)>,
    pub thumbnail: Option<Raster>,
    /// "text", "presentation", "spreadsheet", "drawing".
    pub kind: Option<&'static str>,
    /// Password-protected (encrypted) document: nothing to show without the password.
    pub encrypted: bool,
}

fn decode(bytes: &[u8]) -> Option<Raster> {
    let b = gt_fmt_media::cover::decode_bytes(bytes, 1024)?;
    Some(Raster { w: b.width, h: b.height, data: b.data })
}

fn push(f: &mut Vec<(String, String)>, k: &str, v: String) {
    let v = v.trim().to_string();
    if !v.is_empty() && !f.iter().any(|(k2, _)| k2 == k) {
        f.push((k.to_string(), v));
    }
}

/// Text of the first `<tag ...>text</tag>` (namespace prefix included in `tag`).
fn tag(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}");
    let mut from = 0;
    while let Some(i) = xml[from..].find(&open) {
        let s = from + i + open.len();
        let next = xml[s..].chars().next()?;
        if next == '>' || next == ' ' {
            let gt = s + xml[s..].find('>')?;
            if xml[..gt].ends_with('/') {
                return None;
            }
            let close = format!("</{name}>");
            let end = gt + 1 + xml[gt + 1..].find(&close)?;
            return Some(unescape(&xml[gt + 1..end]));
        }
        from = s;
    }
    None
}

fn attr(xml: &str, name: &str) -> Option<String> {
    let k = format!("{name}=\"");
    let i = xml.find(&k)? + k.len();
    let j = xml[i..].find('"')?;
    Some(unescape(&xml[i..i + j]))
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// "2024-03-05T10:20:00Z" → local short date.
fn iso_date(s: &str) -> Option<String> {
    let d = s.trim();
    let n = |a: usize, b: usize| d.get(a..b).and_then(|x| x.parse::<u16>().ok());
    let pdf = format!("D:{:04}{:02}{:02}{:02}{:02}{}", n(0, 4)?, n(5, 7)?, n(8, 10)?, n(11, 13).unwrap_or(0), n(14, 16).unwrap_or(0), if d.ends_with('Z') { "Z" } else { "" });
    let pdf = if d.ends_with('Z') { format!("{}+00'00'", pdf.trim_end_matches('Z')) } else { pdf };
    crate::pdf::pdf_date(&pdf)
}

fn zip_entry(z: &mut zip::ZipArchive<std::fs::File>, name: &str, max: u64) -> Option<Vec<u8>> {
    let mut f = z.by_name(name).ok()?;
    if f.size() > max {
        return None;
    }
    let mut v = Vec::with_capacity(f.size() as usize);
    f.read_to_end(&mut v).ok()?;
    Some(v)
}

pub fn read(path: &Path, head: &[u8]) -> Props {
    let mut p = Props { facts: Vec::new(), thumbnail: None, kind: None, encrypted: false };
    if head.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        ole(path, &mut p);
    } else if head.starts_with(b"PK") {
        if let Ok(f) = std::fs::File::open(path) {
            if let Ok(mut z) = zip::ZipArchive::new(f) {
                zipped(&mut z, &mut p);
            }
        }
    }
    p
}

fn zipped(z: &mut zip::ZipArchive<std::fs::File>, p: &mut Props) {
    // ODF
    if let Some(mime) = zip_entry(z, "mimetype", 256) {
        let m = String::from_utf8_lossy(&mime).to_string();
        p.kind = if m.contains("presentation") {
            Some("presentation")
        } else if m.contains("spreadsheet") {
            Some("spreadsheet")
        } else if m.contains("graphics") {
            Some("drawing")
        } else {
            Some("text")
        };
        if let Some(meta) = zip_entry(z, "meta.xml", 4 << 20) {
            let x = String::from_utf8_lossy(&meta);
            push(&mut p.facts, "Title", tag(&x, "dc:title").unwrap_or_default());
            push(&mut p.facts, "Author", tag(&x, "meta:initial-creator").or_else(|| tag(&x, "dc:creator")).unwrap_or_default());
            if let Some(i) = x.find("<meta:document-statistic") {
                let st = &x[i..x[i..].find('>').map(|j| i + j).unwrap_or(x.len())];
                for (a, k) in [("meta:page-count", "Pages"), ("meta:word-count", "Words")] {
                    if let Some(v) = attr(st, a) {
                        push(&mut p.facts, k, v);
                    }
                }
            }
            if let Some(d) = tag(&x, "meta:creation-date").and_then(|d| iso_date(&d)) {
                push(&mut p.facts, "Created", d);
            }
            if let Some(d) = tag(&x, "dc:date").and_then(|d| iso_date(&d)) {
                push(&mut p.facts, "Modified", d);
            }
            push(&mut p.facts, "Application", tag(&x, "meta:generator").map(|g| g.split('$').next().unwrap_or("").replace('/', " ")).unwrap_or_default());
        }
        if let Some(t) = zip_entry(z, "Thumbnails/thumbnail.png", 8 << 20) {
            p.thumbnail = decode(&t);
        }
        return;
    }
    // OOXML
    let names: Vec<String> = z.file_names().filter_map(|s| s.ok().map(|s| s.to_string())).collect();
    let has = |n: &str| names.iter().any(|x| x == n);
    if has("[Content_Types].xml") {
        p.kind = if names.iter().any(|n| n.starts_with("ppt/")) {
            Some("presentation")
        } else if names.iter().any(|n| n.starts_with("xl/")) {
            Some("spreadsheet")
        } else {
            Some("text")
        };
        if let Some(core) = zip_entry(z, "docProps/core.xml", 4 << 20) {
            let x = String::from_utf8_lossy(&core);
            push(&mut p.facts, "Title", tag(&x, "dc:title").unwrap_or_default());
            push(&mut p.facts, "Author", tag(&x, "dc:creator").unwrap_or_default());
            if let Some(d) = tag(&x, "dcterms:created").and_then(|d| iso_date(&d)) {
                push(&mut p.facts, "Created", d);
            }
            if let Some(d) = tag(&x, "dcterms:modified").and_then(|d| iso_date(&d)) {
                push(&mut p.facts, "Modified", d);
            }
            push(&mut p.facts, "Last saved by", tag(&x, "cp:lastModifiedBy").unwrap_or_default());
        }
        if let Some(app) = zip_entry(z, "docProps/app.xml", 4 << 20) {
            let x = String::from_utf8_lossy(&app);
            for (t, k) in [("Pages", "Pages"), ("Slides", "Slides"), ("Words", "Words")] {
                push(&mut p.facts, k, tag(&x, t).filter(|v| v != "0").unwrap_or_default());
            }
            let ver = tag(&x, "AppVersion").map(|v| format!(" {v}")).unwrap_or_default();
            push(&mut p.facts, "Application", tag(&x, "Application").map(|a| format!("{a}{ver}")).unwrap_or_default());
        }
        for n in ["docProps/thumbnail.jpeg", "docProps/thumbnail.jpg", "docProps/thumbnail.png"] {
            if let Some(t) = zip_entry(z, n, 8 << 20) {
                p.thumbnail = decode(&t);
                break;
            }
        }
        return;
    }
    // iWork (Pages, Keynote, Numbers)
    for n in ["preview.jpg", "QuickLook/Preview.jpg", "QuickLook/Thumbnail.jpg", "preview-web.jpg"] {
        if let Some(t) = zip_entry(z, n, 16 << 20) {
            p.thumbnail = decode(&t);
            break;
        }
    }
}

/// OLE property-set value (`\x05SummaryInformation`, `\x05DocumentSummaryInformation`).
enum PropVal {
    Str(String),
    Int(i64),
    Time(u64),
}

fn property_set(data: &[u8]) -> Vec<(u32, PropVal)> {
    let mut out = Vec::new();
    let u16_at = |o: usize| data.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |o: usize| data.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if u16_at(0) != Some(0xFFFE) {
        return out;
    }
    let codepage_utf16 = false;
    // First section only.
    let Some(sec) = u32_at(28 + 16).map(|v| v as usize) else { return out };
    let Some(count) = u32_at(sec + 4) else { return out };
    let mut cp = 1252u32;
    let mut entries = Vec::new();
    for i in 0..count.min(256) as usize {
        let (Some(id), Some(off)) = (u32_at(sec + 8 + i * 8), u32_at(sec + 12 + i * 8)) else { break };
        entries.push((id, sec + off as usize));
    }
    for &(id, o) in &entries {
        if id == 1 {
            if let Some(v) = u16_at(o + 4) {
                cp = v as u32;
            }
        }
    }
    let _ = codepage_utf16;
    for (id, o) in entries {
        let Some(ty) = u32_at(o) else { continue };
        let v = match ty & 0xFFFF {
            2 => u16_at(o + 4).map(|v| PropVal::Int(v as i16 as i64)),
            3 => u32_at(o + 4).map(|v| PropVal::Int(v as i32 as i64)),
            0x1E => u32_at(o + 4).and_then(|n| {
                let b = data.get(o + 8..o + 8 + (n as usize).min(4096))?;
                let b = b.split(|&c| c == 0).next().unwrap_or(b);
                let s = if cp == 1200 {
                    let w: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                    String::from_utf16_lossy(&w)
                } else if cp == 65001 {
                    String::from_utf8_lossy(b).to_string()
                } else {
                    gt_core::encoding::decode_codepage(b, cp)
                };
                Some(PropVal::Str(s))
            }),
            0x1F => u32_at(o + 4).and_then(|n| {
                let b = data.get(o + 8..o + 8 + (n as usize * 2).min(8192))?;
                let w: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
                Some(PropVal::Str(String::from_utf16_lossy(&w)))
            }),
            0x40 => data.get(o + 4..o + 12).map(|b| PropVal::Time(u64::from_le_bytes(b.try_into().unwrap()))),
            _ => None,
        };
        if let Some(v) = v {
            out.push((id, v));
        }
    }
    out
}

fn ole(path: &Path, p: &mut Props) {
    let Ok(mut c) = cfb::open(path) else { return };
    let streams: Vec<String> = c.walk().map(|e| e.path().to_string_lossy().trim_start_matches(['/', '\\']).to_string()).collect();
    let has = |n: &str| streams.iter().any(|s| s.eq_ignore_ascii_case(n));
    p.kind = if has("WordDocument") {
        Some("text")
    } else if has("PowerPoint Document") {
        Some("presentation")
    } else if has("Workbook") || has("Book") {
        Some("spreadsheet")
    } else if has("VisioDocument") {
        Some("drawing")
    } else {
        None
    };
    // Encrypted OOXML is an OLE container with these streams; Word and PowerPoint
    // binaries flag encryption in their own headers.
    if (has("EncryptionInfo") && has("EncryptedPackage")) || has("EncryptedSummary") {
        p.encrypted = true;
    }
    let mut read = |name: &str| -> Option<Vec<u8>> {
        let mut s = c.open_stream(name).ok()?;
        let mut v = Vec::new();
        s.by_ref().take(1 << 20).read_to_end(&mut v).ok()?;
        Some(v)
    };
    if p.kind == Some("spreadsheet") {
        // BIFF: a FILEPASS record (0x002F) right after the workbook's BOF.
        if let Some(wb) = read("Workbook").or_else(|| read("Book")) {
            let mut o = 0usize;
            for _ in 0..16 {
                if o + 4 > wb.len() {
                    break;
                }
                let ty = u16::from_le_bytes([wb[o], wb[o + 1]]);
                let len = u16::from_le_bytes([wb[o + 2], wb[o + 3]]) as usize;
                if ty == 0x002F {
                    p.encrypted = true;
                    break;
                }
                o += 4 + len;
            }
        }
    }
    if p.kind == Some("text") {
        // FIB: fEncrypted is bit 8 of the flags word at offset 0x0A.
        if let Some(fib) = read("WordDocument") {
            if fib.len() > 12 && u16::from_le_bytes([fib[10], fib[11]]) & 0x0100 != 0 {
                p.encrypted = true;
            }
        }
    }
    if let Some(si) = read("\u{5}SummaryInformation") {
        for (id, v) in property_set(&si) {
            let key = match id {
                2 => "Title",
                3 => "Subject",
                4 => "Author",
                8 => "Last saved by",
                18 => "Application",
                12 => "Created",
                13 => "Modified",
                14 => "Pages",
                15 => "Words",
                _ => continue,
            };
            match v {
                PropVal::Str(s) => push(&mut p.facts, key, s),
                PropVal::Int(n) if n > 0 => push(&mut p.facts, key, gt_core::fmt::group(n as u64)),
                // FILETIME; times before 1990 are edit durations or junk.
                PropVal::Time(t) if t > 125_911_584_000_000_000 => push(&mut p.facts, key, gt_core::fmt::filetime_short(t)),
                _ => {}
            }
        }
    }
    if let Some(dsi) = read("\u{5}DocumentSummaryInformation") {
        for (id, v) in property_set(&dsi) {
            let key = match id {
                7 => "Slides",
                15 => "Company",
                _ => continue,
            };
            match v {
                PropVal::Str(s) => push(&mut p.facts, key, s),
                PropVal::Int(n) if n > 0 => push(&mut p.facts, key, gt_core::fmt::group(n as u64)),
                _ => {}
            }
        }
    }
    // Facts in a stable order.
    let order = ["Title", "Subject", "Author", "Company", "Pages", "Slides", "Words", "Created", "Modified", "Last saved by", "Application"];
    p.facts.sort_by_key(|(k, _)| order.iter().position(|o| o == k).unwrap_or(99));
}

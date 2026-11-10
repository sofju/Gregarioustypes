//! Recognising the formats the page loads itself, and their facts.

use serde_json::Value;
use std::io::Read;
use std::path::Path;

pub fn is_glb(head: &[u8]) -> bool {
    head.len() >= 12 && &head[0..4] == b"glTF" && u32::from_le_bytes(head[4..8].try_into().unwrap()) == 2
}

pub fn is_gltf_json(head: &[u8]) -> bool {
    let t = String::from_utf8_lossy(&head[..head.len().min(8192)]);
    let t = t.trim_start_matches('\u{feff}').trim_start();
    t.starts_with('{') && t.contains("\"asset\"")
}

/// The glTF JSON document (whole .gltf, or the JSON chunk of a .glb).
fn gltf_json(path: &Path, head: &[u8], size: u64) -> Option<Value> {
    if is_glb(head) {
        let len = u32::from_le_bytes(head.get(12..16)?.try_into().ok()?) as usize;
        if head.len() >= 20 + len {
            return serde_json::from_slice(&head[20..20 + len]).ok();
        }
        let mut f = std::fs::File::open(path).ok()?;
        let mut b = vec![0u8; 20 + len.min(256 << 20)];
        f.read_exact(&mut b).ok()?;
        return serde_json::from_slice(&b[20..]).ok();
    }
    if size > 128 << 20 {
        return None;
    }
    let b = std::fs::read(path).ok()?;
    serde_json::from_slice(&b).ok()
}

pub fn gltf_facts(path: &Path, head: &[u8], size: u64) -> Vec<(String, String)> {
    let mut f = vec![("Format".to_string(), if is_glb(head) { "glTF 2.0 binary".to_string() } else { "glTF 2.0".to_string() })];
    let Some(j) = gltf_json(path, head, size) else { return f };
    let a = &j["asset"];
    if let Some(v) = a["version"].as_str().filter(|v| *v != "2.0") {
        f[0].1 = format!("glTF {v}");
    }
    if let Some(g) = a["generator"].as_str().filter(|s| !s.is_empty()) {
        f.push(("Generator".into(), g.to_string()));
    }
    if let Some(c) = a["copyright"].as_str().filter(|s| !s.is_empty()) {
        f.push(("Copyright".into(), c.to_string()));
    }
    let list = |k: &str| j[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).map(String::from).collect::<Vec<_>>()).unwrap_or_default();
    let used = list("extensionsUsed");
    if !used.is_empty() {
        f.push(("Extensions".into(), used.join(", ")));
    }
    let n = |k: &str| j[k].as_array().map(|a| a.len()).unwrap_or(0);
    if n("scenes") > 1 {
        f.push(("Scenes".into(), n("scenes").to_string()));
    }
    let external = ["buffers", "images"].iter().flat_map(|k| j[*k].as_array().cloned().unwrap_or_default()).filter(|x| x["uri"].as_str().is_some_and(|u| !u.starts_with("data:"))).count();
    if external > 0 {
        f.push(("External files".into(), external.to_string()));
    }
    if n("cameras") > 0 {
        f.push(("Cameras".into(), n("cameras").to_string()));
    }
    f
}

pub const USD_EXTS: &[&str] = &["usd", "usda", "usdc", "usdz"];

pub fn is_usd(head: &[u8], ext: &str) -> bool {
    head.starts_with(b"#usda ") || head.starts_with(b"PXR-USDC") || (ext == "usdz" && head.starts_with(b"PK\x03\x04"))
}

/// `key = value` from a USDA layer's metadata block.
fn usda_meta(text: &str, key: &str) -> Option<String> {
    let head = &text[..text.len().min(16 << 10)];
    let i = head.find(&format!("{key} ="))? + key.len() + 2;
    let rest = head[i..].trim_start();
    if let Some(r) = rest.strip_prefix("\"\"\"") {
        return r.find("\"\"\"").map(|j| r[..j].trim().to_string());
    }
    if let Some(r) = rest.strip_prefix('"') {
        return r.find('"').map(|j| r[..j].to_string());
    }
    let end = rest.find(|c: char| c == '\n' || c == ')').unwrap_or(rest.len());
    Some(rest[..end].trim().to_string())
}

fn usda_facts(text: &str, f: &mut Vec<(String, String)>) {
    for (k, label) in [("defaultPrim", "Default prim"), ("upAxis", "Up axis"), ("metersPerUnit", "Metres per unit"), ("doc", "Description")] {
        if let Some(v) = usda_meta(text, k).filter(|v| !v.is_empty()) {
            f.push((label.into(), v.chars().take(200).collect()));
        }
    }
}

pub fn usd_facts(path: &Path, head: &[u8]) -> Vec<(String, String)> {
    let mut f = Vec::new();
    if head.starts_with(b"#usda ") {
        let line = String::from_utf8_lossy(&head[..head.len().min(32)]).lines().next().unwrap_or("").trim_start_matches('#').to_string();
        f.push(("Format".into(), format!("USD text ({line})")));
        usda_facts(&String::from_utf8_lossy(head), &mut f);
    } else if head.starts_with(b"PXR-USDC") && head.len() > 11 {
        f.push(("Format".into(), format!("USD crate {}.{}.{}", head[8], head[9], head[10])));
    } else if head.starts_with(b"PK") {
        f.push(("Format".into(), "USDZ package".into()));
        if let Ok(mut z) = std::fs::File::open(path).map_err(|_| ()).and_then(|r| zip::ZipArchive::new(r).map_err(|_| ())) {
            let names: Vec<String> = z.file_names().filter_map(|n| n.ok().map(|s| s.to_string())).collect();
            if let Some(first) = names.first() {
                f.push(("Root layer".into(), first.clone()));
                if let Ok(mut e) = z.by_index(0) {
                    let mut b = vec![0u8; (e.size() as usize).min(16 << 10)];
                    if e.read_exact(&mut b).is_ok() && b.starts_with(b"#usda ") {
                        usda_facts(&String::from_utf8_lossy(&b), &mut f);
                    }
                }
            }
            let images = names.iter().filter(|n| matches!(n.rsplit('.').next().map(|e| e.to_ascii_lowercase()).as_deref(), Some("png" | "jpg" | "jpeg" | "exr" | "avif"))).count();
            f.push(("Contents".into(), format!("{} files, {images} images", names.len())));
        }
    }
    f
}

/// Formats three.js reads directly, besides glTF and USD.
pub fn extra_kind(head: &[u8], ext: &str) -> Option<&'static str> {
    let text = || String::from_utf8_lossy(&head[..head.len().min(4096)]).to_string();
    match ext {
        "dae" if text().contains("<COLLADA") => Some("dae"),
        "ply" if head.starts_with(b"ply\n") || head.starts_with(b"ply\r\n") => Some("ply"),
        "3mf" if head.starts_with(b"PK\x03\x04") => Some("3mf"),
        "3ds" if head.len() > 6 && head[0] == 0x4D && head[1] == 0x4D => Some("3ds"),
        _ => None,
    }
}

pub fn extra_facts(kind: &str, head: &[u8]) -> Vec<(String, String)> {
    let mut f = vec![(
        "Format".to_string(),
        match kind {
            "dae" => "COLLADA",
            "ply" => "Stanford PLY",
            "3mf" => "3D Manufacturing Format",
            _ => "3D Studio",
        }
        .to_string(),
    )];
    let t = String::from_utf8_lossy(&head[..head.len().min(16 << 10)]).to_string();
    if kind == "dae" {
        if let Some(i) = t.find("<authoring_tool>") {
            if let Some(j) = t[i..].find("</authoring_tool>") {
                f.push(("Created with".into(), t[i + 16..i + j].trim().to_string()));
            }
        }
        if let Some(i) = t.find("<up_axis>") {
            if let Some(j) = t[i..].find("</up_axis>") {
                f.push(("Up axis".into(), t[i + 9..i + j].trim().trim_end_matches("_UP").to_string()));
            }
        }
    } else if kind == "ply" {
        if let Some(l) = t.lines().find(|l| l.starts_with("format ")) {
            f[0].1 = format!("Stanford PLY ({})", l.trim_start_matches("format ").split_whitespace().next().unwrap_or("").replace('_', " "));
        }
        for l in t.lines().take_while(|l| !l.starts_with("end_header")) {
            if let Some(r) = l.strip_prefix("element ") {
                let mut it = r.split_whitespace();
                if let (Some(name), Some(n)) = (it.next(), it.next().and_then(|n| n.parse::<u64>().ok())) {
                    let label = match name {
                        "vertex" => "Vertices in file",
                        "face" => "Faces in file",
                        _ => continue,
                    };
                    f.push((label.into(), gt_core::fmt::group(n)));
                }
            }
        }
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usda_header() {
        let t = "#usda 1.0\n(\n    defaultPrim = \"World\"\n    metersPerUnit = 0.01\n    upAxis = \"Z\"\n    doc = \"\"\"Made by hand\"\"\"\n)\n";
        let mut f = Vec::new();
        usda_facts(t, &mut f);
        assert_eq!(f, vec![("Default prim".to_string(), "World".to_string()), ("Up axis".into(), "Z".into()), ("Metres per unit".into(), "0.01".into()), ("Description".into(), "Made by hand".into())]);
    }

    #[test]
    fn sniff() {
        assert!(is_glb(b"glTF\x02\0\0\0\x10\0\0\0"));
        assert!(is_gltf_json(b"{\n  \"asset\": {\"version\": \"2.0\"}}"));
        assert_eq!(extra_kind(b"ply\nformat ascii 1.0\n", "ply"), Some("ply"));
        assert_eq!(extra_kind(b"MM\x10\0\0\0\x02\0", "3ds"), Some("3ds"));
    }
}

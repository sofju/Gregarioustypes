//! STL (binary and ASCII) to GLB. Facets share their corners (exact matches),
//! no normals are written so the page shades them flat, the way CAD and slicer
//! tools show STL. Z up (3D printing) becomes glTF's Y up.

use crate::glb::{Glb, ARRAY_BUFFER};
use crate::Model;
use gt_core::CancelToken;
use serde_json::json;
use std::collections::HashMap;

pub struct Stl {
    pub triangles: Vec<[[f32; 3]; 3]>,
    pub binary: bool,
    pub name: String,
    /// Per-facet colours (VisCAM/SolidView or Materialise conventions).
    pub colors: Option<Vec<[u8; 3]>>,
}

fn binary_count(d: &[u8], size: u64) -> Option<u32> {
    if d.len() < 84 {
        return None;
    }
    let n = u32::from_le_bytes(d[80..84].try_into().ok()?);
    (84 + 50 * n as u64 == size).then_some(n)
}

pub fn sniff(head: &[u8], size: u64) -> bool {
    if binary_count(head, size).is_some() {
        return true;
    }
    let t = String::from_utf8_lossy(&head[..head.len().min(2048)]).to_ascii_lowercase();
    t.trim_start().starts_with("solid") && t.contains("facet")
}

pub fn parse(d: &[u8], cancel: &CancelToken) -> Result<Stl, String> {
    if let Some(n) = binary_count(d, d.len() as u64) {
        // Materialise Magics: "COLOR=" in the header, bit 15 clear = facet colour (BGR).
        let magics = d[..80].windows(6).any(|w| w == b"COLOR=");
        // Readable text only (Magics follows "COLOR=" with binary values).
        let text: String = d[..80].iter().take_while(|b| **b >= 0x20 && **b < 0x7F).map(|b| *b as char).collect();
        let header = text.split("COLOR=").next().unwrap_or("").trim().to_string();
        let mut tris = Vec::with_capacity(n as usize);
        let mut colors = Vec::with_capacity(n as usize);
        let mut coloured = 0usize;
        for (i, f) in d[84..].chunks_exact(50).enumerate() {
            if i % 65536 == 0 && cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            let r = |o: usize| f32::from_le_bytes(f[o..o + 4].try_into().unwrap());
            tris.push([[r(12), r(16), r(20)], [r(24), r(28), r(32)], [r(36), r(40), r(44)]]);
            let a = u16::from_le_bytes([f[48], f[49]]);
            let c5 = |v: u16| ((v & 31) as u32 * 255 / 31) as u8;
            let valid = if magics { a & 0x8000 == 0 } else { a & 0x8000 != 0 };
            if valid && a != 0 {
                coloured += 1;
                colors.push(if magics { [c5(a), c5(a >> 5), c5(a >> 10)] } else { [c5(a >> 10), c5(a >> 5), c5(a)] });
            } else {
                colors.push([204, 204, 204]);
            }
        }
        let colors = (coloured * 2 > tris.len().max(1)).then_some(colors);
        return Ok(Stl { triangles: tris, binary: true, name: header, colors });
    }
    let text = String::from_utf8_lossy(d);
    let mut name = String::new();
    let mut tris = Vec::new();
    let mut cur: Vec<[f32; 3]> = Vec::with_capacity(3);
    for (i, line) in text.lines().enumerate() {
        if i % 200_000 == 0 && cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        let l = line.trim_start();
        if let Some(rest) = l.strip_prefix("vertex") {
            let mut it = rest.split_whitespace().map(|x| x.parse::<f32>().unwrap_or(0.0));
            cur.push([it.next().unwrap_or(0.0), it.next().unwrap_or(0.0), it.next().unwrap_or(0.0)]);
        } else if l.starts_with("endloop") {
            // Polygons with more than three corners: fan.
            for k in 1..cur.len().saturating_sub(1) {
                tris.push([cur[0], cur[k], cur[k + 1]]);
            }
            cur.clear();
        } else if let Some(n) = l.strip_prefix("solid") {
            if name.is_empty() {
                name = n.trim().to_string();
            }
        }
    }
    if tris.is_empty() {
        return Err("No facets in this STL file".into());
    }
    Ok(Stl { triangles: tris, binary: false, name, colors: None })
}

pub fn convert(d: &[u8], cancel: &CancelToken) -> Result<Model, String> {
    let stl = parse(d, cancel)?;
    let mut pos: Vec<f32> = Vec::new();
    let mut col: Vec<[f32; 4]> = Vec::new();
    let mut idx: Vec<u32> = Vec::with_capacity(stl.triangles.len() * 3);
    if let Some(c) = &stl.colors {
        // Coloured facets keep their own corners.
        for (t, c) in stl.triangles.iter().zip(c) {
            for v in t {
                idx.push((pos.len() / 3) as u32);
                pos.extend([v[0], v[2], -v[1]]);
                col.push([c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, 1.0]);
            }
        }
    } else {
        let mut seen: HashMap<[u32; 3], u32> = HashMap::with_capacity(stl.triangles.len() * 3 / 5);
        for (i, t) in stl.triangles.iter().enumerate() {
            if i % 65536 == 0 && cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            for v in t {
                let key = [v[0].to_bits(), v[1].to_bits(), v[2].to_bits()];
                let next = (pos.len() / 3) as u32;
                let id = *seen.entry(key).or_insert_with(|| {
                    pos.extend([v[0], v[2], -v[1]]);
                    next
                });
                idx.push(id);
            }
        }
    }
    let mut g = Glb::new("Gregarioustypes (STL)");
    let p = g.floats(&pos, 3, true, Some(ARRAY_BUFFER));
    let mut attrs = json!({ "POSITION": p });
    if !col.is_empty() {
        attrs["COLOR_0"] = json!(g.colors(&col));
    }
    let i = g.indices(&idx);
    let base = if col.is_empty() { [0.72, 0.74, 0.78, 1.0] } else { [1.0; 4] };
    g.materials.push(json!({ "name": "STL", "pbrMetallicRoughness": { "baseColorFactor": base, "metallicFactor": 0.0, "roughnessFactor": 0.55 }, "doubleSided": true }));
    g.meshes.push(json!({ "name": stl.name, "primitives": [ { "attributes": attrs, "indices": i, "material": 0 } ] }));
    let n = g.node(json!({ "name": if stl.name.is_empty() { "STL".to_string() } else { stl.name.clone() }, "mesh": 0 }));
    g.scene_nodes.push(n);
    let mut facts = vec![("Format".to_string(), if stl.binary { "Binary STL".to_string() } else { "ASCII STL".to_string() })];
    if !stl.name.is_empty() && !stl.name.chars().all(|c| c == ' ' || c == '\0') {
        facts.push((if stl.binary { "Header" } else { "Solid" }.into(), stl.name.chars().take(80).collect()));
    }
    if stl.colors.is_some() {
        facts.push(("Colours".into(), "per facet".into()));
    }
    Ok(Model { glb: g.finish(), facts, unit_meters: Some(0.001) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_and_binary() {
        let a = b"solid cube\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 1 1 0\nendloop\nendfacet\nendsolid cube\n";
        assert!(sniff(a, a.len() as u64));
        let s = parse(a, &CancelToken::new()).unwrap();
        assert_eq!(s.triangles.len(), 1);
        assert_eq!(s.name, "cube");
        let mut b = vec![0u8; 80];
        b.extend(2u32.to_le_bytes());
        for t in 0..2 {
            b.extend([0u8; 12]);
            for v in [[0.0f32, 0.0, 0.0], [1.0, 0.0, t as f32], [0.0, 1.0, 0.0]] {
                for c in v {
                    b.extend(c.to_le_bytes());
                }
            }
            b.extend([0u8; 2]);
        }
        assert!(sniff(&b, b.len() as u64));
        let m = convert(&b, &CancelToken::new()).unwrap();
        let js: serde_json::Value = serde_json::from_slice(crate::glb::json_chunk(&m.glb).unwrap()).unwrap();
        // Two shared corners: 4 distinct vertices.
        assert_eq!(js["accessors"][0]["count"], json!(4));
    }
}

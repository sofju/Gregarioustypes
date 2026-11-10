//! Alembic (.abc, Ogawa) through the Alembic library: every visible polygon
//! mesh and subdivision surface at the first sample, in world space, as GLB.
//! Animated caches show their first frame (the facts say how long they run).

use crate::glb::{Glb, ARRAY_BUFFER};
use crate::Model;
use gt_core::CancelToken;
use serde_json::json;
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

pub const OGAWA_MAGIC: &[u8] = b"Ogawa";
pub const HDF5_MAGIC: &[u8] = b"\x89HDF\r\n\x1a\n";

#[repr(C)]
struct AbcMesh {
    name: *const c_char,
    corners: u32,
    pos: *const f32,
    nrm: *const f32,
    uv: *const f32,
}

#[repr(C)]
struct AbcInfo {
    start: f64,
    end: f64,
    fps: f64,
    meshes: u32,
    subds: u32,
    xforms: u32,
    curves: u32,
    points: u32,
    cameras: u32,
    animated: u32,
    hidden: u32,
    max_samples: u64,
    app: [c_char; 256],
    version: [c_char; 128],
    date: [c_char; 64],
    description: [c_char; 512],
}

type MeshCb = unsafe extern "C" fn(user: *mut c_void, m: *const AbcMesh);

extern "C" {
    fn gt_abc_read(path: *const u16, user: *mut c_void, cb: MeshCb, info: *mut AbcInfo, err: *mut c_char, errlen: c_int) -> c_int;
}

struct Collect<'a> {
    g: Glb,
    nodes: Vec<usize>,
    triangles: usize,
    cancel: &'a CancelToken,
}

unsafe extern "C" fn on_mesh(user: *mut c_void, m: *const AbcMesh) {
    let c = &mut *(user as *mut Collect);
    let m = &*m;
    if c.cancel.is_cancelled() || m.corners < 3 {
        return;
    }
    let n = m.corners as usize;
    let pos = std::slice::from_raw_parts(m.pos, n * 3);
    let nrm = (!m.nrm.is_null()).then(|| std::slice::from_raw_parts(m.nrm, n * 3));
    let uv = (!m.uv.is_null()).then(|| std::slice::from_raw_parts(m.uv, n * 2));
    let name = CStr::from_ptr(m.name).to_string_lossy().to_string();
    // Shared corners: same position, normal and UV.
    let mut seen: HashMap<Vec<u32>, u32> = HashMap::with_capacity(n / 2);
    let mut p_out: Vec<f32> = Vec::new();
    let mut n_out: Vec<f32> = Vec::new();
    let mut t_out: Vec<f32> = Vec::new();
    let mut idx: Vec<u32> = Vec::with_capacity(n);
    for i in 0..n {
        let mut key: Vec<u32> = pos[i * 3..i * 3 + 3].iter().map(|f| f.to_bits()).collect();
        if let Some(v) = nrm {
            key.extend(v[i * 3..i * 3 + 3].iter().map(|f| f.to_bits()));
        }
        if let Some(v) = uv {
            key.extend(v[i * 2..i * 2 + 2].iter().map(|f| f.to_bits()));
        }
        let next = (p_out.len() / 3) as u32;
        let id = *seen.entry(key).or_insert_with(|| {
            p_out.extend_from_slice(&pos[i * 3..i * 3 + 3]);
            if let Some(v) = nrm {
                n_out.extend_from_slice(&v[i * 3..i * 3 + 3]);
            }
            if let Some(v) = uv {
                t_out.extend_from_slice(&v[i * 2..i * 2 + 2]);
            }
            next
        });
        idx.push(id);
    }
    // Subdivision surfaces carry no normals: smooth ones, shared by position.
    if nrm.is_none() {
        let count = p_out.len() / 3;
        let mut by_pos: HashMap<[u32; 3], usize> = HashMap::new();
        let mut group = vec![0usize; count];
        for v in 0..count {
            let k = [p_out[v * 3].to_bits(), p_out[v * 3 + 1].to_bits(), p_out[v * 3 + 2].to_bits()];
            let len = by_pos.len();
            group[v] = *by_pos.entry(k).or_insert(len);
        }
        let mut acc = vec![[0f32; 3]; by_pos.len()];
        for t in idx.chunks_exact(3) {
            let p = |i: u32| [p_out[i as usize * 3], p_out[i as usize * 3 + 1], p_out[i as usize * 3 + 2]];
            let (a, b, cc) = (p(t[0]), p(t[1]), p(t[2]));
            let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let e2 = [cc[0] - a[0], cc[1] - a[1], cc[2] - a[2]];
            let fnrm = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
            for &v in t {
                let g = &mut acc[group[v as usize]];
                g[0] += fnrm[0];
                g[1] += fnrm[1];
                g[2] += fnrm[2];
            }
        }
        for v in 0..count {
            let a = acc[group[v]];
            let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
            n_out.extend(if l > 1e-20 { [a[0] / l, a[1] / l, a[2] / l] } else { [0.0, 1.0, 0.0] });
        }
    }
    let g = &mut c.g;
    let mut attrs = json!({ "POSITION": g.floats(&p_out, 3, true, Some(ARRAY_BUFFER)) });
    attrs["NORMAL"] = json!(g.floats(&n_out, 3, false, Some(ARRAY_BUFFER)));
    if !t_out.is_empty() {
        attrs["TEXCOORD_0"] = json!(g.floats(&t_out, 2, false, Some(ARRAY_BUFFER)));
    }
    let i = g.indices(&idx);
    g.meshes.push(json!({ "name": name, "primitives": [ { "attributes": attrs, "indices": i, "material": 0 } ] }));
    let short = name.rsplit('/').next().unwrap_or(&name).to_string();
    let node = g.node(json!({ "name": short, "mesh": g.meshes.len() - 1 }));
    c.nodes.push(node);
    c.triangles += idx.len() / 3;
}

fn cstr(b: &[c_char]) -> String {
    let bytes: Vec<u8> = b.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    String::from_utf8_lossy(&bytes).trim().to_string()
}

pub fn convert(path: &Path, cancel: &CancelToken) -> Result<Model, String> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut c = Collect { g: Glb::new("Gregarioustypes (Alembic)"), nodes: Vec::new(), triangles: 0, cancel };
    c.g.materials.push(json!({ "name": "Default", "pbrMetallicRoughness": { "baseColorFactor": [0.5, 0.5, 0.52, 1.0], "metallicFactor": 0.0, "roughnessFactor": 0.7 }, "doubleSided": true }));
    let mut info: AbcInfo = unsafe { std::mem::zeroed() };
    let mut err = [0 as c_char; 512];
    let t = std::time::Instant::now();
    let ok = unsafe { gt_abc_read(wide.as_ptr(), &mut c as *mut Collect as *mut c_void, on_mesh, &mut info, err.as_mut_ptr(), err.len() as c_int) };
    if cancel.is_cancelled() {
        return Err("cancelled".into());
    }
    if ok == 0 {
        return Err(cstr(&err));
    }
    let mut facts = vec![("Format".to_string(), "Alembic (Ogawa)".to_string())];
    let app = cstr(&info.app);
    if !app.is_empty() {
        facts.push(("Created with".into(), app));
    }
    let date = cstr(&info.date);
    if !date.is_empty() {
        facts.push(("Written".into(), date));
    }
    let desc = cstr(&info.description);
    if !desc.is_empty() {
        facts.push(("Description".into(), desc.chars().take(200).collect()));
    }
    let ver = cstr(&info.version);
    if let Some(v) = ver.split_whitespace().find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit())) {
        facts.push(("Alembic version".into(), v.trim_end_matches(':').to_string()));
    }
    let mut objs = Vec::new();
    for (n, one, many) in [
        (info.meshes, "mesh", "meshes"),
        (info.subds, "subdivision surface", "subdivision surfaces"),
        (info.curves, "curve", "curves"),
        (info.points, "point cloud", "point clouds"),
        (info.cameras, "camera", "cameras"),
        (info.xforms, "transform", "transforms"),
    ] {
        if n > 0 {
            objs.push(format!("{n} {}", if n == 1 { one } else { many }));
        }
    }
    if !objs.is_empty() {
        facts.push(("Objects".into(), objs.join(", ")));
    }
    if info.hidden > 0 {
        facts.push(("Hidden objects".into(), info.hidden.to_string()));
    }
    if info.animated > 0 && info.end > info.start && info.start < 1e300 {
        let dur = info.end - info.start;
        let frames = if info.fps > 0.0 { format!(", {} frames at {:.3} fps", (dur * info.fps).round() as u64 + 1, info.fps).replace(".000 fps", " fps") } else { String::new() };
        facts.push(("Animation".into(), format!("{}{frames} · first frame shown", gt_core::fmt::duration(dur))));
    }
    if c.triangles == 0 {
        return Err(if info.curves + info.points > 0 { "this archive holds only curves or points, which are not shown".into() } else { "there are no polygon meshes in this archive".into() });
    }
    let triangles = c.triangles;
    let mut g = c.g;
    g.scene_nodes = c.nodes;
    let glb = g.finish();
    tracing::info!("{}: Alembic → GLB {} in {:.0} ms ({triangles} triangles)", path.display(), gt_core::fmt::size(glb.len() as u64), t.elapsed().as_secs_f64() * 1000.0);
    Ok(Model { glb, facts, unit_meters: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Uses the Blender-made sample when it is there (samples/ is not in git).
    #[test]
    fn converts_blender_sample() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/3d/blender/scene-animated.abc");
        if !p.is_file() {
            return;
        }
        let m = convert(&p, &CancelToken::new()).unwrap();
        let js: serde_json::Value = serde_json::from_slice(crate::glb::json_chunk(&m.glb).unwrap()).unwrap();
        assert_eq!(js["meshes"].as_array().unwrap().len(), 3);
        // Blender's 0.8 m cube at (2, 0.5, 0.4), Z up, is at (2, 0.4, -0.5) Y up.
        let cube = js["meshes"].as_array().unwrap().iter().position(|m| m["name"] == "/Cube/Cube").unwrap();
        let acc = &js["accessors"][js["meshes"][cube]["primitives"][0]["attributes"]["POSITION"].as_u64().unwrap() as usize];
        let min: Vec<f64> = acc["min"].as_array().unwrap().iter().map(|v| (v.as_f64().unwrap() * 100.0).round() / 100.0).collect();
        assert_eq!(min, vec![1.6, 0.0, -0.9]);
        assert!(m.facts.iter().any(|(k, v)| k == "Animation" && v.contains("48 frames")));
    }
}

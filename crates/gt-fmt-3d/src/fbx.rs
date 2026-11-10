//! FBX and Wavefront OBJ (+MTL) through ufbx, written out as GLB: node
//! hierarchy, meshes split per material, PBR materials with their textures,
//! skinning, blend shapes and every animation take (baked to linear keys).
//! ufbx converts the scene to glTF conventions (right-handed, Y up, metres).

use crate::glb::{Glb, ARRAY_BUFFER};
use crate::Model;
use gt_core::CancelToken;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::Path;

const MAX_MORPHS: usize = 256;
const REPEAT: u32 = 10497;
const CLAMP: u32 = 33071;

pub const FBX_MAGIC: &[u8] = b"Kaydara FBX Binary";

/// Binary FBX, or ASCII FBX (comment header or the FBXHeaderExtension node).
pub fn is_fbx(head: &[u8]) -> bool {
    if head.starts_with(FBX_MAGIC) {
        return true;
    }
    let t = String::from_utf8_lossy(&head[..head.len().min(4096)]);
    t.trim_start().starts_with("; FBX") || t.contains("FBXHeaderExtension:")
}

/// Wavefront OBJ text (not a COFF object file).
pub fn is_obj(head: &[u8]) -> bool {
    let h = &head[..head.len().min(8192)];
    if h.iter().filter(|b| **b == 0).count() > 0 {
        return false;
    }
    let t = String::from_utf8_lossy(h);
    t.lines().map(|l| l.trim_start()).any(|l| {
        l.starts_with("v ") || l.starts_with("vn ") || l.starts_with("vt ") || l.starts_with("f ") || l.starts_with("mtllib ") || l.starts_with("o ") || l.starts_with("g ") || l.starts_with("usemtl ")
    })
}

fn describe(e: &ufbx::Error) -> String {
    let info = e.info();
    let d: &str = &e.description;
    if info.is_empty() {
        d.to_string()
    } else {
        format!("{d}: {info}")
    }
}

fn v3(v: ufbx::Vec3) -> [f32; 3] {
    [v.x as f32, v.y as f32, v.z as f32]
}

fn close(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 1e-5 * (1.0 + x.abs().max(y.abs())))
}

fn flat<const N: usize>(v: &[[f32; N]]) -> &[f32] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const f32, v.len() * N) }
}

fn unit_name(m: f64) -> String {
    let named = [(0.001, "millimetres"), (0.01, "centimetres"), (0.1, "decimetres"), (1.0, "metres"), (1000.0, "kilometres"), (0.0254, "inches"), (0.3048, "feet"), (0.9144, "yards")];
    for (v, n) in named {
        if (m - v).abs() < v * 1e-4 {
            return n.to_string();
        }
    }
    format!("{m} m")
}

fn axis_name(a: ufbx::CoordinateAxis) -> &'static str {
    use ufbx::CoordinateAxis::*;
    match a {
        PositiveX | NegativeX => "X",
        PositiveY | NegativeY => "Y",
        PositiveZ | NegativeZ => "Z",
        _ => "?",
    }
}

struct Ctx<'a> {
    scene: &'a ufbx::Scene,
    path: &'a Path,
    g: Glb,
    materials: HashMap<u32, usize>,
    default_material: Option<usize>,
    textures: HashMap<u32, Option<(usize, bool)>>,
    missing: Vec<String>,
    is_obj: bool,
}

struct MeshOut {
    mesh: usize,
    skin: Option<Value>,
    skinned: bool,
    /// A skinned mesh is shown once (its instances would overlap).
    placed: bool,
    /// Blend channels by target index (element ids, default weights).
    channels: Vec<(u32, f32)>,
}

impl Ctx<'_> {
    /// glTF texture for a ufbx texture: (index, has alpha).
    fn texture(&mut self, t: &ufbx::Texture) -> Option<(usize, bool)> {
        if let Some(r) = self.textures.get(&t.element.typed_id) {
            return *r;
        }
        // Layered/procedural textures: their first file texture.
        let file = if t.type_ == ufbx::TextureType::File { t } else { t.file_textures.iter().next().map(|r| &**r).unwrap_or(t) };
        let name: &str = if !file.relative_filename.is_empty() { &file.relative_filename } else { &file.filename };
        let bytes = if !file.content.is_empty() {
            Some(file.content.to_vec())
        } else {
            let abs: &str = &file.absolute_filename;
            let fname: &str = &file.filename;
            crate::tex::find(self.path, &[abs, name, fname])
        };
        let label = if name.is_empty() { file.element.name.to_string() } else { name.to_string() };
        let r = bytes.and_then(|b| crate::tex::web(b, &label)).map(|(b, mime)| {
            let alpha = crate::tex::has_alpha(&b);
            let wrap = |w: ufbx::WrapMode| if w == ufbx::WrapMode::Clamp { CLAMP } else { REPEAT };
            let s = self.g.sampler(wrap(t.wrap_u), wrap(t.wrap_v));
            (self.g.image(&b, mime, s), alpha)
        });
        if r.is_none() && !label.is_empty() {
            let short = label.replace('\\', "/").rsplit('/').next().unwrap_or(&label).to_string();
            if !self.missing.contains(&short) {
                self.missing.push(short);
            }
        }
        self.textures.insert(t.element.typed_id, r);
        r
    }

    /// `{index, extensions}` for a texture slot, with its UV transform.
    fn slot(&mut self, t: &ufbx::Texture) -> Option<(Value, bool)> {
        let (index, alpha) = self.texture(t)?;
        let mut v = json!({ "index": index });
        if t.has_uv_transform {
            // FBX UVs start at the bottom; ours were flipped (v' = 1 - v).
            let m = &t.uv_to_texture;
            let (a, b, c, d, tx, ty) = (m.m00, m.m01, m.m10, m.m11, m.m03, m.m13);
            // F·M·F with F(x, y) = (x, 1 - y).
            let (a2, b2, c2, d2) = (a, -b, -c, d);
            let tx2 = tx + b;
            let ty2 = 1.0 - ty - d;
            let sx = (a2 * a2 + c2 * c2).sqrt();
            let sy = (b2 * b2 + d2 * d2).sqrt();
            let r = (-c2).atan2(a2);
            v["extensions"] = json!({ "KHR_texture_transform": { "offset": [tx2, ty2], "scale": [sx, sy], "rotation": r } });
            self.g.use_ext("KHR_texture_transform");
        }
        Some((v, alpha))
    }

    fn material(&mut self, m: &ufbx::Material) -> usize {
        if let Some(i) = self.materials.get(&m.element.typed_id) {
            return *i;
        }
        let p = &m.pbr;
        let tex = |map: &ufbx::MaterialMap| map.texture.as_ref().filter(|_| map.texture_enabled).map(|t| &**t as *const ufbx::Texture);
        let val = |map: &ufbx::MaterialMap, d: f64| if map.has_value { map.value_vec4.x } else { d };
        let factor = val(&p.base_factor, 1.0);
        let opacity = val(&p.opacity, 1.0).clamp(0.0, 1.0);
        let mut pbr = Map::new();
        let mut gt = Map::new();
        let mut blend = opacity < 0.999;
        let mut mask = false;
        let base = tex(&p.base_color).and_then(|t| self.slot(unsafe { &*t }));
        let color = if base.is_some() { [factor; 3] } else { [p.base_color.value_vec4.x * factor, p.base_color.value_vec4.y * factor, p.base_color.value_vec4.z * factor] };
        pbr.insert("baseColorFactor".into(), json!([color[0], color[1], color[2], opacity]));
        if let Some((v, alpha)) = &base {
            pbr.insert("baseColorTexture".into(), v.clone());
            // A cut-out texture wired to opacity too.
            let same = tex(&p.opacity).zip(tex(&p.base_color)).is_some_and(|(a, b)| unsafe { (*a).element.typed_id == (*b).element.typed_id });
            if same && *alpha {
                mask = true;
            }
        }
        if !mask {
            if let Some((v, _)) = tex(&p.opacity).and_then(|t| self.slot(unsafe { &*t })) {
                gt.insert("alpha".into(), v);
                blend = true;
            }
        }
        let metal = val(&p.metalness, 0.0).clamp(0.0, 1.0);
        let rough = val(&p.roughness, 0.6).clamp(0.0, 1.0);
        let mut mf = metal;
        let mut rf = rough;
        if let Some((v, _)) = tex(&p.roughness).and_then(|t| self.slot(unsafe { &*t })) {
            gt.insert("roughness".into(), v);
            rf = 1.0;
        }
        if let Some((v, _)) = tex(&p.metalness).and_then(|t| self.slot(unsafe { &*t })) {
            gt.insert("metalness".into(), v);
            mf = 1.0;
        }
        pbr.insert("metallicFactor".into(), json!(mf));
        pbr.insert("roughnessFactor".into(), json!(rf));
        let mut out = json!({ "name": m.element.name.to_string(), "pbrMetallicRoughness": Value::Object(pbr), "doubleSided": true });
        if let Some((v, _)) = tex(&p.normal_map).and_then(|t| self.slot(unsafe { &*t })) {
            out["normalTexture"] = v;
        } else if let Some((mut v, _)) = tex(&m.fbx.bump).and_then(|t| self.slot(unsafe { &*t })) {
            v["scale"] = json!(val(&m.fbx.bump_factor, 1.0));
            gt.insert("bump".into(), v);
        }
        if let Some((v, _)) = tex(&p.ambient_occlusion).and_then(|t| self.slot(unsafe { &*t })) {
            out["occlusionTexture"] = v;
        }
        let ef = val(&p.emission_factor, 1.0);
        let ec = &p.emission_color.value_vec4;
        let emission = [ec.x * ef, ec.y * ef, ec.z * ef];
        let emissive_tex = tex(&p.emission_color).and_then(|t| self.slot(unsafe { &*t }));
        if let Some((v, _)) = emissive_tex {
            out["emissiveTexture"] = v;
            let e = ef.min(1.0);
            out["emissiveFactor"] = json!([e, e, e]);
        } else if emission.iter().any(|c| *c > 0.0) {
            let peak = emission.iter().cloned().fold(0.0, f64::max);
            if peak > 1.0 {
                out["emissiveFactor"] = json!(emission.map(|c| c / peak));
                out["extensions"] = json!({ "KHR_materials_emissive_strength": { "emissiveStrength": peak } });
                self.g.use_ext("KHR_materials_emissive_strength");
            } else {
                out["emissiveFactor"] = json!(emission);
            }
        }
        if mask {
            out["alphaMode"] = json!("MASK");
            out["alphaCutoff"] = json!(0.5);
        } else if blend {
            out["alphaMode"] = json!("BLEND");
        }
        if !gt.is_empty() {
            let ext = out.get_mut("extensions").and_then(|e| e.as_object_mut()).map(|e| e.clone()).unwrap_or_default();
            let mut ext = ext;
            ext.insert("GT_maps".into(), Value::Object(gt));
            out["extensions"] = Value::Object(ext);
            self.g.use_ext("GT_maps");
        }
        self.g.materials.push(out);
        let i = self.g.materials.len() - 1;
        self.materials.insert(m.element.typed_id, i);
        i
    }

    fn default_material(&mut self) -> usize {
        if let Some(i) = self.default_material {
            return i;
        }
        self.g.materials.push(json!({ "name": "Default", "pbrMetallicRoughness": { "baseColorFactor": [0.8, 0.8, 0.8, 1.0], "metallicFactor": 0.0, "roughnessFactor": 0.6 }, "doubleSided": true }));
        let i = self.g.materials.len() - 1;
        self.default_material = Some(i);
        i
    }

    fn mesh(&mut self, mesh: &ufbx::Mesh, cancel: &CancelToken) -> Result<Option<MeshOut>, String> {
        if mesh.num_triangles == 0 {
            return Ok(None);
        }
        let skin = mesh.skin_deformers.iter().next().filter(|s| !s.clusters.is_empty() && !s.vertices.is_empty());
        let mut channels: Vec<(u32, f32)> = Vec::new();
        let mut offsets: Vec<Vec<[f32; 3]>> = Vec::new();
        for d in &mesh.blend_deformers {
            for c in &d.channels {
                if channels.len() >= MAX_MORPHS {
                    break;
                }
                let Some(k) = c.keyframes.last() else { continue };
                let shape = &k.shape;
                let mut o = vec![[0f32; 3]; mesh.num_vertices];
                for (i, v) in shape.offset_vertices.iter().enumerate() {
                    if let (Some(dst), Some(off)) = (o.get_mut(*v as usize), shape.position_offsets.get(i)) {
                        *dst = v3(*off);
                    }
                }
                offsets.push(o);
                channels.push((c.element.element_id, c.weight as f32));
            }
        }
        let has_n = mesh.vertex_normal.exists;
        let has_uv = mesh.vertex_uv.exists;
        let use_colors = mesh.vertex_color.exists && (self.is_obj || mesh.materials.is_empty());
        let mut tri = vec![0u32; mesh.max_face_triangles.max(1) * 3];
        let all: Vec<u32> = (0..mesh.faces.len() as u32).collect();
        let parts: Vec<(Option<&ufbx::Material>, &[u32])> = if mesh.material_parts.is_empty() {
            vec![(mesh.materials.iter().next().map(|m| &**m), &all[..])]
        } else {
            mesh.material_parts.iter().map(|p| (mesh.materials.get(p.index as usize).map(|m| &**m), p.face_indices.as_ref())).collect()
        };
        let mut prims = Vec::new();
        for (mat, faces) in parts {
            if cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            let mut corners: Vec<u32> = Vec::new();
            for &fi in faces {
                let Some(face) = mesh.faces.get(fi as usize) else { continue };
                if face.num_indices < 3 {
                    continue;
                }
                let n = mesh.triangulate_face(&mut tri, *face) as usize;
                corners.extend_from_slice(&tri[..n * 3]);
            }
            if corners.is_empty() {
                continue;
            }
            let count = corners.len();
            let mut pos: Vec<[f32; 3]> = corners.iter().map(|&ix| v3(mesh.vertex_position[ix as usize])).collect();
            let mut nrm: Vec<[f32; 3]> = if has_n {
                corners
                    .iter()
                    .map(|&ix| {
                        let n = v3(mesh.vertex_normal[ix as usize]);
                        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                        if l > 1e-12 {
                            [n[0] / l, n[1] / l, n[2] / l]
                        } else {
                            [0.0, 1.0, 0.0]
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let mut uv: Vec<[f32; 2]> = if has_uv {
                corners
                    .iter()
                    .map(|&ix| {
                        let t = mesh.vertex_uv[ix as usize];
                        [t.x as f32, 1.0 - t.y as f32]
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let mut col: Vec<[f32; 4]> = if use_colors {
                corners
                    .iter()
                    .map(|&ix| {
                        let c = mesh.vertex_color[ix as usize];
                        [c.x as f32, c.y as f32, c.z as f32, c.w as f32]
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let (mut joints, mut weights): (Vec<[u16; 4]>, Vec<[f32; 4]>) = match skin {
                Some(s) => corners
                    .iter()
                    .map(|&ix| {
                        let v = mesh.vertex_indices[ix as usize] as usize;
                        let mut j = [0u16; 4];
                        let mut w = [0f32; 4];
                        if let Some(sv) = s.vertices.get(v) {
                            let b = sv.weight_begin as usize;
                            let n = (sv.num_weights as usize).min(4);
                            for k in 0..n {
                                if let Some(sw) = s.weights.get(b + k) {
                                    j[k] = sw.cluster_index as u16;
                                    w[k] = sw.weight as f32;
                                }
                            }
                        }
                        let sum: f32 = w.iter().sum();
                        if sum > 1e-6 {
                            w.iter_mut().for_each(|x| *x /= sum);
                        } else {
                            w = [1.0, 0.0, 0.0, 0.0];
                        }
                        (j, w)
                    })
                    .unzip(),
                None => (Vec::new(), Vec::new()),
            };
            let mut morph: Vec<Vec<[f32; 3]>> = offsets.iter().map(|o| corners.iter().map(|&ix| o[mesh.vertex_indices[ix as usize] as usize]).collect()).collect();
            let mut idx = vec![0u32; count];
            let n = {
                let mut streams: Vec<ufbx::VertexStream> = vec![ufbx::VertexStream::new(&mut pos[..])];
                if has_n {
                    streams.push(ufbx::VertexStream::new(&mut nrm[..]));
                }
                if has_uv {
                    streams.push(ufbx::VertexStream::new(&mut uv[..]));
                }
                if use_colors {
                    streams.push(ufbx::VertexStream::new(&mut col[..]));
                }
                if skin.is_some() {
                    streams.push(ufbx::VertexStream::new(&mut joints[..]));
                    streams.push(ufbx::VertexStream::new(&mut weights[..]));
                }
                for m in morph.iter_mut() {
                    streams.push(ufbx::VertexStream::new(&mut m[..]));
                }
                ufbx::generate_indices(&mut streams, &mut idx, ufbx::AllocatorOpts::default()).map_err(|e| describe(&e))?
            };
            pos.truncate(n);
            nrm.truncate(n);
            uv.truncate(n);
            col.truncate(n);
            joints.truncate(n);
            weights.truncate(n);
            morph.iter_mut().for_each(|m| m.truncate(n));
            let mut attrs = Map::new();
            attrs.insert("POSITION".into(), json!(self.g.floats(flat(&pos), 3, true, Some(ARRAY_BUFFER))));
            if has_n {
                attrs.insert("NORMAL".into(), json!(self.g.floats(flat(&nrm), 3, false, Some(ARRAY_BUFFER))));
            }
            if has_uv {
                attrs.insert("TEXCOORD_0".into(), json!(self.g.floats(flat(&uv), 2, false, Some(ARRAY_BUFFER))));
            }
            if use_colors {
                attrs.insert("COLOR_0".into(), json!(self.g.colors(&col)));
            }
            if skin.is_some() {
                attrs.insert("JOINTS_0".into(), json!(self.g.joints(&joints)));
                attrs.insert("WEIGHTS_0".into(), json!(self.g.floats(flat(&weights), 4, false, Some(ARRAY_BUFFER))));
            }
            let material = match mat {
                Some(m) => self.material(m),
                None => self.default_material(),
            };
            let mut prim = json!({ "attributes": Value::Object(attrs), "indices": self.g.indices(&idx), "material": material });
            if !morph.is_empty() {
                let targets: Vec<Value> = morph.iter().map(|m| json!({ "POSITION": self.g.floats(flat(m), 3, true, Some(ARRAY_BUFFER)) })).collect();
                prim["targets"] = json!(targets);
            }
            prims.push(prim);
        }
        if prims.is_empty() {
            return Ok(None);
        }
        let mut m = json!({ "name": mesh.element.name.to_string(), "primitives": prims });
        if !channels.is_empty() {
            m["weights"] = json!(channels.iter().map(|c| c.1).collect::<Vec<_>>());
            let names: Vec<String> = mesh.blend_deformers.iter().flat_map(|d| d.channels.iter().map(|c| c.element.name.to_string())).take(channels.len()).collect();
            m["extras"] = json!({ "targetNames": names });
        }
        self.g.meshes.push(m);
        let mesh_index = self.g.meshes.len() - 1;
        let skin = skin.map(|s| {
            let mut ibm: Vec<f32> = Vec::with_capacity(s.clusters.len() * 16);
            let mut joints = Vec::new();
            for c in &s.clusters {
                let b = &c.geometry_to_bone;
                ibm.extend([b.m00, b.m10, b.m20, 0.0, b.m01, b.m11, b.m21, 0.0, b.m02, b.m12, b.m22, 0.0, b.m03, b.m13, b.m23, 1.0].map(|x| x as f32));
                joints.push(c.bone_node.as_ref().map(|n| n.element.typed_id as usize).unwrap_or(self.scene.root_node.element.typed_id as usize));
            }
            let acc = self.g.floats(&ibm, 16, false, None);
            json!({ "joints": joints, "inverseBindMatrices": acc })
        });
        let skinned = skin.is_some();
        Ok(Some(MeshOut { mesh: mesh_index, skin, skinned, placed: false, channels }))
    }
}

/// Nodes under LOD groups other than their first level, and hidden nodes.
fn hidden_nodes(scene: &ufbx::Scene) -> Vec<bool> {
    let mut hidden = vec![false; scene.nodes.len()];
    for n in &scene.nodes {
        let id = n.element.typed_id as usize;
        let parent_hidden = n.parent.as_ref().is_some_and(|p| hidden[p.element.typed_id as usize]);
        let lod_extra = n.parent.as_ref().is_some_and(|p| p.attrib_type == ufbx::ElementType::LodGroup && p.children.iter().next().is_some_and(|first| first.element.typed_id != n.element.typed_id));
        hidden[id] = parent_hidden || lod_extra || !n.visible;
    }
    hidden
}

fn sampler(g: &mut Glb, samplers: &mut Vec<Value>, times: &[f32], values: &[f32], comps: usize) -> usize {
    let input = g.floats(times, 1, true, None);
    let output = g.floats(values, comps, false, None);
    samplers.push(json!({ "input": input, "output": output, "interpolation": "LINEAR" }));
    samplers.len() - 1
}

pub fn convert(path: &Path, cancel: &CancelToken) -> Result<Model, String> {
    let p = path.to_str().ok_or("unsupported file name")?;
    let c2 = cancel.clone();
    let progress = move |_: &ufbx::Progress| if c2.is_cancelled() { ufbx::ProgressResult::Cancel } else { ufbx::ProgressResult::Continue };
    let opts = ufbx::LoadOpts {
        target_axes: ufbx::CoordinateAxes::right_handed_y_up(),
        target_unit_meters: 1.0,
        space_conversion: ufbx::SpaceConversion::TransformRoot,
        geometry_transform_handling: ufbx::GeometryTransformHandling::HelperNodes,
        inherit_mode_handling: ufbx::InheritModeHandling::Compensate,
        generate_missing_normals: true,
        load_external_files: true,
        ignore_missing_external_files: true,
        obj_search_mtl_by_filename: true,
        clean_skin_weights: true,
        progress_cb: ufbx::ProgressCb::Ref(&progress),
        progress_interval_hint: 1 << 20,
        ..Default::default()
    };
    let t = std::time::Instant::now();
    let scene = ufbx::load_file(p, opts).map_err(|e| if cancel.is_cancelled() { "cancelled".to_string() } else { describe(&e) })?;
    let parsed = t.elapsed();
    let is_obj = scene.metadata.file_format == ufbx::FileFormat::Obj;
    let mut cx = Ctx {
        scene: &scene,
        path,
        g: Glb::new("Gregarioustypes (ufbx 0.23)"),
        materials: HashMap::new(),
        default_material: None,
        textures: HashMap::new(),
        missing: Vec::new(),
        is_obj,
    };
    // One glTF node per ufbx node, in typed_id order.
    let mut nodes: Vec<Value> = scene
        .nodes
        .iter()
        .map(|n| {
            let lt = &n.local_transform;
            let mut v = json!({ "name": n.element.name.to_string() });
            let tr = v3(lt.translation);
            if tr != [0.0; 3] {
                v["translation"] = json!(tr);
            }
            let q = [lt.rotation.x as f32, lt.rotation.y as f32, lt.rotation.z as f32, lt.rotation.w as f32];
            if q != [0.0, 0.0, 0.0, 1.0] {
                v["rotation"] = json!(q);
            }
            let s = v3(lt.scale);
            if s != [1.0; 3] {
                v["scale"] = json!(s);
            }
            v
        })
        .collect();
    for n in &scene.nodes {
        if let Some(parent) = n.parent.as_ref() {
            let pi = parent.element.typed_id as usize;
            let kids = nodes[pi].as_object_mut().unwrap().entry("children").or_insert_with(|| json!([]));
            kids.as_array_mut().unwrap().push(json!(n.element.typed_id));
        }
    }
    let hidden = hidden_nodes(&scene);
    let mut built: HashMap<u32, Option<MeshOut>> = HashMap::new();
    // Nodes showing each mesh (targets of blend weight animation), by mesh id.
    let mut weight_nodes: HashMap<u32, Vec<usize>> = HashMap::new();
    let mut extra_nodes: Vec<Value> = Vec::new();
    let mut triangles = 0usize;
    for n in &scene.nodes {
        let Some(mesh) = n.mesh.as_ref() else { continue };
        if hidden[n.element.typed_id as usize] {
            continue;
        }
        let mid = mesh.element.typed_id;
        if !built.contains_key(&mid) {
            let out = cx.mesh(mesh, cancel)?;
            if out.is_some() {
                triangles += mesh.num_triangles;
            }
            built.insert(mid, out);
        }
        let Some(out) = built.get_mut(&mid).and_then(|o| o.as_mut()) else { continue };
        if out.skinned {
            if out.placed {
                continue;
            }
            out.placed = true;
            // Skinned meshes ignore their node's transform: a node of their own at the top.
            if let Some(skin) = out.skin.take() {
                cx.g.skins.push(skin);
            }
            let idx = scene.nodes.len() + extra_nodes.len();
            extra_nodes.push(json!({ "name": n.element.name.to_string(), "mesh": out.mesh, "skin": cx.g.skins.len() - 1 }));
            weight_nodes.entry(mid).or_default().push(idx);
        } else {
            nodes[n.element.typed_id as usize]["mesh"] = json!(out.mesh);
            weight_nodes.entry(mid).or_default().push(n.element.typed_id as usize);
        }
    }
    // NURBS surfaces (Maya, Rhino exports): tessellated by ufbx.
    for surface in &scene.nurbs_surfaces {
        let shown: Vec<usize> = surface.element.instances.iter().map(|n| n.element.typed_id as usize).filter(|i| !hidden[*i]).collect();
        if shown.is_empty() {
            continue;
        }
        let Ok(m) = ufbx::tessellate_nurbs_surface(surface, ufbx::TessellateSurfaceOpts::default()) else { continue };
        if let Some(out) = cx.mesh(&m, cancel)? {
            triangles += m.num_triangles;
            for i in shown {
                if nodes[i].get("mesh").is_none() {
                    nodes[i]["mesh"] = json!(out.mesh);
                }
            }
        }
    }
    if triangles == 0 {
        return Err("there are no polygon meshes in this file (only curves, cameras, lights or empty nodes)".into());
    }
    let rest: Vec<([f32; 3], [f32; 4], [f32; 3])> = scene
        .nodes
        .iter()
        .map(|n| {
            let lt = &n.local_transform;
            (v3(lt.translation), [lt.rotation.x as f32, lt.rotation.y as f32, lt.rotation.z as f32, lt.rotation.w as f32], v3(lt.scale))
        })
        .collect();
    // Animation takes.
    let mut channel_of: HashMap<u32, (u32, usize)> = HashMap::new();
    for (mid, out) in &built {
        if let Some(o) = out {
            for (k, (eid, _)) in o.channels.iter().enumerate() {
                channel_of.insert(*eid, (*mid, k));
            }
        }
    }
    let mut takes = Vec::new();
    for (si, stack) in scene.anim_stacks.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        let opts = ufbx::BakeOpts { trim_start_time: true, key_reduction_enabled: true, ..Default::default() };
        let Ok(baked) = ufbx::bake_anim(&scene, &stack.anim, opts) else { continue };
        let mut samplers = Vec::new();
        let mut channels = Vec::new();
        for bn in &baked.nodes {
            let ni = bn.typed_id as usize;
            let Some(r) = rest.get(ni) else { continue };
            if !bn.translation_keys.is_empty() && !(bn.constant_translation && close(v3(bn.translation_keys[0].value), r.0)) {
                let times: Vec<f32> = bn.translation_keys.iter().map(|k| k.time as f32).collect();
                let vals: Vec<f32> = bn.translation_keys.iter().flat_map(|k| v3(k.value)).collect();
                let s = sampler(&mut cx.g, &mut samplers, &times, &vals, 3);
                channels.push(json!({ "sampler": s, "target": { "node": ni, "path": "translation" } }));
            }
            if !bn.rotation_keys.is_empty() {
                let mut q: Vec<[f32; 4]> = bn.rotation_keys.iter().map(|k| [k.value.x as f32, k.value.y as f32, k.value.z as f32, k.value.w as f32]).collect();
                for i in 1..q.len() {
                    let d: f32 = (0..4).map(|c| q[i][c] * q[i - 1][c]).sum();
                    if d < 0.0 {
                        q[i] = q[i].map(|x| -x);
                    }
                }
                let same = bn.constant_rotation && (0..4).map(|c| q[0][c] * r.1[c]).sum::<f32>().abs() > 1.0 - 1e-6;
                if !same {
                    let times: Vec<f32> = bn.rotation_keys.iter().map(|k| k.time as f32).collect();
                    let s = sampler(&mut cx.g, &mut samplers, &times, flat(&q), 4);
                    channels.push(json!({ "sampler": s, "target": { "node": ni, "path": "rotation" } }));
                }
            }
            if !bn.scale_keys.is_empty() && !(bn.constant_scale && close(v3(bn.scale_keys[0].value), r.2)) {
                let times: Vec<f32> = bn.scale_keys.iter().map(|k| k.time as f32).collect();
                let vals: Vec<f32> = bn.scale_keys.iter().flat_map(|k| v3(k.value)).collect();
                let s = sampler(&mut cx.g, &mut samplers, &times, &vals, 3);
                channels.push(json!({ "sampler": s, "target": { "node": ni, "path": "scale" } }));
            }
        }
        // Blend shape weights: all targets of a mesh share one sampler.
        let mut curves: HashMap<u32, Vec<Vec<(f32, f32)>>> = HashMap::new();
        for be in &baked.elements {
            let Some(&(mid, k)) = channel_of.get(&be.element_id) else { continue };
            for prop in &be.props {
                let name: &str = &prop.name;
                if name == "DeformPercent" && !prop.keys.is_empty() {
                    let n = built[&mid].as_ref().map(|o| o.channels.len()).unwrap_or(0);
                    let e = curves.entry(mid).or_insert_with(|| vec![Vec::new(); n]);
                    e[k] = prop.keys.iter().map(|key| (key.time as f32, (key.value.x / 100.0) as f32)).collect();
                }
            }
        }
        for (mid, cs) in curves {
            let Some(out) = built[&mid].as_ref() else { continue };
            let mut times: Vec<f32> = cs.iter().flat_map(|c| c.iter().map(|k| k.0)).collect();
            times.sort_by(|a, b| a.total_cmp(b));
            times.dedup_by(|a, b| (*a - *b).abs() < 1e-5);
            if times.is_empty() {
                continue;
            }
            let eval = |c: &[(f32, f32)], t: f32, d: f32| -> f32 {
                if c.is_empty() {
                    return d;
                }
                let i = c.partition_point(|k| k.0 <= t);
                if i == 0 {
                    return c[0].1;
                }
                if i >= c.len() {
                    return c[c.len() - 1].1;
                }
                let (a, b) = (c[i - 1], c[i]);
                let f = if b.0 > a.0 { (t - a.0) / (b.0 - a.0) } else { 0.0 };
                a.1 + (b.1 - a.1) * f
            };
            let vals: Vec<f32> = times.iter().flat_map(|t| cs.iter().zip(&out.channels).map(|(c, ch)| eval(c, *t, ch.1)).collect::<Vec<_>>()).collect();
            let s = sampler(&mut cx.g, &mut samplers, &times, &vals, 1);
            for node in weight_nodes.get(&mid).into_iter().flatten() {
                channels.push(json!({ "sampler": s, "target": { "node": node, "path": "weights" } }));
            }
        }
        if !channels.is_empty() {
            let name: &str = &stack.element.name;
            let name = if name.is_empty() { format!("Take {}", si + 1) } else { name.trim_start_matches("AnimStack::").to_string() };
            takes.push(name.clone());
            cx.g.animations.push(json!({ "name": name, "samplers": samplers, "channels": channels }));
        }
    }
    let missing = std::mem::take(&mut cx.missing);
    let mut g = cx.g;
    nodes.extend(extra_nodes);
    g.nodes = nodes;
    g.scene_nodes = std::iter::once(scene.root_node.element.typed_id as usize).chain((scene.nodes.len()..g.nodes.len()).collect::<Vec<_>>()).collect();
    // Facts.
    let md = &scene.metadata;
    let mut facts = Vec::new();
    let format = match md.file_format {
        ufbx::FileFormat::Obj => "Wavefront OBJ".to_string(),
        _ => format!("FBX {}.{} {}", md.version / 1000, md.version % 1000 / 100, if md.ascii { "ASCII" } else { "binary" }),
    };
    facts.push(("Format".to_string(), format));
    let app = |a: &ufbx::Application| {
        let n: &str = &a.name;
        let v: &str = &a.version;
        if n.is_empty() {
            None
        } else if v.is_empty() || n.contains(v) {
            Some(n.to_string())
        } else {
            Some(format!("{n} {v}"))
        }
    };
    let creator: &str = &md.creator;
    match (app(&md.original_application), app(&md.latest_application)) {
        (Some(o), Some(l)) if o != l => {
            facts.push(("Created with".into(), o));
            facts.push(("Saved with".into(), l));
        }
        (Some(o), _) => facts.push(("Created with".into(), o)),
        _ if !creator.is_empty() => facts.push(("Created with".into(), creator.to_string())),
        _ => {}
    }
    if !is_obj {
        facts.push(("Units".into(), unit_name(scene.settings.original_unit_meters)));
        let up = axis_name(scene.settings.original_axis_up);
        if up != "?" {
            facts.push(("Up axis".into(), up.into()));
        }
        if scene.settings.frames_per_second > 0.0 && !takes.is_empty() {
            facts.push(("Frame rate".into(), format!("{:.3} fps", scene.settings.frames_per_second).replace(".000", "")));
        }
    }
    if !g.skins.is_empty() {
        let bones = scene.nodes.iter().filter(|n| n.bone.is_some()).count();
        facts.push(("Skeleton".into(), format!("{bones} bones")));
    }
    let morphs: usize = built.values().flatten().map(|o| o.channels.len()).sum();
    if morphs > 0 {
        facts.push(("Blend shapes".into(), morphs.to_string()));
    }
    if !missing.is_empty() {
        let more = if missing.len() > 4 { format!(" and {} more", missing.len() - 4) } else { String::new() };
        facts.push(("Missing textures".into(), format!("{}{more}", missing[..missing.len().min(4)].join(", "))));
    }
    let glb = g.finish();
    tracing::info!("{}: parsed in {:.0} ms, GLB {} in {:.0} ms ({triangles} triangles)", path.display(), parsed.as_secs_f64() * 1000.0, gt_core::fmt::size(glb.len() as u64), t.elapsed().as_secs_f64() * 1000.0);
    Ok(Model { glb, facts, unit_meters: if is_obj { None } else { Some(1.0) } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffing() {
        assert!(is_fbx(b"Kaydara FBX Binary  \0\x1a\0"));
        assert!(is_fbx(b"; FBX 7.4.0 project file\n"));
        assert!(is_obj(b"# Blender\nmtllib a.mtl\no Cube\nv 1 1 1\n"));
        assert!(!is_obj(b"\x64\x86\x05\x00\x00\x00binary coff"));
    }

    #[test]
    fn converts_obj() {
        let dir = std::env::temp_dir().join(format!("gt3d-obj-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("quad.obj");
        std::fs::write(&p, "mtllib quad.mtl\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nusemtl red\nf 1/1 2/2 3/3 4/4\n").unwrap();
        std::fs::write(dir.join("quad.mtl"), "newmtl red\nKd 1 0 0\n").unwrap();
        let m = convert(&p, &CancelToken::new()).unwrap();
        let js: Value = serde_json::from_slice(crate::glb::json_chunk(&m.glb).unwrap()).unwrap();
        assert_eq!(js["meshes"][0]["primitives"][0]["attributes"]["POSITION"], json!(0));
        assert_eq!(js["accessors"][0]["count"], json!(4));
        let mat = &js["materials"][0];
        assert_eq!(mat["name"], json!("red"));
        assert_eq!(mat["pbrMetallicRoughness"]["baseColorFactor"][0].as_f64().unwrap().round(), 1.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

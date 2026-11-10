//! Minimal glTF 2.0 binary (GLB) writer: everything the converters produce
//! (FBX, OBJ, STL, Alembic) becomes one GLB in memory for the 3D page.

use serde_json::{json, Map, Value};

pub const ARRAY_BUFFER: u32 = 34962;
pub const ELEMENT_ARRAY_BUFFER: u32 = 34963;

const FLOAT: u32 = 5126;
const UNSIGNED_BYTE: u32 = 5121;
const UNSIGNED_SHORT: u32 = 5123;
const UNSIGNED_INT: u32 = 5125;

#[derive(Default)]
pub struct Glb {
    bin: Vec<u8>,
    pub buffer_views: Vec<Value>,
    pub accessors: Vec<Value>,
    pub meshes: Vec<Value>,
    pub materials: Vec<Value>,
    pub textures: Vec<Value>,
    pub images: Vec<Value>,
    pub samplers: Vec<Value>,
    pub nodes: Vec<Value>,
    pub skins: Vec<Value>,
    pub animations: Vec<Value>,
    pub scene_nodes: Vec<usize>,
    pub extensions_used: Vec<&'static str>,
    pub generator: String,
    pub extras: Map<String, Value>,
}

impl Glb {
    pub fn new(generator: &str) -> Self {
        Glb { generator: generator.to_string(), ..Default::default() }
    }

    fn view(&mut self, bytes: &[u8], target: Option<u32>) -> usize {
        while self.bin.len() % 4 != 0 {
            self.bin.push(0);
        }
        let offset = self.bin.len();
        self.bin.extend_from_slice(bytes);
        let mut v = json!({ "buffer": 0, "byteOffset": offset, "byteLength": bytes.len() });
        if let Some(t) = target {
            v["target"] = json!(t);
        }
        self.buffer_views.push(v);
        self.buffer_views.len() - 1
    }

    fn accessor(&mut self, view: usize, ctype: u32, count: usize, kind: &str, extra: Option<(Value, Value)>, normalized: bool) -> usize {
        let mut a = json!({ "bufferView": view, "componentType": ctype, "count": count, "type": kind });
        if let Some((min, max)) = extra {
            a["min"] = min;
            a["max"] = max;
        }
        if normalized {
            a["normalized"] = json!(true);
        }
        self.accessors.push(a);
        self.accessors.len() - 1
    }

    /// Float attribute with `comps` components per element (VEC2/VEC3/VEC4/SCALAR/MAT4).
    pub fn floats(&mut self, data: &[f32], comps: usize, bounds: bool, target: Option<u32>) -> usize {
        let kind = match comps {
            1 => "SCALAR",
            2 => "VEC2",
            3 => "VEC3",
            4 => "VEC4",
            16 => "MAT4",
            _ => unreachable!(),
        };
        let count = data.len() / comps;
        let extra = bounds.then(|| {
            let mut min = vec![f32::INFINITY; comps];
            let mut max = vec![f32::NEG_INFINITY; comps];
            for e in data.chunks_exact(comps) {
                for (i, v) in e.iter().enumerate() {
                    min[i] = min[i].min(*v);
                    max[i] = max[i].max(*v);
                }
            }
            if count == 0 {
                min.fill(0.0);
                max.fill(0.0);
            }
            (json!(min), json!(max))
        });
        let v = self.view(bytemuck_f32(data), target);
        self.accessor(v, FLOAT, count, kind, extra, false)
    }

    /// Triangle indices, as u16 when they fit.
    pub fn indices(&mut self, idx: &[u32]) -> usize {
        let max = idx.iter().copied().max().unwrap_or(0);
        if max < 65535 {
            let v: Vec<u8> = idx.iter().flat_map(|i| (*i as u16).to_le_bytes()).collect();
            let view = self.view(&v, Some(ELEMENT_ARRAY_BUFFER));
            self.accessor(view, UNSIGNED_SHORT, idx.len(), "SCALAR", None, false)
        } else {
            let v: Vec<u8> = idx.iter().flat_map(|i| i.to_le_bytes()).collect();
            let view = self.view(&v, Some(ELEMENT_ARRAY_BUFFER));
            self.accessor(view, UNSIGNED_INT, idx.len(), "SCALAR", None, false)
        }
    }

    /// Skin joint indices (VEC4 of u8 or u16).
    pub fn joints(&mut self, j: &[[u16; 4]]) -> usize {
        let max = j.iter().flat_map(|a| a.iter()).copied().max().unwrap_or(0);
        if max < 256 {
            let v: Vec<u8> = j.iter().flat_map(|a| a.map(|x| x as u8)).collect();
            let view = self.view(&v, Some(ARRAY_BUFFER));
            self.accessor(view, UNSIGNED_BYTE, j.len(), "VEC4", None, false)
        } else {
            let v: Vec<u8> = j.iter().flat_map(|a| a.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>()).collect();
            let view = self.view(&v, Some(ARRAY_BUFFER));
            self.accessor(view, UNSIGNED_SHORT, j.len(), "VEC4", None, false)
        }
    }

    /// Vertex colours as normalised u8 RGBA.
    pub fn colors(&mut self, c: &[[f32; 4]]) -> usize {
        let v: Vec<u8> = c.iter().flat_map(|a| a.map(|x| (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)).collect();
        let view = self.view(&v, Some(ARRAY_BUFFER));
        self.accessor(view, UNSIGNED_BYTE, c.len(), "VEC4", None, true)
    }

    /// An embedded image; returns the texture index (one texture per image and sampler).
    pub fn image(&mut self, bytes: &[u8], mime: &str, sampler: usize) -> usize {
        let view = self.view(bytes, None);
        self.images.push(json!({ "bufferView": view, "mimeType": mime }));
        self.textures.push(json!({ "source": self.images.len() - 1, "sampler": sampler }));
        self.textures.len() - 1
    }

    pub fn sampler(&mut self, wrap_s: u32, wrap_t: u32) -> usize {
        let s = json!({ "magFilter": 9729, "minFilter": 9987, "wrapS": wrap_s, "wrapT": wrap_t });
        if let Some(i) = self.samplers.iter().position(|x| *x == s) {
            return i;
        }
        self.samplers.push(s);
        self.samplers.len() - 1
    }

    pub fn use_ext(&mut self, e: &'static str) {
        if !self.extensions_used.contains(&e) {
            self.extensions_used.push(e);
        }
    }

    pub fn node(&mut self, n: Value) -> usize {
        self.nodes.push(n);
        self.nodes.len() - 1
    }

    pub fn bin_len(&self) -> usize {
        self.bin.len()
    }

    pub fn finish(self) -> Vec<u8> {
        let mut root = json!({
            "asset": { "version": "2.0", "generator": self.generator },
            "scene": 0,
            "scenes": [ { "nodes": self.scene_nodes } ],
        });
        let o = root.as_object_mut().unwrap();
        if !self.bin.is_empty() {
            o.insert("buffers".into(), json!([ { "byteLength": self.bin.len() } ]));
        }
        for (k, v) in [
            ("bufferViews", self.buffer_views),
            ("accessors", self.accessors),
            ("meshes", self.meshes),
            ("materials", self.materials),
            ("textures", self.textures),
            ("images", self.images),
            ("samplers", self.samplers),
            ("nodes", self.nodes),
            ("skins", self.skins),
            ("animations", self.animations),
        ] {
            if !v.is_empty() {
                o.insert(k.into(), Value::Array(v));
            }
        }
        if !self.extensions_used.is_empty() {
            o.insert("extensionsUsed".into(), json!(self.extensions_used));
        }
        if !self.extras.is_empty() {
            o.insert("extras".into(), Value::Object(self.extras));
        }
        let mut js = serde_json::to_vec(&root).unwrap_or_default();
        while js.len() % 4 != 0 {
            js.push(b' ');
        }
        let mut bin = self.bin;
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        let total = 12 + 8 + js.len() + if bin.is_empty() { 0 } else { 8 + bin.len() };
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(js.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&js);
        if !bin.is_empty() {
            out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
            out.extend_from_slice(b"BIN\0");
            out.extend_from_slice(&bin);
        }
        out
    }
}

fn bytemuck_f32(d: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(d.as_ptr() as *const u8, std::mem::size_of_val(d)) }
}

/// Mime type of an image the browser can decode itself, from its magic bytes.
pub fn web_image_mime(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG") {
        Some("image/png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else if b.starts_with(b"GIF8") {
        Some("image/gif")
    } else if b.starts_with(b"BM") {
        Some("image/bmp")
    } else if b.len() > 12 && &b[4..8] == b"ftyp" && (&b[8..12] == b"avif" || &b[8..12] == b"avis") {
        Some("image/avif")
    } else {
        None
    }
}

/// Facts about a GLB/glTF document's JSON (for the info panel).
pub fn json_chunk(glb: &[u8]) -> Option<&[u8]> {
    if glb.len() < 20 || &glb[0..4] != b"glTF" || &glb[16..20] != b"JSON" {
        return None;
    }
    let len = u32::from_le_bytes(glb[12..16].try_into().ok()?) as usize;
    glb.get(20..20 + len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_valid_container() {
        let mut g = Glb::new("test");
        let p = g.floats(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0], 3, true, Some(ARRAY_BUFFER));
        let i = g.indices(&[0, 1, 2]);
        g.meshes.push(json!({ "primitives": [ { "attributes": { "POSITION": p }, "indices": i } ] }));
        let n = g.node(json!({ "mesh": 0 }));
        g.scene_nodes.push(n);
        let out = g.finish();
        assert_eq!(&out[0..4], b"glTF");
        assert_eq!(u32::from_le_bytes(out[8..12].try_into().unwrap()) as usize, out.len());
        let js: Value = serde_json::from_slice(json_chunk(&out).unwrap()).unwrap();
        assert_eq!(js["accessors"][0]["max"], json!([1.0, 1.0, 0.0]));
        assert_eq!(js["accessors"][1]["componentType"], json!(UNSIGNED_SHORT));
        assert_eq!(out.len() % 4, 0);
    }
}

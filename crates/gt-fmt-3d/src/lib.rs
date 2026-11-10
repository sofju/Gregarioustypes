//! 3D models: everything ends up in one three.js page in the shared WebView2.
//! glTF/GLB, USD (usda/usdc/usdz), COLLADA, PLY, 3MF and 3DS load there
//! directly; FBX and OBJ go through ufbx and STL through a converter here
//! (to GLB in memory); Alembic through the Alembic library, and .blend through
//! the bundled Blender (GLB cached on disk).

pub mod abc;
pub mod blend;
pub mod fbx;
pub mod formats;
pub mod glb;
pub mod stl;
pub mod tex;
pub mod view;

use gt_core::CancelToken;
use gt_ui::widgets::card::{CardData, InfoCard};
use gt_ui::{Loaded, Probe, Provider, Registry, View, ViewEnv};
use std::sync::Arc;
use view::{Content, ModelLoaded};

/// A converted model.
pub struct Model {
    pub glb: Vec<u8>,
    pub facts: Vec<(String, String)>,
    /// Size of one scene unit in metres, when known (for the size shown).
    pub unit_meters: Option<f64>,
}

struct CardLoaded(CardData);

impl Loaded for CardLoaded {
    fn into_view(self: Box<Self>, env: &ViewEnv) -> Box<dyn View> {
        Box::new(InfoCard::new(&env.gfx.dc, self.0))
    }
}

pub(crate) fn card(p: &Probe, note: String, facts: Vec<(String, String)>) -> Box<dyn Loaded> {
    let icon = gt_shell::info::image(&p.path.display().to_string(), 256, false).map(|b| (b.width, b.height, b.data));
    let mut f = vec![("Size".to_string(), gt_core::fmt::size_long(p.size))];
    f.extend(facts);
    Box::new(CardLoaded(CardData { icon, name: p.name(), kind: gt_shell::info::type_name(&p.path.display().to_string()), facts: f, note: Some(note) }))
}

fn model(p: &Probe, content: Content, facts: Vec<(String, String)>, unit: Option<f64>) -> Box<dyn Loaded> {
    Box::new(ModelLoaded::ready(&p.path, content, facts, unit))
}

fn converted(p: &Probe, r: Result<Model, String>) -> Result<Box<dyn Loaded>, String> {
    match r {
        Ok(m) => Ok(model(p, Content::Glb(Arc::new(m.glb)), m.facts, m.unit_meters)),
        Err(e) if e == "cancelled" => Err(e),
        Err(e) => Ok(card(p, format!("This model could not be read: {e}"), Vec::new())),
    }
}

pub struct GltfProvider;

impl Provider for GltfProvider {
    fn id(&self) -> &'static str {
        "gltf"
    }
    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir {
            return 0;
        }
        let s = if formats::is_glb(&p.head) || (p.ext == "gltf" && formats::is_gltf_json(&p.head)) {
            95
        } else if p.ext_is(&["glb", "gltf"]) {
            40
        } else {
            0
        };
        if s > 0 {
            crate::view::preload();
        }
        s
    }
    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let facts = formats::gltf_facts(&p.path, &p.head, p.size);
        Ok(model(p, Content::File { path: p.path.clone(), kind: "gltf" }, facts, Some(1.0)))
    }
}

pub struct UsdProvider;

impl Provider for UsdProvider {
    fn id(&self) -> &'static str {
        "usd"
    }
    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir || !p.ext_is(formats::USD_EXTS) || !formats::is_usd(&p.head, &p.ext) {
            return 0;
        }
        crate::view::preload();
        95
    }
    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let facts = formats::usd_facts(&p.path, &p.head);
        Ok(model(p, Content::File { path: p.path.clone(), kind: "usd" }, facts, Some(1.0)))
    }
}

pub struct ExtraProvider;

impl Provider for ExtraProvider {
    fn id(&self) -> &'static str {
        "model-extra"
    }
    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir || formats::extra_kind(&p.head, &p.ext).is_none() {
            return 0;
        }
        crate::view::preload();
        90
    }
    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let kind = formats::extra_kind(&p.head, &p.ext).ok_or("not a 3D model")?;
        let facts = formats::extra_facts(kind, &p.head);
        Ok(model(p, Content::File { path: p.path.clone(), kind }, facts, None))
    }
}

pub struct FbxProvider;

impl Provider for FbxProvider {
    fn id(&self) -> &'static str {
        "fbx"
    }
    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir {
            return 0;
        }
        let s = match p.ext.as_str() {
            "fbx" if fbx::is_fbx(&p.head) => 95,
            "obj" if fbx::is_obj(&p.head) => 90,
            _ => 0,
        };
        if s > 0 {
            crate::view::preload();
        }
        s
    }
    fn load(&self, p: &Probe, cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        converted(p, fbx::convert(&p.path, cancel))
    }
}

pub struct StlProvider;

impl Provider for StlProvider {
    fn id(&self) -> &'static str {
        "stl"
    }
    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir || p.ext != "stl" || !stl::sniff(&p.head, p.size) {
            return 0;
        }
        crate::view::preload();
        95
    }
    fn load(&self, p: &Probe, cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let d = std::fs::read(&p.path).map_err(|e| e.to_string())?;
        converted(p, stl::convert(&d, cancel))
    }
}

pub struct AbcProvider;

impl Provider for AbcProvider {
    fn id(&self) -> &'static str {
        "abc"
    }
    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir || p.ext != "abc" || !(p.head.starts_with(abc::OGAWA_MAGIC) || p.head.starts_with(abc::HDF5_MAGIC)) {
            return 0;
        }
        crate::view::preload();
        95
    }
    fn load(&self, p: &Probe, cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        if p.head.starts_with(abc::HDF5_MAGIC) {
            let facts = vec![("Format".to_string(), "Alembic (HDF5)".to_string())];
            return Ok(card(p, "This Alembic file uses the old HDF5 storage (before 2013), which is not supported. Re-export it as Ogawa.".into(), facts));
        }
        converted(p, abc::convert(&p.path, cancel))
    }
}

pub fn register(r: &mut Registry) {
    r.add(GltfProvider);
    r.add(UsdProvider);
    r.add(FbxProvider);
    r.add(StlProvider);
    r.add(AbcProvider);
    r.add(blend::BlendProvider);
    r.add(ExtraProvider);
}

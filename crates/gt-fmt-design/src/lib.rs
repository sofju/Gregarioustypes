//! Design files: InDesign (.indd previews and metadata, .idml lite layout),
//! Xara (.xar/.web/.wix/.art previews) and Affinity (.af* previews).
//! Photoshop PSD/PSB is an image codec (gt-image).

pub mod affinity;
pub mod common;
pub mod dom;
pub mod idml;
pub mod idml_render;
pub mod indd;
pub mod xara;

use gt_core::CancelToken;
use gt_ui::{Loaded, Probe, Provider, Registry};

pub struct IdmlProvider;

impl Provider for IdmlProvider {
    fn id(&self) -> &'static str {
        "idml"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir || !p.head.starts_with(b"PK") {
            return 0;
        }
        let mime = p.head.windows(31).any(|w| w == b"application/vnd.adobe.indesign-");
        match (p.ext == "idml", mime) {
            (_, true) => 95,
            (true, false) => 70,
            _ => 0,
        }
    }

    fn load(&self, p: &Probe, cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let (src, facts) = idml::load(&p.path, cancel)?;
        Ok(Box::new(common::PagesLoaded { src, facts, note: Some("Lite layout: InDesign's own line breaks, tables and effects may differ".into()) }))
    }
}

pub fn register(r: &mut Registry) {
    r.add(indd::InddProvider);
    r.add(IdmlProvider);
    r.add(xara::XaraProvider);
    r.add(affinity::AffinityProvider);
}

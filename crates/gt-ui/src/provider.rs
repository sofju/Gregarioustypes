//! Format providers: probe a file, load it off the UI thread, then build a view.

use crate::view::View;
use gt_core::settings::Settings;
use gt_core::CancelToken;
use std::path::PathBuf;
use std::sync::Arc;

/// What a provider gets to decide whether it handles a file.
pub struct Probe {
    pub path: PathBuf,
    /// Lower-case extension without the dot.
    pub ext: String,
    /// First bytes of the file (64 KB).
    pub head: Vec<u8>,
    pub size: u64,
    pub is_dir: bool,
    pub settings: Arc<Settings>,
    /// Size of the target monitor in physical pixels (for reduced decodes).
    pub screen: (u32, u32),
}

impl Probe {
    pub fn ext_is(&self, list: &[&str]) -> bool {
        list.iter().any(|e| *e == self.ext)
    }
    /// The whole file fits in `head`.
    pub fn complete(&self) -> bool {
        self.head.len() as u64 >= self.size
    }
    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| self.path.display().to_string())
    }
}

/// UI-thread services available while turning loaded data into a view.
pub struct ViewEnv<'a> {
    pub gfx: &'a gt_render::Gfx,
    pub settings: &'a Settings,
    pub scale: f32,
}

/// Background work producing a refined payload for `View::upgrade`.
pub type Upgrade = Box<dyn FnOnce(&CancelToken) -> Option<Box<dyn std::any::Any + Send>> + Send>;

/// Data loaded off-thread; converted to a view on the UI thread.
pub trait Loaded: Send {
    fn into_view(self: Box<Self>, env: &ViewEnv) -> Box<dyn View>;

    /// Optional second stage run after the first result is shown.
    fn take_upgrade(&mut self) -> Option<Upgrade> {
        None
    }
}

pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    /// 0 = cannot handle. Extension matches typically score 50, confirmed magic 90+.
    fn score(&self, p: &Probe) -> u32;
    fn load(&self, p: &Probe, cancel: &CancelToken) -> Result<Box<dyn Loaded>, String>;
}

#[derive(Default)]
pub struct Registry {
    providers: Vec<Arc<dyn Provider>>,
}

impl Registry {
    pub fn add(&mut self, p: impl Provider + 'static) {
        self.providers.push(Arc::new(p));
    }

    pub fn by_id(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.providers.iter().find(|p| p.id() == id).cloned()
    }

    /// Providers able to handle `p`, best first.
    pub fn candidates(&self, p: &Probe) -> Vec<Arc<dyn Provider>> {
        let mut v: Vec<(u32, Arc<dyn Provider>)> = self.providers.iter().map(|pr| (pr.score(p), pr.clone())).filter(|(s, _)| *s > 0).collect();
        v.sort_by(|a, b| b.0.cmp(&a.0));
        v.into_iter().map(|(_, p)| p).collect()
    }
}

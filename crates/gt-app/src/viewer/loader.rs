//! Off-thread loading: probe the file, pick providers best-first, isolate panics.

use gt_core::settings::Settings;
use gt_core::CancelToken;
use gt_ipc::Item;
use gt_ui::widgets::card::CardData;
use gt_ui::{Loaded, Probe, Registry};
use std::path::PathBuf;
use std::sync::Arc;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

pub struct LoadResult {
    pub generation: u64,
    pub path: String,
    pub provider: &'static str,
    pub result: Result<Box<dyn Loaded>, String>,
    pub elapsed_ms: f64,
}

/// Title-bar extras, sent after the preview so they never delay first paint.
pub struct Extras {
    pub generation: u64,
    /// Small shell icon for the title bar.
    pub icon: Option<gt_shell::info::Bgra>,
    /// Friendly name of the default app ("Open with Photos").
    pub open_with: Option<String>,
}

pub enum LoadMsg {
    Done(LoadResult),
    Extras(Extras),
    /// Refined content (full-resolution decode...) for `View::upgrade`.
    Upgrade { generation: u64, payload: Box<dyn std::any::Any + Send> },
    /// A neighbour decoded ahead of time.
    Prefetched(Prefetched),
    /// A report (MediaInfo...) finished.
    Report { id: u64, result: Result<String, String> },
}

pub struct Prefetched {
    pub path: String,
    pub provider: &'static str,
    pub loaded: Box<dyn Loaded>,
    pub upgrade: Option<Box<dyn std::any::Any + Send>>,
    pub elapsed_ms: f64,
}

/// Head bytes read for sniffing.
const HEAD: usize = 64 * 1024;

fn probe(item: &Item, settings: &Arc<Settings>, screen: (u32, u32)) -> Result<Probe, String> {
    let path = PathBuf::from(&item.path);
    let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    let is_dir = meta.is_dir();
    let head = if is_dir { Vec::new() } else { gt_core::sniff::read_head(&path, HEAD).map_err(|e| e.to_string())? };
    Ok(Probe { ext: gt_core::sniff::ext(&path), path, head, size: meta.len(), is_dir, settings: settings.clone(), screen })
}

pub fn load(registry: &Registry, item: &Item, settings: &Arc<Settings>, cancel: &CancelToken, forced: Option<&'static str>, screen: (u32, u32)) -> (Result<Box<dyn Loaded>, String>, &'static str) {
    if !item.filesystem {
        return (Ok(virtual_card(item)), "virtual");
    }
    let p = match probe(item, settings, screen) {
        Ok(p) => p,
        Err(e) => return (Err(e), "none"),
    };
    // The engine chosen for this extension in the settings comes first.
    let chosen = forced.map(String::from).or_else(|| p.settings.engine_for(&p.ext).map(String::from));
    let candidates = match chosen.as_deref().and_then(|id| registry.by_id(id)) {
        Some(f) => {
            let mut v = vec![f];
            v.extend(registry.candidates(&p));
            v
        }
        None => registry.candidates(&p),
    };
    let mut last_err = String::from("no previewer for this file");
    for prov in candidates {
        if cancel.is_cancelled() {
            return (Err("cancelled".into()), "none");
        }
        match prov.load(&p, cancel) {
            Ok(l) => return (Ok(l), prov.id()),
            Err(e) => {
                tracing::warn!("{} failed on {}: {e}", prov.id(), item.path);
                last_err = e;
            }
        }
    }
    (Err(last_err), "none")
}

struct Card(CardData);
impl Loaded for Card {
    fn into_view(self: Box<Self>, env: &gt_ui::ViewEnv) -> Box<dyn gt_ui::View> {
        Box::new(gt_ui::widgets::card::InfoCard::new(&env.gfx.dc, self.0))
    }
}

/// Items without a file-system path (inside zip folders, on phones...).
fn virtual_card(item: &Item) -> Box<dyn Loaded> {
    let name = item.path.rsplit(['\\', '/']).next().unwrap_or(&item.path).to_string();
    let icon = gt_shell::info::image(&item.path, 256, false).map(|b| (b.width, b.height, b.data));
    Box::new(Card(CardData {
        icon,
        name,
        kind: "Shell item".into(),
        facts: vec![("Location".into(), item.path.clone())],
        note: Some("This item has no file-system path, so only its icon can be shown.".into()),
    }))
}

pub fn error_card(path: &str, title: &str, note: &str) -> Box<dyn Loaded> {
    let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
    let icon = gt_shell::info::image(path, 256, false).map(|b| (b.width, b.height, b.data));
    Box::new(Card(CardData { icon, name, kind: title.into(), facts: vec![("Location".into(), path.to_string())], note: Some(note.into()) }))
}

type Job = Box<dyn FnOnce() + Send>;

/// Loader threads started with the viewer, COM already initialised, so a preview
/// request costs a channel send instead of a thread spawn plus COM start-up.
pub struct Pool {
    tx: crossbeam_channel::Sender<Job>,
}

impl Pool {
    pub fn new(threads: usize) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<Job>();
        for i in 0..threads {
            let rx = rx.clone();
            std::thread::Builder::new()
                .name(format!("gt-load-{i}"))
                .spawn(move || {
                    unsafe {
                        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                    }
                    // Warm up the shell's icon machinery once.
                    if i == 0 {
                        let _ = gt_shell::info::image(r"C:\Windows", 32, false);
                    }
                    for job in rx {
                        job();
                    }
                })
                .expect("loader thread");
        }
        Self { tx }
    }

    /// Run any background job on the pool.
    pub fn run(&self, job: impl FnOnce() + Send + 'static) {
        let _ = self.tx.send(Box::new(job));
    }

    /// Title-bar icon and default app for an item shown from the prefetch cache.
    pub fn extras(&self, generation: u64, path: String, filesystem: bool, tx: crossbeam_channel::Sender<LoadMsg>, hwnd: isize, msg: u32) {
        let job = move || {
            let icon = gt_shell::info::image(&path, 32, false);
            let open_with = if filesystem && !std::path::Path::new(&path).is_dir() { gt_shell::info::default_app(&path) } else { None };
            let _ = tx.send(LoadMsg::Extras(Extras { generation, icon, open_with }));
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(0), LPARAM(0));
            }
        };
        let _ = self.tx.send(Box::new(job));
    }

    /// Decode `item` ahead of time (upgrade included) and hand it to the window.
    #[allow(clippy::too_many_arguments)]
    pub fn prefetch(&self, registry: Arc<Registry>, item: Item, settings: Arc<Settings>, tx: crossbeam_channel::Sender<LoadMsg>, hwnd: isize, msg: u32, screen: (u32, u32), cancel: CancelToken) {
        let job = move || {
            if cancel.is_cancelled() {
                return;
            }
            let t0 = std::time::Instant::now();
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load(&registry, &item, &settings, &cancel, None, screen)));
            let Ok((Ok(mut loaded), provider)) = out else { return };
            let upgrade = loaded.take_upgrade().and_then(|up| std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| up(&cancel))).ok().flatten());
            let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
            let _ = tx.send(LoadMsg::Prefetched(Prefetched { path: item.path.clone(), provider, loaded, upgrade, elapsed_ms }));
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(0), LPARAM(0));
            }
        };
        let _ = self.tx.send(Box::new(job));
    }

    /// Load `item`; results go to `tx` and the window is poked with `msg`.
    #[allow(clippy::too_many_arguments)]
    pub fn load(
        &self,
        registry: Arc<Registry>,
        item: Item,
        settings: Arc<Settings>,
        cancel: CancelToken,
        forced: Option<&'static str>,
        generation: u64,
        tx: crossbeam_channel::Sender<LoadMsg>,
        hwnd: isize,
        msg: u32,
        screen: (u32, u32),
    ) {
        let job = move || {
            if cancel.is_cancelled() {
                return;
            }
            let t0 = std::time::Instant::now();
            let path = item.path.clone();
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load(&registry, &item, &settings, &cancel, forced, screen)));
            let (mut result, provider) = match out {
                Ok(r) => r,
                Err(_) => (Err("the previewer panicked on this file".into()), "panic"),
            };
            let upgrade = result.as_mut().ok().and_then(|l| l.take_upgrade());
            let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
            let poke = || unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(0), LPARAM(0));
            };
            let _ = tx.send(LoadMsg::Done(LoadResult { generation, path: path.clone(), provider, result, elapsed_ms }));
            poke();
            if cancel.is_cancelled() {
                return;
            }
            let icon = gt_shell::info::image(&path, 32, false);
            let open_with = if item.filesystem && !std::path::Path::new(&path).is_dir() { gt_shell::info::default_app(&path) } else { None };
            let _ = tx.send(LoadMsg::Extras(Extras { generation, icon, open_with }));
            poke();
            if let Some(up) = upgrade {
                let t1 = std::time::Instant::now();
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| up(&cancel))).ok().flatten();
                if let Some(payload) = r {
                    tracing::info!("upgrade for {path} in {:.1} ms", t1.elapsed().as_secs_f64() * 1000.0);
                    let _ = tx.send(LoadMsg::Upgrade { generation, payload });
                    poke();
                }
            }
        };
        let _ = self.tx.send(Box::new(job));
    }
}

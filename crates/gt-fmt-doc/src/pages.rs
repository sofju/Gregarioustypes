//! Page sources (PDF, SVG...) and the background thread that rasterizes them.
//!
//! Views queue render jobs; the worker takes the most recent job first (the
//! page the user is looking at now) and skips jobs whose generation is stale.
//! Finished bitmaps go to the view's mailbox and the window gets a wake message.

use parking_lot::{Condvar, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Rendered pixels: premultiplied BGRA, `w * 4` bytes per row.
pub struct Raster {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

pub trait PageSource: Send + Sync {
    fn count(&self) -> usize;
    /// Page size in source units (points for PDF, CSS pixels for SVG).
    fn size(&self, page: usize) -> (f32, f32);
    /// DIPs per source unit at 100 % zoom.
    fn dip_per_unit(&self) -> f32 {
        96.0 / 72.0
    }
    /// Render the pixel region (x, y, w, h) of `page` drawn at `scale` pixels per unit.
    fn render(&self, page: usize, scale: f32, x: i32, y: i32, w: u32, h: u32) -> Option<Raster>;
    fn text(&self, page: usize) -> Option<String> {
        let _ = page;
        None
    }
    fn label(&self, page: usize) -> Option<String> {
        let _ = page;
        None
    }
    /// Pages are paper (white sheet with a shadow) rather than artwork on a checkerboard.
    fn paper(&self) -> bool {
        true
    }
}

pub struct Done {
    pub key: u64,
    pub page: usize,
    pub scale: f32,
    pub x: i32,
    pub y: i32,
    pub raster: Option<Raster>,
}

#[derive(Default)]
pub struct Mailbox {
    pub done: Mutex<Vec<Done>>,
    /// Window to wake (HWND as isize) and message.
    pub notify: Mutex<Option<(isize, u32)>>,
}

struct Job {
    src: Arc<dyn PageSource>,
    key: u64,
    page: usize,
    scale: f32,
    rect: (i32, i32, u32, u32),
    gen: Arc<AtomicU64>,
    at: u64,
    mailbox: Arc<Mailbox>,
}

struct Queue {
    jobs: Mutex<Vec<Job>>,
    cv: Condvar,
}

fn queue() -> &'static Queue {
    static Q: OnceLock<&'static Queue> = OnceLock::new();
    Q.get_or_init(|| {
        let q: &'static Queue = Box::leak(Box::new(Queue { jobs: Mutex::new(Vec::new()), cv: Condvar::new() }));
        // Two workers: PDFium serialises itself, but SVG and PDF jobs of
        // different views, or a slow page, should not block everything.
        for n in 0..2 {
            std::thread::Builder::new().name(format!("gt-pages-{n}")).spawn(move || worker(q)).expect("page worker");
        }
        q
    })
}

fn worker(q: &'static Queue) {
    loop {
        let job = {
            let mut jobs = q.jobs.lock();
            loop {
                jobs.retain(|j| j.gen.load(Ordering::Relaxed) == j.at);
                if let Some(j) = jobs.pop() {
                    break j;
                }
                q.cv.wait(&mut jobs);
            }
        };
        let (x, y, w, h) = job.rect;
        let raster = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.src.render(job.page, job.scale, x, y, w, h))).unwrap_or(None);
        if job.gen.load(Ordering::Relaxed) != job.at {
            continue;
        }
        job.mailbox.done.lock().push(Done { key: job.key, page: job.page, scale: job.scale, x, y, raster });
        let notify = *job.mailbox.notify.lock();
        if let Some((hwnd, msg)) = notify {
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(0), LPARAM(0));
            }
        }
    }
}

/// A view's handle on the render queue.
pub struct Renderer {
    src: Arc<dyn PageSource>,
    pub mailbox: Arc<Mailbox>,
    gen: Arc<AtomicU64>,
}

impl Renderer {
    pub fn new(src: Arc<dyn PageSource>) -> Self {
        Renderer { src, mailbox: Arc::new(Mailbox::default()), gen: Arc::new(AtomicU64::new(1)) }
    }

    pub fn source(&self) -> &Arc<dyn PageSource> {
        &self.src
    }

    /// Queue a job; later submissions run first.
    pub fn submit(&self, key: u64, page: usize, scale: f32, rect: (i32, i32, u32, u32)) {
        let q = queue();
        let at = self.gen.load(Ordering::Relaxed);
        q.jobs.lock().push(Job { src: self.src.clone(), key, page, scale, rect, gen: self.gen.clone(), at, mailbox: self.mailbox.clone() });
        q.cv.notify_one();
    }

    /// Drop every queued job of this view (zoom changed, view closed).
    pub fn invalidate(&self) {
        self.gen.fetch_add(1, Ordering::Relaxed);
    }

    pub fn take_done(&self) -> Vec<Done> {
        std::mem::take(&mut *self.mailbox.done.lock())
    }

    pub fn set_notify(&self, n: Option<(isize, u32)>) {
        *self.mailbox.notify.lock() = n;
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.invalidate();
    }
}

/// Render synchronously on the calling thread (first page during load).
pub fn render_now(src: &dyn PageSource, page: usize, scale: f32, w: u32, h: u32) -> Option<Raster> {
    src.render(page, scale, 0, 0, w, h)
}

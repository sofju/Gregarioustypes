//! The playback engine: one warm mpv core shared by every media view.
//!
//! mpv draws into its own child window inside `host`, a disabled child of the
//! preview window placed over the content rect. Being disabled, the host passes
//! mouse input to the preview window, and our topmost UI layer (transport bar,
//! MediaInfo report) composes above it.

use crate::mpv::{self, Event, Mpv, Node};
use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetStockObject, HBRUSH, BLACK_BRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

static WAKE_MSG: AtomicU32 = AtomicU32::new(0);

const OBS_PAUSE: u64 = 1;
const OBS_TIME: u64 = 2;
const OBS_DURATION: u64 = 3;
const OBS_EOF: u64 = 4;
const OBS_VOLUME: u64 = 5;
const OBS_MUTE: u64 = 6;
const OBS_VIDEO_PARAMS: u64 = 7;
const OBS_TRACKS: u64 = 8;
const OBS_CHAPTERS: u64 = 9;
const OBS_BUFFERING: u64 = 10;
const OBS_CACHE: u64 = 11;
const OBS_VIDEO_OUT: u64 = 12;
const OBS_HWDEC: u64 = 13;
const OBS_PLAYLIST_POS: u64 = 14;
const OBS_SEEKING: u64 = 15;
const OBS_AUDIO_PARAMS: u64 = 16;

#[derive(Clone, Debug, Default)]
pub struct Track {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub lang: String,
    pub codec: String,
    pub selected: bool,
    pub default: bool,
    pub external: bool,
    pub albumart: bool,
    pub channels: i64,
}

impl Track {
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if !self.title.is_empty() {
            parts.push(self.title.clone());
        }
        if !self.lang.is_empty() {
            parts.push(self.lang.clone());
        }
        if !self.codec.is_empty() {
            parts.push(self.codec.to_uppercase());
        }
        if parts.is_empty() {
            format!("Track {}", self.id)
        } else {
            parts.join(" · ")
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PlayerState {
    /// The file most recently asked for has started loading in mpv.
    pub started: bool,
    pub loaded: bool,
    /// Playback started: the first video frame is about to be shown.
    pub first_frame: bool,
    pub paused: bool,
    pub time: f64,
    pub duration: f64,
    pub eof: bool,
    pub ended: bool,
    pub volume: f64,
    pub mute: bool,
    pub buffering: bool,
    pub seeking: bool,
    pub cache_time: f64,
    pub error: Option<String>,
    /// Decoded video size (0 for audio).
    pub video_w: i64,
    pub video_h: i64,
    pub video_gamma: String,
    pub video_prim: String,
    pub sig_peak: f64,
    pub out_gamma: String,
    pub out_max_luma: f64,
    pub hwdec: String,
    pub audio_desc: String,
    pub tracks: Vec<Track>,
    pub chapters: Vec<(f64, String)>,
    pub playlist_pos: i64,
}

impl PlayerState {
    pub fn tracks_of(&self, kind: &str) -> Vec<&Track> {
        self.tracks.iter().filter(|t| t.kind == kind && !t.albumart).collect()
    }
}

/// What changed during a `pump`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Changes {
    pub any: bool,
    /// Only the playback position moved.
    pub time_only: bool,
    pub first_frame: bool,
    pub ended: bool,
}

pub struct LoadOpts {
    pub audio_only: bool,
    pub pause: bool,
    pub start: f64,
    /// The source is HDR (PQ/HLG/Dolby Vision): tone-map to the display peak.
    pub hdr_source: bool,
    /// Extra file-local options ("name=value").
    pub extra: Vec<String>,
}

pub struct Engine {
    parent: HWND,
    host: HWND,
    init: Option<JoinHandle<Result<Mpv, String>>>,
    mpv: Option<Arc<Mpv>>,
    failed: Option<String>,
    pub st: PlayerState,
    /// Reply id of the latest `loadfile`, and the playlist entry it created.
    gen: u64,
    want_entry: Option<i64>,
    cur_entry: i64,
    /// Playlists: whatever entry mpv moves to is the current one.
    follow: bool,
    shown: bool,
    rect: (i32, i32, i32, i32),
    peak: Option<f32>,
    volume: f64,
    load_at: Option<Instant>,
    url: String,
}

thread_local! {
    static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) };
}

/// Run `f` with the engine (created on first use, for `parent`).
pub fn with<R>(parent: HWND, f: impl FnOnce(&mut Engine) -> R) -> R {
    ENGINE.with(|e| {
        let mut e = e.borrow_mut();
        if e.is_none() {
            *e = Some(Engine::new(parent));
        }
        f(e.as_mut().unwrap())
    })
}

/// Run `f` only if the engine exists.
pub fn with_existing<R>(f: impl FnOnce(&mut Engine) -> R) -> Option<R> {
    ENGINE.with(|e| e.borrow_mut().as_mut().map(f))
}

/// Close mpv and its window (the previewer has been idle); the next media file
/// starts it again.
pub fn release() {
    let e = ENGINE.with(|e| e.borrow_mut().take());
    if let Some(mut e) = e {
        if let Some(j) = e.init.take() {
            let _ = j.join();
        }
        let mpv = e.mpv.take();
        unsafe {
            let _ = DestroyWindow(e.host);
        }
        drop(mpv);
        tracing::info!("mpv released");
    }
}

/// Create the host window and start mpv in the background, so the first video
/// opens without paying for DLL loading, mpv start-up and the GPU swap chain.
pub fn prewarm(parent: HWND, wake_msg: u32) {
    WAKE_MSG.store(wake_msg, Ordering::Relaxed);
    with(parent, |_| {});
}

unsafe extern "system" fn host_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn create_host(parent: HWND) -> HWND {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("Gregarioustypes.VideoHost");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(host_proc),
            hInstance: hinst.into(),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        CreateWindowExW(
            WS_EX_NOPARENTNOTIFY,
            class,
            w!(""),
            WS_CHILD | WS_DISABLED | WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
            0,
            0,
            64,
            64,
            Some(parent),
            None,
            Some(hinst.into()),
            None,
        )
        .unwrap_or_default()
    }
}

static WAKES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

unsafe extern "C" fn wakeup(ctx: *mut c_void) {
    WAKES.fetch_add(1, Ordering::Relaxed);
    let msg = WAKE_MSG.load(Ordering::Relaxed);
    if msg != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(ctx)), msg, WPARAM(0), LPARAM(0));
        }
    }
}

fn start_mpv(host: isize, parent: isize) -> Result<Mpv, String> {
    let t = Instant::now();
    let m = Mpv::create()?;
    let shader_cache = gt_core::paths::cache_dir().join("mpv-shaders");
    let _ = std::fs::create_dir_all(&shader_cache);
    let opts: &[(&str, String)] = &[
        ("wid", host.to_string()),
        ("vo", "gpu-next".into()),
        ("gpu-api", "d3d11".into()),
        ("gpu-context", "d3d11".into()),
        ("hwdec", "auto-safe".into()),
        ("idle", "yes".into()),
        ("force-window", "yes".into()),
        ("keep-open", "yes".into()),
        ("terminal", "no".into()),
        ("config", "no".into()),
        ("load-scripts", "no".into()),
        ("ytdl", "no".into()),
        ("osc", "no".into()),
        ("load-stats-overlay", "no".into()),
        ("load-console", "no".into()),
        ("load-select", "no".into()),
        ("load-auto-profiles", "no".into()),
        ("load-positioning", "no".into()),
        ("load-commands", "no".into()),
        ("load-context-menu", "no".into()),
        ("input-default-bindings", "no".into()),
        ("input-builtin-bindings", "no".into()),
        ("input-vo-keyboard", "no".into()),
        ("input-cursor", "no".into()),
        ("cursor-autohide", "no".into()),
        ("osd-level", "0".into()),
        ("osd-bar", "no".into()),
        ("title", "Gregarioustypes".into()),
        ("audio-client-name", "Gregarioustypes".into()),
        ("target-colorspace-hint", "yes".into()),
        // SDR stays an sRGB swap chain (DWM maps it to the SDR white level);
        // HDR switches the swap chain to HDR10.
        ("target-colorspace-hint-mode", "source".into()),
        // ITU-R BT.2390 EETF: untouched below the knee, smooth roll-off to the
        // display peak (mpv's default spline darkens mid-tones of bright masters).
        ("tone-mapping", "bt.2390".into()),
        // Windows reports nonsense black levels for some panels (min = max = 80
        // nits here), which crushes SDR shadows: assume true black instead, so SDR
        // video passes through unchanged like in any other player.
        ("target-contrast", "inf".into()),
        ("gpu-shader-cache-dir", shader_cache.display().to_string()),
        ("sub-auto", "fuzzy".into()),
        ("audio-file-auto", "no".into()),
        ("cover-art-auto", "no".into()),
        ("image-display-duration", "inf".into()),
        ("background-color", "#000000".into()),
        ("demuxer-max-bytes", "64MiB".into()),
        ("demuxer-max-back-bytes", "16MiB".into()),
        ("volume-max", "100".into()),
        ("reset-on-next-file", "speed,video-zoom,video-pan-x,video-pan-y,video-rotate,af,vf,ab-loop-a,ab-loop-b,loop-file".into()),
    ];
    for (k, v) in opts {
        if let Err(e) = m.set_option(k, v) {
            tracing::debug!("mpv option {k}: {e}");
        }
    }
    m.request_log("warn");
    m.set_wakeup(wakeup, parent as *mut c_void);
    m.initialize()?;
    crate::midi::register_protocol(&m);
    for (id, name, fmt) in [
        (OBS_PAUSE, "pause", mpv::FORMAT_FLAG),
        (OBS_TIME, "time-pos", mpv::FORMAT_DOUBLE),
        (OBS_DURATION, "duration", mpv::FORMAT_DOUBLE),
        (OBS_EOF, "eof-reached", mpv::FORMAT_FLAG),
        (OBS_VOLUME, "volume", mpv::FORMAT_DOUBLE),
        (OBS_MUTE, "mute", mpv::FORMAT_FLAG),
        (OBS_VIDEO_PARAMS, "video-params", mpv::FORMAT_NODE),
        (OBS_TRACKS, "track-list", mpv::FORMAT_NODE),
        (OBS_CHAPTERS, "chapter-list", mpv::FORMAT_NODE),
        (OBS_BUFFERING, "paused-for-cache", mpv::FORMAT_FLAG),
        (OBS_CACHE, "demuxer-cache-time", mpv::FORMAT_DOUBLE),
        (OBS_VIDEO_OUT, "video-target-params", mpv::FORMAT_NODE),
        (OBS_HWDEC, "hwdec-current", mpv::FORMAT_STRING),
        (OBS_PLAYLIST_POS, "playlist-pos", mpv::FORMAT_INT64),
        (OBS_SEEKING, "seeking", mpv::FORMAT_FLAG),
        (OBS_AUDIO_PARAMS, "audio-params", mpv::FORMAT_NODE),
    ] {
        m.observe(id, name, fmt);
    }
    tracing::info!("mpv ready in {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    Ok(m)
}

impl Engine {
    fn new(parent: HWND) -> Engine {
        let host = create_host(parent);
        let (h, p) = (host.0 as isize, parent.0 as isize);
        let init = std::thread::Builder::new().name("gt-mpv-init".into()).spawn(move || start_mpv(h, p)).ok();
        Engine {
            parent,
            host,
            init,
            mpv: None,
            failed: None,
            st: PlayerState { volume: 100.0, ..Default::default() },
            gen: 0,
            want_entry: None,
            cur_entry: -1,
            follow: false,
            shown: false,
            rect: (0, 0, 0, 0),
            peak: None,
            volume: 100.0,
            load_at: None,
            url: String::new(),
        }
    }

    /// The mpv core, waiting for the background start-up if needed.
    pub fn mpv(&mut self) -> Option<Arc<Mpv>> {
        if let Some(j) = self.init.take() {
            match j.join() {
                Ok(Ok(m)) => {
                    self.mpv = Some(Arc::new(m));
                    // mpv only signals when its queue becomes non-empty; events queued
                    // during start-up must be drained now or it never signals again.
                    let msg = WAKE_MSG.load(Ordering::Relaxed);
                    if msg != 0 {
                        unsafe {
                            let _ = PostMessageW(Some(self.parent), msg, WPARAM(0), LPARAM(0));
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!("mpv unavailable: {e}");
                    self.failed = Some(e);
                }
                Err(_) => self.failed = Some("mpv start-up panicked".into()),
            }
        }
        self.mpv.clone()
    }

    pub fn failure(&self) -> Option<&str> {
        self.failed.as_deref()
    }

    pub fn parent(&self) -> HWND {
        self.parent
    }

    /// Tell mpv the display peak (HDR output is tone-mapped to it).
    /// Remember the display peak; HDR sources are tone-mapped to it (applied per
    /// file: SDR sources keep an SDR swap chain and must not get an HDR target peak).
    pub fn set_display(&mut self, hdr: bool, peak_nits: f32) {
        self.peak = hdr.then_some(peak_nits.round());
    }

    pub fn load(&mut self, url: &str, o: LoadOpts) {
        let Some(m) = self.mpv() else {
            self.st.error = self.failed.clone().or(Some("The media engine is unavailable".into()));
            return;
        };
        let mut opts = vec![format!("pause={}", if o.pause { "yes" } else { "no" })];
        if o.audio_only {
            opts.push("vid=no".into());
        }
        if o.start > 0.0 {
            opts.push(format!("start={}", o.start));
        }
        if let (true, Some(p)) = (o.hdr_source, self.peak) {
            opts.push(format!("target-peak={p}"));
        }
        opts.extend(o.extra);
        let joined = opts.join(",");
        self.gen += 1;
        self.want_entry = None;
        self.follow = false;
        self.st = PlayerState { volume: self.volume, mute: self.st.mute, ..Default::default() };
        self.load_at = Some(Instant::now());
        self.url = url.to_string();
        tracing::debug!("mpv loadfile {url} [{joined}] as #{}", self.gen);
        if let Err(e) = m.command_async(self.gen, &["loadfile", url, "replace", "-1", &joined]) {
            tracing::warn!("mpv loadfile: {e}");
            self.st.error = Some(e);
        }
    }

    /// Follow mpv's own playlist: each entry it starts becomes the current one.
    pub fn follow_playlist(&mut self, on: bool) {
        self.follow = on;
    }

    /// Queue more files after the current one (playlists).
    pub fn append(&mut self, url: &str, audio_only: bool) {
        if let Some(m) = self.mpv() {
            let opts = if audio_only { "vid=no" } else { "" };
            let _ = m.command_async(0, &["loadfile", url, "append", "-1", opts]);
        }
    }

    pub fn stop(&mut self) {
        if let Some(m) = self.mpv.clone() {
            let _ = m.command_async(0, &["stop"]);
        }
        self.gen += 1; // events of the stopped file no longer matter
        self.want_entry = None;
        self.follow = false;
        self.st = PlayerState { volume: self.volume, mute: self.st.mute, ..Default::default() };
        self.show(false);
    }

    pub fn command(&mut self, args: &[&str]) {
        if let Some(m) = self.mpv() {
            if let Err(e) = m.command_async(0, args) {
                tracing::debug!("mpv {args:?}: {e}");
            }
        }
    }

    pub fn set_pause(&mut self, p: bool) {
        if let Some(m) = self.mpv() {
            if !p && self.st.eof {
                // Replay from the start once the end was reached.
                let _ = m.command_async(0, &["seek", "0", "absolute", "exact"]);
            }
            m.set_flag_async("pause", p);
            self.st.paused = p;
        }
    }

    pub fn seek(&mut self, t: f64, exact: bool) {
        let t = t.clamp(0.0, if self.st.duration > 0.0 { self.st.duration } else { f64::MAX });
        self.command(&["seek", &format!("{t:.3}"), if exact { "absolute+exact" } else { "absolute+keyframes" }]);
        self.st.time = t;
    }

    pub fn set_volume(&mut self, v: f64) {
        let v = v.clamp(0.0, 100.0);
        self.volume = v;
        self.st.volume = v;
        if let Some(m) = self.mpv() {
            m.set_double_async("volume", v);
            if self.st.mute && v > 0.0 {
                m.set_flag_async("mute", false);
            }
        }
    }

    pub fn set_mute(&mut self, mute: bool) {
        self.st.mute = mute;
        if let Some(m) = self.mpv() {
            m.set_flag_async("mute", mute);
        }
    }

    pub fn set_track(&mut self, kind: &str, id: Option<i64>) {
        let prop = match kind {
            "audio" => "aid",
            "sub" => "sid",
            _ => "vid",
        };
        if let Some(m) = self.mpv() {
            m.set_string_async(prop, &id.map(|i| i.to_string()).unwrap_or_else(|| "no".into()));
        }
    }

    /// Place the video over `r` (client pixels of the parent); hidden while `visible` is false.
    pub fn place(&mut self, r: (i32, i32, i32, i32), visible: bool) {
        let visible = visible && r.2 > 0 && r.3 > 0;
        if r != self.rect {
            self.rect = r;
            unsafe {
                let _ = SetWindowPos(self.host, Some(HWND_BOTTOM), r.0, r.1, r.2.max(1), r.3.max(1), SWP_NOACTIVATE | SWP_NOZORDER);
                // mpv follows its parent, but nudge its window directly too (posted, never blocks).
                if let Ok(child) = GetWindow(self.host, GW_CHILD) {
                    let _ = SetWindowPos(child, None, 0, 0, r.2.max(1), r.3.max(1), SWP_NOACTIVATE | SWP_NOZORDER | SWP_ASYNCWINDOWPOS);
                }
            }
        }
        self.show(visible);
    }

    fn show(&mut self, v: bool) {
        if v != self.shown {
            self.shown = v;
            unsafe {
                let _ = ShowWindow(self.host, if v { SW_SHOWNA } else { SW_HIDE });
            }
        }
    }

    /// Drain mpv's event queue into `st`.
    pub fn pump(&mut self) -> Changes {
        let mut c = Changes::default();
        tracing::trace!("pump (wakes so far {})", WAKES.load(Ordering::Relaxed));
        if self.init.as_ref().is_some_and(|j| j.is_finished()) {
            self.mpv();
        }
        let Some(m) = self.mpv.clone() else { return c };
        let mut only_time = true;
        while let Some(ev) = m.wait_event(0.0) {
            let current = self.want_entry.is_some_and(|w| w == self.cur_entry);
            if !matches!(ev, Event::Property { id: OBS_TIME | OBS_CACHE | OBS_VIDEO_OUT, .. } | Event::Log { .. }) {
                tracing::debug!("mpv event {ev:?} (want {:?}, cur {})", self.want_entry, self.cur_entry);
            }
            match ev {
                Event::Reply { id, error, result } => {
                    if id == self.gen && id != 0 {
                        if error < 0 {
                            self.st.error = Some(m.error_string(error));
                            c.any = true;
                            only_time = false;
                        }
                        self.want_entry = result.int("playlist_entry_id");
                        if self.want_entry.is_some_and(|w| w == self.cur_entry) && !self.st.started {
                            self.st.started = true;
                            c.any = true;
                            only_time = false;
                        }
                    }
                }
                Event::Log { prefix, level, text } => {
                    if level == "error" || level == "fatal" {
                        tracing::warn!("mpv [{prefix}] {text}");
                        if current && self.st.error.is_none() && (prefix == "cplayer" || prefix == "ffmpeg/demuxer" || prefix == "stream") && !text.is_empty() {
                            self.st.error = Some(text);
                        }
                    } else {
                        tracing::debug!("mpv [{prefix}] {text}");
                    }
                }
                Event::StartFile { entry } => {
                    self.cur_entry = entry;
                    if self.follow {
                        self.want_entry = Some(entry);
                        let keep = (self.st.volume, self.st.mute);
                        self.st = PlayerState { volume: keep.0, mute: keep.1, ..Default::default() };
                    }
                    if self.want_entry == Some(entry) {
                        self.st.started = true;
                        c.any = true;
                        only_time = false;
                    }
                }
                Event::EndFile { reason, error, entry } => {
                    if self.want_entry == Some(entry) {
                        if reason == mpv::END_ERROR {
                            let msg = m.error_string(error);
                            tracing::warn!("mpv failed on {}: {msg}", self.url);
                            if self.st.error.is_none() {
                                self.st.error = Some(msg);
                            }
                        }
                        if reason == mpv::END_EOF || reason == mpv::END_ERROR {
                            self.st.ended = true;
                            c.ended = true;
                        }
                        c.any = true;
                        only_time = false;
                    }
                }
                Event::FileLoaded if current => {
                    self.st.loaded = true;
                    c.any = true;
                    only_time = false;
                    if let Some(t) = self.load_at {
                        tracing::info!("mpv loaded {} in {:.1} ms", self.url, t.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                Event::PlaybackRestart if current => {
                    if !self.st.first_frame {
                        self.st.first_frame = true;
                        c.first_frame = true;
                        if let Some(t) = self.load_at.take() {
                            tracing::info!("mpv playing {} {:.1} ms after loadfile (hwdec {})", self.url, t.elapsed().as_secs_f64() * 1000.0, if self.st.hwdec.is_empty() { "-" } else { &self.st.hwdec });
                        }
                    }
                    c.any = true;
                    only_time = false;
                }
                Event::Property { id, value, .. } => {
                    if !current && id != OBS_VOLUME && id != OBS_MUTE {
                        continue;
                    }
                    // Output parameters carry per-frame HDR statistics: only the
                    // transfer and peak matter here.
                    let before = (self.st.out_gamma.clone(), self.st.out_max_luma);
                    if id == OBS_VIDEO_OUT {
                        tracing::trace!("video-target-params {value:?}");
                    }
                    self.apply_property(id, value);
                    if id == OBS_VIDEO_OUT {
                        if before == (self.st.out_gamma.clone(), self.st.out_max_luma) {
                            continue;
                        }
                        tracing::info!("mpv output: {} peak {:.0} nits (source {} {})", self.st.out_gamma, self.st.out_max_luma, self.st.video_gamma, self.st.video_prim);
                    }
                    c.any = true;
                    if id != OBS_TIME && id != OBS_CACHE {
                        only_time = false;
                    }
                }
                Event::Shutdown => {
                    self.failed = Some("mpv shut down".into());
                }
                _ => {}
            }
        }
        c.time_only = c.any && only_time;
        c
    }

    fn apply_property(&mut self, id: u64, v: Node) {
        let flag = |v: &Node| matches!(v, Node::Flag(true));
        let num = |v: &Node| match v {
            Node::Double(d) => *d,
            Node::Int(i) => *i as f64,
            _ => 0.0,
        };
        let st = &mut self.st;
        match id {
            OBS_PAUSE => st.paused = flag(&v),
            OBS_TIME => st.time = num(&v),
            OBS_DURATION => st.duration = num(&v),
            OBS_EOF => st.eof = flag(&v),
            OBS_VOLUME => {
                st.volume = num(&v);
                self.volume = st.volume;
            }
            OBS_MUTE => st.mute = flag(&v),
            OBS_BUFFERING => st.buffering = flag(&v),
            OBS_SEEKING => st.seeking = flag(&v),
            OBS_CACHE => st.cache_time = num(&v),
            OBS_HWDEC => {
                st.hwdec = match v {
                    Node::Str(s) => s,
                    _ => String::new(),
                }
            }
            OBS_PLAYLIST_POS => st.playlist_pos = num(&v) as i64,
            OBS_VIDEO_PARAMS => {
                st.video_w = v.int("dw").or(v.int("w")).unwrap_or(0);
                st.video_h = v.int("dh").or(v.int("h")).unwrap_or(0);
                st.video_gamma = v.str("gamma").unwrap_or("").to_string();
                st.video_prim = v.str("primaries").unwrap_or("").to_string();
                st.sig_peak = v.num("sig-peak").unwrap_or(0.0);
            }
            OBS_VIDEO_OUT => {
                st.out_gamma = v.str("gamma").unwrap_or("").to_string();
                st.out_max_luma = v.num("max-luma").unwrap_or(0.0);
            }
            OBS_AUDIO_PARAMS => {
                let rate = v.int("samplerate").unwrap_or(0);
                let ch = v.str("hr-channels").or(v.str("channels")).unwrap_or("").to_string();
                st.audio_desc = if rate > 0 { format!("{} Hz {}", rate, ch) } else { String::new() };
            }
            OBS_TRACKS => {
                st.tracks = v
                    .list()
                    .iter()
                    .map(|t| Track {
                        id: t.int("id").unwrap_or(0),
                        kind: t.str("type").unwrap_or("").to_string(),
                        title: t.str("title").unwrap_or("").to_string(),
                        lang: t.str("lang").unwrap_or("").to_string(),
                        codec: t.str("codec").unwrap_or("").to_string(),
                        selected: t.flag("selected"),
                        default: t.flag("default"),
                        external: t.flag("external"),
                        albumart: t.flag("albumart"),
                        channels: t.int("demux-channel-count").unwrap_or(0),
                    })
                    .collect();
            }
            OBS_CHAPTERS => {
                st.chapters = v.list().iter().map(|c| (c.num("time").unwrap_or(0.0), c.str("title").unwrap_or("").to_string())).collect();
            }
            _ => {}
        }
    }

    pub fn host(&self) -> HWND {
        self.host
    }
}

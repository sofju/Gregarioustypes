//! The archive/disc preview: facts on top, the entry tree below, nested preview
//! on double-click, and for Blu-ray discs a summary, "Play" and "BDInfo".

use crate::bdmv;
use crate::listing::{self, Access, ListEntry, Listing};
use crate::sevenzip::Progress;
use gt_core::CancelToken;
use gt_ui::widgets::doclist::{DocList, Row, TreeBuilder};
use gt_ui::{glyph, rgba, Action, Align, DrawCx, Event, EventCx, Font, Fonts, HostCx, Rect, Request, View, ViewEnv};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

struct Job {
    rx: Receiver<Result<PathBuf, String>>,
    name: String,
    size: u64,
    progress: Arc<Progress>,
    cancel: CancelToken,
    started: Instant,
}

pub struct ArchiveView {
    doc: Option<DocList>,
    path: PathBuf,
    access: Option<Access>,
    entries: Arc<Vec<ListEntry>>,
    bd: Option<bdmv::Disc>,
    host: Option<HostCx>,
    job: Option<Job>,
    request: Option<Request>,
    subtitle: String,
    info: Vec<(String, String)>,
    status: Option<(String, Instant)>,
    /// Listing still being read (large or slow archives).
    pending: Option<Arc<Progress>>,
}

fn fmt_ratio(packed: u64, size: u64) -> String {
    if size == 0 {
        String::new()
    } else {
        format!("{:.0}%", packed as f64 * 100.0 / size as f64)
    }
}

pub fn temp_root() -> PathBuf {
    gt_core::paths::cache_dir().join("extract")
}

/// Remove files extracted for nested previews.
pub fn clear_temp() {
    let _ = std::fs::remove_dir_all(temp_root());
}

fn build(l: &Listing) -> (DocList, String, Vec<(String, String)>) {
    let files = l.entries.iter().filter(|e| !e.is_dir).count();
    let dirs = l.entries.iter().filter(|e| e.is_dir).count();
    let total: u64 = l.entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
    let packed: u64 = l.entries.iter().filter(|e| !e.is_dir).filter_map(|e| e.packed).sum();
    let has_packed = l.entries.iter().any(|e| e.packed.is_some_and(|p| p > 0)) && !l.is_disc;
    let encrypted = l.entries.iter().filter(|e| e.encrypted).count();
    let archive_size = std::fs::metadata(&l.path).map(|m| m.len()).unwrap_or(0);
    let mut rows = Vec::new();
    let mut info = Vec::new();
    let mut kind = l.kind.clone();
    if l.chain.len() > 1 {
        kind = format!("{kind} · {}", l.chain.join(" › "));
    }
    rows.push(Row::field("Type", kind.clone()));
    info.push(("Type".to_string(), kind));
    let counts = format!("{} file{}{}", gt_core::fmt::group(files as u64), if files == 1 { "" } else { "s" }, if dirs > 0 { format!(", {} folder{}", gt_core::fmt::group(dirs as u64), if dirs == 1 { "" } else { "s" }) } else { String::new() });
    rows.push(Row::field("Contents", counts.clone()));
    info.push(("Contents".into(), counts));
    rows.push(Row::field("Total size", gt_core::fmt::size_long(total)));
    if has_packed && packed > 0 {
        rows.push(Row::field("Compressed", format!("{} ({} of the original)", gt_core::fmt::size(packed), fmt_ratio(packed, total))));
    } else if archive_size > 0 && !l.is_disc && total > 0 && archive_size < total {
        rows.push(Row::field("Compressed", format!("{} ({} of the original)", gt_core::fmt::size(archive_size), fmt_ratio(archive_size, total))));
    } else if archive_size > 0 && !l.is_disc {
        rows.push(Row::field("Archive size", gt_core::fmt::size_long(archive_size)));
    }
    if encrypted > 0 {
        rows.push(Row::field("Encrypted", format!("{} file{} need a password", gt_core::fmt::group(encrypted as u64), if encrypted == 1 { "" } else { "s" })));
    }
    for (k, v) in &l.facts {
        rows.push(Row::field(k.clone(), v.clone()));
        info.push((k.clone(), v.clone()));
    }
    if !l.package.is_empty() {
        rows.push(Row::Heading("Package".into()));
        for (k, v) in &l.package {
            rows.push(Row::field(k.clone(), v.clone()));
        }
        if let Some(d) = &l.description {
            for line in d.lines().take(12) {
                rows.push(Row::Text(line.to_string()));
            }
        }
    }
    if let Some(bd) = &l.bd {
        rows.push(Row::Heading(if bd.uhd { "Ultra HD Blu-ray".into() } else { "Blu-ray disc".into() }));
        if !bd.title.is_empty() {
            rows.push(Row::field("Disc title", bd.title.clone()));
        }
        for (k, v) in &bd.facts {
            rows.push(Row::field(k.clone(), v.clone()));
        }
        if bd.encrypted {
            rows.push(Row::Text("The stream files are still AACS-encrypted: only the disc structure can be read and played titles won't decode.".into()));
        }
        if let Some(m) = bd.main() {
            rows.push(Row::field("Main title", format!("{} · {} · {} chapter{}", m.name, bdmv::fmt_duration(m.seconds), m.chapters, if m.chapters == 1 { "" } else { "s" })));
            for (k, v) in m.describe_streams() {
                rows.push(Row::field(k, v));
            }
            info.push(("Main title".into(), format!("{} ({})", m.name, bdmv::fmt_duration(m.seconds))));
        }
        let presented = bd.playlists.iter().filter(|p| p.presented).count();
        if bd.playlists.len() > 1 {
            rows.push(Row::field("Playlists", format!("{} ({} feature-length, without short or looping ones)", bd.playlists.len(), presented)));
        }
    }
    if !l.audio_tracks.is_empty() {
        rows.push(Row::Heading("Audio tracks".into()));
        for t in &l.audio_tracks {
            let s = t.seconds.round() as u64;
            rows.push(Row::field(format!("Track {}", t.number), format!("{}:{:02}", s / 60, s % 60)));
        }
    }
    for n in &l.notes {
        rows.push(Row::Text(n.clone()));
    }
    rows.push(Row::Spacer);
    let mut cols = Vec::new();
    if has_packed {
        cols.push("Packed".to_string());
    }
    cols.push("Modified".to_string());
    let mut tb = TreeBuilder::new(cols);
    for e in &l.entries {
        let parts: Vec<String> = e.path.split(['/', '\\']).filter(|s| !s.is_empty() && *s != ".").map(|s| s.to_string()).collect();
        if parts.is_empty() {
            continue;
        }
        let mut extra = Vec::new();
        if has_packed {
            extra.push(e.packed.map(gt_core::fmt::size).unwrap_or_default());
        }
        extra.push(e.mtime.map(gt_core::fmt::unix_time_short).unwrap_or_default());
        if e.is_dir {
            tb.dir_extra(&parts, if has_packed { vec![String::new(), extra.pop().unwrap_or_default()] } else { extra });
        } else {
            tb.add_entry(&parts, e.size, extra, e.id, e.encrypted);
        }
    }
    // Expand the first level, or more when the tree is small.
    let depth = if l.entries.len() < 60 { 3 } else { 1 };
    let tree = tb.finish(depth);
    let subtitle = format!("{} · {}", l.chain.first().map(|s| s.as_str()).unwrap_or(""), if files == 1 { "1 file".into() } else { format!("{} files", gt_core::fmt::group(files as u64)) });
    let mut doc = DocList::new("archive", rows, Some(tree), Some(subtitle.clone()));
    doc.set_extra_width(if has_packed { 110.0 } else { 140.0 });
    doc.set_key_width(130.0);
    (doc, subtitle, info)
}

impl ArchiveView {
    pub fn new(listing: Listing) -> ArchiveView {
        let (doc, subtitle, info) = build(&listing);
        ArchiveView {
            doc: Some(doc),
            path: listing.path.clone(),
            access: listing.access.clone(),
            entries: Arc::new(listing.entries),
            bd: listing.bd,
            host: None,
            job: None,
            request: None,
            subtitle,
            info,
            status: None,
            pending: None,
        }
    }

    /// Placeholder while a slow archive is still being read.
    pub fn pending(path: PathBuf, progress: Arc<Progress>) -> ArchiveView {
        ArchiveView {
            doc: None,
            path,
            access: None,
            entries: Arc::new(Vec::new()),
            bd: None,
            host: None,
            job: None,
            request: None,
            subtitle: String::new(),
            info: Vec::new(),
            status: None,
            pending: Some(progress),
        }
    }

    fn open_entry(&mut self, id: u32) {
        let Some(access) = self.access.clone() else { return };
        let Some(e) = self.entries.iter().find(|e| e.id == id).cloned() else { return };
        if let Some(j) = self.job.take() {
            j.cancel.cancel();
        }
        if e.encrypted {
            self.status = Some(("This file is encrypted: it needs the archive's password".into(), Instant::now()));
            return;
        }
        let name = e.path.rsplit(['/', '\\']).next().unwrap_or("item").to_string();
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            self.path.hash(&mut h);
            e.path.hash(&mut h);
            format!("{:016x}", h.finish())
        };
        let dir = temp_root().join(key);
        let dest = dir.join(&name);
        let (tx, rx) = std::sync::mpsc::channel();
        let progress = Arc::new(Progress::default());
        let cancel = CancelToken::new();
        let (p2, c2) = (progress.clone(), cancel.clone());
        let host = self.host.map(|h| (h.parent.0 as isize, h.wake_msg));
        std::thread::Builder::new()
            .name("gt-extract".into())
            .spawn(move || {
                let r = (|| {
                    if dest.is_file() && std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(u64::MAX) == e.size {
                        return Ok(dest.clone());
                    }
                    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                    let tmp = dest.with_extension("partial");
                    let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
                    listing::extract(&access, id, &mut f, p2, &c2)?;
                    drop(f);
                    std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
                    Ok(dest.clone())
                })();
                let _ = tx.send(r);
                if let Some((hwnd, msg)) = host {
                    unsafe {
                        let _ = PostMessageW(Some(windows::Win32::Foundation::HWND(hwnd as *mut _)), msg, WPARAM(0), LPARAM(0));
                    }
                }
            })
            .ok();
        self.job = Some(Job { rx, name, size: e.size, progress, cancel, started: Instant::now() });
    }

    /// MediaInfo of the main title: its playlist, read with the clip info and the
    /// start of its stream (copied out of an image; used in place in a folder).
    fn title_mediainfo_job(&self) -> Option<gt_ui::ReportJob> {
        let main = self.bd.as_ref()?.main()?.clone();
        let access = self.access.clone()?;
        let entries = self.entries.clone();
        let disc_path = self.path.clone();
        Some(Box::new(move |cancel: &CancelToken, progress: &dyn Fn(String)| {
            let find = |suffix: &str| entries.iter().find(|e| !e.is_dir && e.path.replace('\\', "/").to_ascii_lowercase().ends_with(&suffix.to_ascii_lowercase()) && !e.path.to_ascii_lowercase().contains("backup")).cloned();
            let mpls = find(&format!("BDMV/PLAYLIST/{}", main.name)).ok_or("the main playlist is missing")?;
            if let Access::Files(paths) = &access {
                let p = paths.get(mpls.id as usize).ok_or("missing playlist")?;
                return gt_fmt_media::mediainfo::report(p, false);
            }
            let key = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                disc_path.hash(&mut h);
                format!("bd-{:016x}", h.finish())
            };
            let root = temp_root().join(key).join("BDMV");
            for d in ["PLAYLIST", "CLIPINF", "STREAM"] {
                std::fs::create_dir_all(root.join(d)).map_err(|e| e.to_string())?;
            }
            let copy = |e: &ListEntry, dest: &Path, limit: u64| -> Result<(), String> {
                struct Limited(std::fs::File, u64);
                impl std::io::Write for Limited {
                    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                        if self.1 == 0 {
                            return Err(std::io::ErrorKind::WriteZero.into());
                        }
                        let n = (b.len() as u64).min(self.1) as usize;
                        self.0.write_all(&b[..n])?;
                        self.1 -= n as u64;
                        Ok(n)
                    }
                    fn flush(&mut self) -> std::io::Result<()> {
                        Ok(())
                    }
                }
                if dest.is_file() {
                    return Ok(());
                }
                let mut f = Limited(std::fs::File::create(dest).map_err(|e| e.to_string())?, limit);
                let r = listing::extract(&access, e.id, &mut f, Arc::new(Progress::default()), cancel);
                if r.is_err() && f.1 > 0 {
                    let _ = std::fs::remove_file(dest);
                    return r;
                }
                Ok(())
            };
            progress("Reading the main title…".into());
            let mpls_dest = root.join("PLAYLIST").join(&main.name);
            copy(&mpls, &mpls_dest, u64::MAX)?;
            for clip in main.clips.iter().take(1) {
                let stem = clip.trim_end_matches(".m2ts");
                if let Some(c) = find(&format!("BDMV/CLIPINF/{stem}.clpi")) {
                    copy(&c, &root.join("CLIPINF").join(format!("{stem}.clpi")), u64::MAX)?;
                }
                if let Some(m) = find(&format!("BDMV/STREAM/{clip}")) {
                    progress("Reading the start of the main stream…".into());
                    copy(&m, &root.join("STREAM").join(clip), 48 << 20)?;
                }
            }
            if cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            gt_fmt_media::mediainfo::report(&mpls_dest, false)
        }))
    }

    fn main_playlist(&self) -> Option<u32> {
        let m = self.bd.as_ref()?.main()?;
        m.name.trim_end_matches(".mpls").trim_end_matches(".MPLS").parse().ok()
    }
}

impl View for ArchiveView {
    fn kind(&self) -> &'static str {
        "archive"
    }

    fn preferred_size(&mut self, fonts: &mut Fonts, scale: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        match self.doc.as_mut() {
            Some(d) => d.preferred_size(fonts, scale, max_w, max_h),
            None => ((720.0 * scale).min(max_w), (420.0 * scale).min(max_h)),
        }
    }

    fn attach(&mut self, host: &HostCx) {
        self.host = Some(*host);
    }

    fn detach(&mut self) {
        if let Some(j) = self.job.take() {
            j.cancel.cancel();
        }
    }

    fn upgrade(&mut self, _env: &ViewEnv, payload: Box<dyn std::any::Any + Send>) -> bool {
        match payload.downcast::<Result<Listing, String>>() {
            Ok(r) => {
                match *r {
                    Ok(l) => {
                        let fresh = ArchiveView::new(l);
                        let host = self.host;
                        *self = fresh;
                        self.host = host;
                    }
                    Err(e) => {
                        self.pending = None;
                        self.status = Some((format!("Can't read this archive: {e}"), Instant::now()));
                    }
                }
                true
            }
            Err(_) => false,
        }
    }

    fn draw(&mut self, cx: &mut DrawCx, r: Rect) {
        if let Some(d) = self.doc.as_mut() {
            d.draw(cx, r);
        } else if let Some(p) = &self.pending {
            let files = p.files.load(Ordering::Relaxed);
            let (done, total) = (p.done_bytes.load(Ordering::Relaxed), p.total_bytes.load(Ordering::Relaxed));
            let mut t = "Reading archive…".to_string();
            if files > 0 {
                t = format!("Reading archive… {} items", gt_core::fmt::group(files));
            }
            if total > 0 {
                t = format!("{t} · {:.0}%", done as f64 * 100.0 / total as f64);
            }
            cx.text(&t, r.inset(cx.px(24.0)), Font::Ui(13.0), cx.theme.text2, Align::Center);
        } else if let Some((s, _)) = &self.status {
            cx.text_wrapped(s, r.inset(cx.px(24.0)), Font::Ui(13.0), rgba(1.0, 0.62, 0.55, 1.0));
        }
        // Extraction progress / status, as a pill at the bottom.
        let msg = if let Some(j) = &self.job {
            let done = j.progress.done_bytes.load(Ordering::Relaxed);
            let total = j.progress.total_bytes.load(Ordering::Relaxed).max(j.size);
            (j.started.elapsed() > Duration::from_millis(150)).then(|| if total > 0 { format!("Opening {}… {:.0}%", j.name, (done as f64 * 100.0 / total as f64).min(100.0)) } else { format!("Opening {}…", j.name) })
        } else if self.doc.is_some() {
            self.status.as_ref().filter(|(_, t)| t.elapsed() < Duration::from_secs(4)).map(|(s, _)| s.clone())
        } else {
            None
        };
        if let Some(m) = msg {
            let (tw, th) = cx.measure(&m, Font::Ui(12.5));
            let w = (tw + cx.px(28.0)).min(r.w - cx.px(20.0));
            let b = Rect::new(r.x + (r.w - w) / 2.0, r.bottom() - th - cx.px(36.0), w, th + cx.px(14.0));
            cx.round(b, b.h / 2.0, rgba(0.07, 0.07, 0.075, 0.92));
            cx.text(&m, b, Font::Ui(12.5), cx.theme.text, Align::Center);
        }
    }

    fn event(&mut self, cx: &mut EventCx, r: Rect, ev: &Event) {
        if let Some(d) = self.doc.as_mut() {
            d.event(cx, r, ev);
            if let Some(id) = d.take_activated() {
                self.open_entry(id);
                cx.redraw = true;
            }
        }
    }

    fn next_frame_in(&self) -> Option<Duration> {
        if self.job.is_some() || self.pending.is_some() {
            Some(Duration::from_millis(120))
        } else if self.status.as_ref().is_some_and(|(_, t)| t.elapsed() < Duration::from_secs(4)) {
            Some(Duration::from_millis(500))
        } else {
            None
        }
    }

    fn tick(&mut self, _env: &ViewEnv) -> bool {
        self.wake();
        self.job.is_some() || self.pending.is_some() || self.status.is_some()
    }

    fn wake(&mut self) -> bool {
        let Some(j) = &self.job else { return false };
        match j.rx.try_recv() {
            Ok(Ok(path)) => {
                let title = j.name.clone();
                self.job = None;
                self.request = Some(Request::OpenNested { path, title });
                true
            }
            Ok(Err(e)) => {
                self.job = None;
                self.status = Some((format!("Can't open this file: {e}"), Instant::now()));
                true
            }
            Err(_) => false,
        }
    }

    fn take_request(&mut self) -> Option<Request> {
        self.request.take()
    }

    fn actions(&self) -> Vec<Action> {
        let mut v = Vec::new();
        if self.bd.is_some() {
            v.push(Action::new("play", glyph::PLAY, "Play the main title").label("Play"));
            v.push(Action::new("bdinfo", glyph::DISC, "BDInfo report (scans the whole disc)").label("BDInfo"));
            v.push(Action::new("mediainfo", glyph::REPORT, "MediaInfo of the main title").label("MediaInfo"));
        }
        v
    }

    fn on_action(&mut self, id: &str, cx: &mut EventCx) {
        match id {
            "play" => {
                let Some(n) = self.main_playlist() else { return };
                let device = self.path.display().to_string();
                let src = gt_fmt_media::view::Source { path: device.clone(), url: format!("bd://mpls/{n}"), extra: vec![format!("bluray-device={device}")] };
                let title = format!("Main title ({:05}.mpls)", n);
                let uhd = self.bd.as_ref().is_some_and(|b| b.uhd);
                let view = gt_fmt_media::view::VideoView::for_source(src, None, uhd);
                self.request = Some(Request::PushView { view: Box::new(view), title });
                cx.redraw = true;
            }
            "mediainfo" => {
                if let Some(job) = self.title_mediainfo_job() {
                    self.request = Some(Request::Report { title: "MediaInfo", job });
                }
            }
            "bdinfo" => {
                let path = self.path.clone();
                self.request = Some(Request::Report { title: "BDInfo", job: Box::new(move |cancel, progress| crate::bdinfo::run(&path, cancel, progress)) });
            }
            _ => {}
        }
    }

    fn copy_text(&self) -> Option<String> {
        self.doc.as_ref().and_then(|d| d.copy_text())
    }

    fn info(&self) -> Vec<(String, String)> {
        self.info.clone()
    }

    fn subtitle(&self) -> Option<String> {
        (!self.subtitle.is_empty()).then(|| self.subtitle.clone())
    }

    fn wants_focus(&self) -> bool {
        false
    }
}

pub fn is_temp(path: &Path) -> bool {
    path.starts_with(temp_root())
}

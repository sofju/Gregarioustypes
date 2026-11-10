//! Open an archive or disc image and produce a flat listing plus facts:
//! 7-Zip for the container (following each handler's "main sub-file", e.g.
//! DMG → HFS+), our streaming readers for tar/cpio inside compressed streams,
//! package metadata for .deb/.rpm and the Blu-ray summary for BDMV discs.

use crate::bdmv;
use crate::disc::{self, AudioTrack};
use crate::sevenzip::{self, filetime_to_unix, Archive, OpenError, PipeReader, PipeWriter, Progress, Prop, Source};
use crate::stream::{self, StreamEntry};
use gt_core::CancelToken;
use parking_lot::Mutex;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

/// The chain of opened archives, outermost first; entries come from the last.
pub struct Opened {
    pub chain: Vec<Archive>,
    /// The archive file itself, when the outermost archive is read straight
    /// from disk (RAR data is extracted from it by 7z.exe, see `rar`).
    pub file: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamKind {
    Tar,
    Cpio,
}

#[derive(Clone)]
pub enum Access {
    None,
    /// Entry id = item index in the innermost archive.
    Archive(Arc<Mutex<Opened>>),
    /// Entry id = ordinal in the tar/cpio stream of `item` (innermost archive).
    Stream { arc: Arc<Mutex<Opened>>, item: u32, kind: StreamKind },
    /// Entry ids index `paths` (folders on disk, e.g. a BDMV folder).
    Files(Arc<Vec<PathBuf>>),
}

#[derive(Clone, Debug, Default)]
pub struct ListEntry {
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub packed: Option<u64>,
    pub mtime: Option<i64>,
    pub encrypted: bool,
    pub id: u32,
}

#[derive(Default)]
pub struct Listing {
    /// "ZIP archive", "ISO disc image"...
    pub kind: String,
    /// Formats opened along the way ("Dmg", "HFS").
    pub chain: Vec<String>,
    pub facts: Vec<(String, String)>,
    pub package: Vec<(String, String)>,
    pub description: Option<String>,
    pub entries: Vec<ListEntry>,
    pub access: Option<Access>,
    pub bd: Option<bdmv::Disc>,
    pub audio_tracks: Vec<AudioTrack>,
    pub notes: Vec<String>,
    pub elapsed_ms: f64,
    pub path: PathBuf,
    pub is_disc: bool,
}

const COMPRESSORS: &[&str] = &["gzip", "bzip2", "xz", "zstd", "lzma", "lzma86", "z", "lz4", "lzip", "brotli", "lz5", "lizard", "zst"];

fn kind_name(format: &str, is_disc: bool) -> String {
    let f = format.to_ascii_lowercase();
    if is_disc {
        return match f.as_str() {
            "udf" => "Disc image (UDF)".into(),
            "iso" => "Disc image (ISO 9660)".into(),
            _ => format!("Disc image ({format})"),
        };
    }
    match f.as_str() {
        "zip" => "ZIP archive".into(),
        "7z" => "7z archive".into(),
        "rar" | "rar5" => "RAR archive".into(),
        "tar" => "Tar archive".into(),
        "gzip" => "Gzip-compressed file".into(),
        "bzip2" => "Bzip2-compressed file".into(),
        "xz" => "XZ-compressed file".into(),
        "zstd" => "Zstandard-compressed file".into(),
        "cpio" => "cpio archive".into(),
        "ar" => "Debian package".into(),
        "rpm" => "RPM package".into(),
        "xar" => "XAR archive (macOS installer package)".into(),
        "dmg" => "Apple disk image".into(),
        "cab" => "Cabinet archive".into(),
        "wim" => "Windows imaging (WIM) archive".into(),
        "udf" | "iso" => "Disc image".into(),
        "hfs" => "HFS+ volume".into(),
        "apfs" => "APFS volume".into(),
        _ => format!("{format} archive"),
    }
}

fn time_fact(p: Prop) -> Option<String> {
    match p {
        Prop::Time(t) if t > 0 => Some(gt_core::fmt::unix_time(filetime_to_unix(t))),
        _ => None,
    }
}

/// Stream item `item` of the innermost archive through a pipe into `f`.
fn with_stream<T>(arc: &Arc<Mutex<Opened>>, item: u32, cancel: &CancelToken, f: impl FnOnce(&mut PipeReader) -> T) -> T {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(8);
    let a = arc.clone();
    let c = cancel.clone();
    let worker = std::thread::Builder::new()
        .name("gt-7z-stream".into())
        .spawn(move || {
            let opened = a.lock();
            if let Some(inner) = opened.chain.last() {
                let _ = inner.extract(item, Box::new(PipeWriter(tx)), Arc::new(Progress::default()), &c);
            }
        })
        .expect("stream thread");
    let mut reader = PipeReader::new(rx);
    let out = f(&mut reader);
    drop(reader); // the writer fails and extraction stops if we quit early
    let _ = worker.join();
    out
}

/// List a tar or cpio stream inside a compressed single-item archive.
fn stream_listing(arc: &Arc<Mutex<Opened>>, item: u32, cancel: &CancelToken, progress: &Progress) -> Option<(StreamKind, Vec<StreamEntry>)> {
    with_stream(arc, item, cancel, |r| {
        let mut head = vec![0u8; 512];
        let mut n = 0;
        while n < head.len() {
            match r.read(&mut head[n..]) {
                Ok(0) | Err(_) => break,
                Ok(k) => n += k,
            }
        }
        head.truncate(n);
        let kind = if stream::is_tar(&head) {
            StreamKind::Tar
        } else if stream::is_cpio(&head) {
            StreamKind::Cpio
        } else {
            return None;
        };
        let mut chained = std::io::Cursor::new(head).chain(r);
        let mut v = Vec::new();
        let on = |e: StreamEntry| {
            progress.files.fetch_add(1, Ordering::Relaxed);
            v.push(e);
        };
        let res = match kind {
            StreamKind::Tar => stream::tar(&mut chained, cancel, on, None),
            StreamKind::Cpio => stream::cpio(&mut chained, cancel, on, None),
        };
        if let Err(e) = res {
            if e == "cancelled" {
                return None;
            }
            tracing::warn!("stream listing stopped: {e}");
        }
        Some((kind, v))
    })
}

/// Extract entry `id` into `sink`.
pub fn extract(access: &Access, id: u32, sink: &mut dyn std::io::Write, progress: Arc<Progress>, cancel: &CancelToken) -> Result<(), String> {
    match access {
        Access::None => Err("no data".into()),
        Access::Files(paths) => {
            let p = paths.get(id as usize).ok_or("no such entry")?;
            let mut f = std::fs::File::open(p).map_err(|e| e.to_string())?;
            std::io::copy(&mut f, sink).map_err(|e| e.to_string())?;
            Ok(())
        }
        Access::Archive(arc) => {
            // RAR data comes out through the separate 7-Zip program.
            let rar = {
                let o = arc.lock();
                match (&o.file, o.chain.as_slice()) {
                    (Some(file), [only]) if crate::rar::is_rar(&only.format) => {
                        let entry = only.prop(id, sevenzip::KPID_PATH).str().unwrap_or("").to_string();
                        Some((file.clone(), entry, only.prop(id, sevenzip::KPID_SIZE).num().unwrap_or(0)))
                    }
                    _ => None,
                }
            };
            if let Some((file, entry, size)) = rar {
                return crate::rar::extract(&file, &entry, size, sink, &progress, cancel);
            }
            struct Fwd<'a>(&'a mut dyn std::io::Write);
            impl std::io::Write for Fwd<'_> {
                fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                    self.0.write(b)
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    self.0.flush()
                }
            }
            // `extract` needs a 'static sink: go through a pipe.
            let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(8);
            let a = arc.clone();
            let c = cancel.clone();
            let worker = std::thread::spawn(move || {
                let o = a.lock();
                match o.chain.last() {
                    Some(inner) => inner.extract(id, Box::new(PipeWriter(tx)), progress, &c).map(|_| ()),
                    None => Err("closed".into()),
                }
            });
            let mut r = PipeReader::new(rx);
            let copied = std::io::copy(&mut r, &mut Fwd(sink)).map_err(|e| e.to_string());
            let res = worker.join().map_err(|_| "extraction panicked".to_string())?;
            res?;
            copied.map(|_| ())
        }
        Access::Stream { arc, item, kind } => {
            let kind = *kind;
            with_stream(arc, *item, cancel, |r| match kind {
                StreamKind::Tar => stream::tar(r, cancel, |_| {}, Some((id as usize, sink))),
                StreamKind::Cpio => stream::cpio(r, cancel, |_| {}, Some((id as usize, sink))),
            })
        }
    }
}

/// Read a small entry fully into memory.
pub fn read_entry(access: &Access, id: u32, cancel: &CancelToken) -> Option<Vec<u8>> {
    let mut v = Vec::new();
    extract(access, id, &mut v, Arc::new(Progress::default()), cancel).ok()?;
    Some(v)
}

fn split(path: &str) -> Vec<&str> {
    path.split(['/', '\\']).filter(|s| !s.is_empty() && *s != ".").collect()
}

/// Find a Blu-ray structure among the entries and summarise it.
fn bluray(entries: &[ListEntry], access: &Access, cancel: &CancelToken) -> Option<bdmv::Disc> {
    let lower = |e: &ListEntry| e.path.replace('\\', "/").to_ascii_lowercase();
    let index = entries.iter().find(|e| !e.is_dir && lower(e).ends_with("bdmv/index.bdmv") && !lower(e).contains("backup/"))?;
    let root = lower(index).trim_end_matches("index.bdmv").to_string();
    let mut disc = bdmv::Disc::default();
    let mut lists: Vec<&ListEntry> = entries.iter().filter(|e| !e.is_dir && lower(e).starts_with(&format!("{root}playlist/")) && lower(e).ends_with(".mpls")).collect();
    lists.sort_by(|a, b| a.path.cmp(&b.path));
    for e in lists.into_iter().take(200) {
        if cancel.is_cancelled() {
            return None;
        }
        let name = split(&e.path).last().copied().unwrap_or("").to_string();
        if let Some(data) = read_entry(access, e.id, cancel) {
            if let Some(mut p) = bdmv::parse_mpls(&name, &data) {
                if p.streams.iter().any(|s| s.codec == "HEVC") {
                    disc.uhd = true;
                }
                p.presented = p.seconds >= 20.0;
                disc.playlists.push(p);
            }
        }
    }
    let meta = entries
        .iter()
        .filter(|e| !e.is_dir && lower(e).starts_with(&format!("{root}meta/dl/bdmt_")) && lower(e).ends_with(".xml"))
        .min_by_key(|e| if lower(e).ends_with("_eng.xml") { 0 } else { 1 });
    if let Some(m) = meta {
        if let Some(t) = read_entry(access, m.id, cancel).and_then(|x| bdmv::meta_title(&x)) {
            disc.title = t;
        }
    }
    disc.playlists.sort_by(|a, b| b.seconds.partial_cmp(&a.seconds).unwrap_or(std::cmp::Ordering::Equal));
    Some(disc)
}

/// Debian control fields from the `control.tar.*` member of the outer `ar`.
fn deb_info(outer: &Archive, cancel: &CancelToken) -> Option<(Vec<(String, String)>, Option<String>)> {
    let entries = outer.entries(cancel);
    let ctl = entries.iter().find(|e| e.path.starts_with("control.tar"))?;
    let nested = outer.nested(ctl.index, &ctl.path, cancel)?;
    let arc = Arc::new(Mutex::new(Opened { chain: vec![nested], file: None }));
    let progress = Progress::default();
    let (kind, list) = stream_listing(&arc, 0, cancel, &progress)?;
    let ordinal = list.iter().position(|e| e.path == "control" || e.path.ends_with("/control"))?;
    let access = Access::Stream { arc, item: 0, kind };
    let data = read_entry(&access, ordinal as u32, cancel)?;
    Some(crate::package::deb_control(&String::from_utf8_lossy(&data)))
}

pub fn open(path: &Path, head: &[u8], progress: Arc<Progress>, cancel: &CancelToken) -> Result<Listing, String> {
    let t = Instant::now();
    let lib = sevenzip::lib()?;
    let ext = gt_core::sniff::ext(path);
    let mut listing = Listing { path: path.to_path_buf(), ..Default::default() };
    // Disc images 7-Zip can't read directly become a virtual ISO.
    let mut src: Option<Box<dyn Source>> = None;
    let disc_like = disc::EXTS.contains(&ext.as_str()) || head.starts_with(b"IsZ!") || disc::sniff_compressed(head).is_some();
    if disc_like {
        match disc::open(path, head) {
            Some(disc::Opened::Iso(s, info)) => {
                listing.facts.push(("Image".into(), info.layout.clone()));
                if let Some(d) = &info.data_file {
                    listing.facts.push(("Data file".into(), d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
                }
                listing.audio_tracks = info.audio_tracks;
                listing.is_disc = true;
                src = Some(s);
            }
            Some(disc::Opened::Unsupported(name)) => return Err(format!("{name} images use a proprietary compressed format that isn't supported yet")),
            None => {}
        }
    }
    let is_disc = listing.is_disc || ext == "iso";
    listing.is_disc = is_disc;
    let src: Box<dyn Source> = match src {
        Some(s) => s,
        None => Box::new(std::fs::File::open(path).map_err(|e| e.to_string())?),
    };
    let preferred: &[&str] = if is_disc {
        &["Udf", "Iso"]
    } else {
        match ext.as_str() {
            "deb" => &["Ar"],
            "rpm" => &["Rpm"],
            "pkg" | "xar" => &["Xar"],
            "dmg" => &["Dmg"],
            "cpio" => &["Cpio"],
            _ => &[],
        }
    };
    let candidates = if listing.is_disc && !preferred.is_empty() && ext != "iso" {
        preferred.iter().filter_map(|n| lib.format(n)).collect()
    } else {
        lib.candidates(&ext, head, preferred)
    };
    let first = match Archive::open(src, Some(path), &candidates, progress.clone(), cancel) {
        Ok(a) => a,
        Err(OpenError::Cancelled) => return Err("cancelled".into()),
        Err(e) => return Err(e.to_string()),
    };
    listing.kind = kind_name(&first.format, is_disc);
    listing.chain.push(first.format.clone());
    // Archive-level facts.
    let method = first.arc_prop(sevenzip::KPID_METHOD).str().map(|s| s.to_string()).unwrap_or_default();
    if !method.is_empty() && !is_disc {
        listing.facts.push(("Method".into(), method));
    }
    if first.arc_prop(sevenzip::KPID_SOLID).bool() {
        listing.facts.push(("Solid".into(), "yes".into()));
    }
    if let Some(n) = first.arc_prop(sevenzip::KPID_NUM_VOLUMES).num().filter(|&n| n > 1) {
        listing.facts.push(("Volumes".into(), n.to_string()));
    }
    if let Some(v) = first.arc_prop(sevenzip::KPID_VOLUME_NAME).str().filter(|s| !s.is_empty()) {
        listing.facts.push(("Volume label".into(), v.to_string()));
    }
    if let Some(c) = first.arc_prop(sevenzip::KPID_CHARACTS).str().filter(|s| !s.is_empty()) {
        listing.facts.push(("Characteristics".into(), c.to_string()));
    }
    if let Some(c) = time_fact(first.arc_prop(sevenzip::KPID_CTIME)) {
        listing.facts.push(("Created".into(), c));
    }
    if let Some(c) = first.arc_prop(sevenzip::KPID_COMMENT).str().filter(|s| !s.trim().is_empty() && !is_disc) {
        listing.notes.push(format!("Comment: {c}"));
    }
    if let Some(w) = first.arc_prop(sevenzip::KPID_WARNING).str().filter(|s| !s.is_empty()) {
        listing.notes.push(w.to_string());
    }
    let mut chain = vec![first];
    // Follow "main sub-files" (DMG → HFS, RPM → gzip, DEB → data.tar.xz).
    for _ in 0..4 {
        let inner = chain.last().unwrap();
        let Some(main) = inner.arc_prop(1).num() else { break };
        let name = inner.prop(main as u32, sevenzip::KPID_PATH).str().unwrap_or("").to_string();
        match inner.nested(main as u32, &name, cancel) {
            Some(a) => {
                listing.chain.push(a.format.clone());
                chain.push(a);
            }
            None => break,
        }
    }
    // Package metadata from the outermost container.
    match chain[0].format.as_str() {
        "Ar" => {
            if let Some((f, d)) = deb_info(&chain[0], cancel) {
                listing.package = f;
                listing.description = d;
            }
        }
        "Rpm" => {
            if let Ok(mut f) = std::fs::File::open(path) {
                let mut b = vec![0u8; 1 << 20];
                let n = f.read(&mut b).unwrap_or(0);
                b.truncate(n);
                if let Some((fields, d)) = crate::package::rpm_header(&b) {
                    listing.package = fields;
                    listing.description = d;
                }
            }
        }
        _ => {}
    }
    let inner_format = chain.last().unwrap().format.to_ascii_lowercase();
    let file = (!listing.is_disc).then(|| path.to_path_buf());
    let arc = Arc::new(Mutex::new(Opened { chain, file }));
    let single = arc.lock().chain.last().unwrap().count() == 1;
    // Compressed tar/cpio: stream the inner listing.
    if COMPRESSORS.contains(&inner_format.as_str()) && single {
        if let Some((kind, list)) = stream_listing(&arc, 0, cancel, &progress) {
            listing.chain.push(if kind == StreamKind::Tar { "tar".into() } else { "cpio".into() });
            if listing.kind.ends_with("compressed file") {
                listing.kind = format!("{} ({})", if kind == StreamKind::Tar { "Tar archive" } else { "cpio archive" }, listing.chain[listing.chain.len() - 2]);
            }
            listing.entries = list
                .into_iter()
                .enumerate()
                .map(|(i, e)| ListEntry { path: e.path, is_dir: e.is_dir, size: e.size, packed: None, mtime: Some(e.mtime).filter(|&m| m > 0), encrypted: false, id: i as u32 })
                .collect();
            listing.access = Some(Access::Stream { arc: arc.clone(), item: 0, kind });
        }
    }
    if listing.access.is_none() {
        {
            let opened = arc.lock();
            let inner = opened.chain.last().unwrap();
            let fallback_name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            for e in inner.entries(cancel) {
                let mut p = e.path.clone();
                if p.is_empty() {
                    p = fallback_name.clone();
                }
                listing.entries.push(ListEntry {
                    path: p,
                    is_dir: e.is_dir,
                    size: e.size,
                    packed: e.packed,
                    mtime: e.mtime.map(filetime_to_unix),
                    encrypted: e.encrypted,
                    id: e.index,
                });
            }
        }
        listing.access = Some(Access::Archive(arc));
    }
    if cancel.is_cancelled() {
        return Err("cancelled".into());
    }
    if listing.is_disc {
        let access = listing.access.clone().unwrap();
        listing.bd = bluray(&listing.entries, &access, cancel);
        // A plain .iso goes to bdinfo-rs for the codec detail (HDR, Dolby Vision...).
        if listing.bd.is_some() && ext == "iso" {
            match crate::bdinfo::summary(path, cancel) {
                Ok(d) => listing.bd = Some(d),
                Err(e) => tracing::warn!("bdinfo-rs: {e}"),
            }
        }
    }
    listing.elapsed_ms = t.elapsed().as_secs_f64() * 1000.0;
    Ok(listing)
}

/// A Blu-ray folder on disk (the folder holding BDMV, or BDMV itself).
pub fn open_bd_folder(dir: &Path, cancel: &CancelToken) -> Option<Listing> {
    let root = if dir.join("BDMV").join("index.bdmv").is_file() {
        dir.to_path_buf()
    } else if dir.file_name().is_some_and(|n| n.eq_ignore_ascii_case("BDMV")) && dir.join("index.bdmv").is_file() {
        dir.parent()?.to_path_buf()
    } else {
        return None;
    };
    let mut paths = Vec::new();
    let mut entries = Vec::new();
    fn walk(d: &Path, root: &Path, paths: &mut Vec<PathBuf>, entries: &mut Vec<ListEntry>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        let mut v: Vec<_> = rd.flatten().collect();
        v.sort_by_key(|e| e.file_name());
        for e in v {
            let p = e.path();
            let rel = p.strip_prefix(root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_default();
            let md = e.metadata().ok();
            let is_dir = p.is_dir();
            let mtime = md.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
            entries.push(ListEntry { path: rel, is_dir, size: md.map(|m| m.len()).unwrap_or(0), packed: None, mtime, encrypted: false, id: paths.len() as u32 });
            paths.push(p.clone());
            if is_dir && entries.len() < 100_000 {
                walk(&p, root, paths, entries);
            }
        }
    }
    walk(&root, &root, &mut paths, &mut entries);
    let access = Access::Files(Arc::new(paths));
    let bd = match crate::bdinfo::summary(&root, cancel) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("bdinfo-rs: {e}");
            bluray(&entries, &access, cancel)?
        }
    };
    Some(Listing { kind: "Blu-ray folder".into(), chain: vec!["BDMV".into()], entries, access: Some(access), bd: Some(bd), path: root, is_disc: true, ..Default::default() })
}

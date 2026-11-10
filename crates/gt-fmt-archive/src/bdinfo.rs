//! Blu-ray analysis with bdinfo-rs (a Rust reimplementation of BDInfo that reads
//! `.iso` images through its own UDF reader, and BDMV folders):
//! - `summary`: the disc panel, from the playlist/clip databases plus a quick
//!   codec pass over the main title's stream files only;
//! - `run`: the classic BDInfo report (full measured scan), cached per disc,
//!   with live progress and cancellation.

use crate::bdmv::{Disc, Playlist, Stream};
use bdinfo_rs_core::bdrom::disc::{BdRom, ScanMode, ScanObservers, ScanOptions, ScanProgress, ScanReport};
use bdinfo_rs_core::bdrom::order::{selection_stream_files, PlaylistFilter};
use bdinfo_rs_core::report::text::{render_with, RenderOptions};
use bdinfo_rs_core::scan;
use gt_core::CancelToken;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

fn open(disc: &Path, mode: ScanMode, files: Option<&BTreeSet<String>>, cancel: &CancelToken, on: &mut dyn FnMut(ScanProgress<'_>)) -> Result<ScanReport, String> {
    let flag = AtomicBool::new(cancel.is_cancelled());
    let mut cb = |p: ScanProgress<'_>| {
        if cancel.is_cancelled() {
            flag.store(true, Ordering::Relaxed);
        }
        on(p);
    };
    let observers = ScanObservers::new(&mut cb, &flag);
    let r = if disc.is_dir() { scan::open_folder(disc, mode, ScanOptions::default(), files, observers) } else { scan::open_iso(disc, mode, ScanOptions::default(), files, observers) };
    if cancel.is_cancelled() {
        return Err("cancelled".into());
    }
    r.map_err(|e| e.to_string())
}

/// BDInfo's playlist order: short and looping playlists dropped, unless that
/// leaves nothing (tiny test discs), then everything.
fn presented(bd: &BdRom) -> (Vec<usize>, PlaylistFilter) {
    let f = PlaylistFilter::default();
    let order = bd.presentation_order(&f);
    if !order.is_empty() {
        return (order, f);
    }
    let all = PlaylistFilter::everything();
    (bd.presentation_order(&all), all)
}

fn kind_of(s: &bdinfo_rs_core::bdrom::disc::StreamSummary) -> &'static str {
    if s.stream_type.is_video() {
        "Video"
    } else if s.stream_type.is_audio() {
        "Audio"
    } else if s.stream_type.is_graphics() {
        "Subtitles"
    } else {
        "Text"
    }
}

/// The disc panel: structure plus codec detail of the main title.
pub fn summary(disc: &Path, cancel: &CancelToken) -> Result<Disc, String> {
    let t = Instant::now();
    let meta = open(disc, ScanMode::Metadata, None, cancel, &mut |_| {})?;
    let (order, _) = presented(&meta.bdrom);
    let main_name = order.first().and_then(|&i| meta.bdrom.playlists.get(i)).map(|p| p.name.clone());
    // Quick codec pass over the main title's clips only: HDR, Dolby Vision, cores.
    let bd = match &main_name {
        Some(n) => {
            let files = selection_stream_files(&meta.bdrom.playlists, std::slice::from_ref(n));
            match open(disc, ScanMode::Codecs, Some(&files), cancel, &mut |_| {}) {
                Ok(r) => r.bdrom,
                Err(e) if e == "cancelled" => return Err(e),
                Err(e) => {
                    tracing::warn!("bdinfo-rs codec pass: {e}");
                    meta.bdrom
                }
            }
        }
        None => meta.bdrom,
    };
    let (order, _) = presented(&bd);
    let mut out = Disc { title: bd.disc_title.clone().unwrap_or_default(), uhd: bd.is_uhd, ..Default::default() };
    if !bd.volume_label.is_empty() {
        out.facts.push(("Volume label".into(), bd.volume_label.clone()));
    }
    let mut features = Vec::new();
    if bd.is_uhd {
        features.push("Ultra HD");
    }
    if bd.is_3d {
        features.push("3D");
    }
    if bd.is_bd_java {
        features.push("BD-Java");
    }
    if bd.is_50hz {
        features.push("50 Hz");
    }
    if bd.is_bd_plus {
        features.push("BD+");
    }
    if !features.is_empty() {
        out.facts.push(("Features".into(), features.join(", ")));
    }
    out.encrypted = bd.is_aacs_encrypted;
    // Presented playlists first (the main title leads), then the rest.
    let mut idx: Vec<usize> = order.clone();
    idx.extend((0..bd.playlists.len()).filter(|i| !order.contains(i)));
    for i in idx {
        let p = &bd.playlists[i];
        out.playlists.push(Playlist {
            name: p.name.clone(),
            seconds: p.total_length,
            clips: p.clips.iter().map(|c| c.name.clone()).collect(),
            chapters: p.chapter_count,
            streams: p
                .streams
                .iter()
                .map(|s| Stream { kind: kind_of(s), codec: if s.codec_name.is_empty() { s.codec_short_name.clone() } else { s.codec_name.clone() }, detail: s.description.clone(), lang: s.language_name.clone() })
                .collect(),
            presented: order.contains(&i),
        });
    }
    tracing::info!("bdinfo-rs summary in {:.1} ms ({} playlists)", t.elapsed().as_secs_f64() * 1000.0, out.playlists.len());
    Ok(out)
}

fn cache_path(disc: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    disc.hash(&mut h);
    if let Ok(m) = std::fs::metadata(disc) {
        m.len().hash(&mut h);
        m.modified().ok().hash(&mut h);
    }
    bdinfo_rs_core::version().hash(&mut h);
    let dir = gt_core::paths::cache_dir().join("bdinfo");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("{:016x}.txt", h.finish()))
}

/// The classic BDInfo report: measures every presented playlist's streams.
pub fn run(disc: &Path, cancel: &CancelToken, progress: &dyn Fn(String)) -> Result<String, String> {
    let cached = cache_path(disc);
    if let Ok(t) = std::fs::read_to_string(&cached) {
        if !t.trim().is_empty() {
            return Ok(t);
        }
    }
    let t = Instant::now();
    progress("Reading the disc structure…".into());
    let meta = open(disc, ScanMode::Metadata, None, cancel, &mut |_| {})?;
    if meta.bdrom.is_aacs_encrypted {
        return Err("This disc is still AACS-encrypted: only its structure can be read, so there is nothing to measure.".into());
    }
    let (order, _) = presented(&meta.bdrom);
    let names: Vec<String> = order.iter().filter_map(|&i| meta.bdrom.playlists.get(i)).map(|p| p.name.clone()).collect();
    let files = selection_stream_files(&meta.bdrom.playlists, &names);
    let mut last = Instant::now();
    let mut on = |p: ScanProgress<'_>| {
        if last.elapsed().as_millis() < 200 || p.total == 0 {
            return;
        }
        last = Instant::now();
        let pct = p.done as f64 * 100.0 / p.total as f64;
        let rate = p.done as f64 / t.elapsed().as_secs_f64().max(0.001);
        let eta = (p.total - p.done) as f64 / rate.max(1.0);
        progress(format!(
            "Scanning {} · {:.0}% ({} of {}) · about {} left",
            p.file,
            pct,
            gt_core::fmt::size(p.done),
            gt_core::fmt::size(p.total),
            gt_core::fmt::duration(eta)
        ));
    };
    let report = open(disc, ScanMode::Full, Some(&files), cancel, &mut on)?;
    let (order, _) = presented(&report.bdrom);
    let text = render_with(&report.bdrom, &order, &report.errors, RenderOptions::default());
    tracing::info!("bdinfo-rs report in {:.1} s ({} playlists)", t.elapsed().as_secs_f64(), order.len());
    let _ = std::fs::write(&cached, &text);
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uhd_sample_summary_and_report() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/archives/generated/bluray-uhd.iso");
        if !p.exists() {
            return;
        }
        let c = CancelToken::new();
        let d = summary(&p, &c).unwrap();
        assert!(d.uhd);
        let m = d.main().unwrap();
        assert_eq!(m.chapters, 3);
        assert!(m.streams.iter().any(|s| s.kind == "Video" && s.detail.contains("HDR10")), "{:?}", m.streams);
        assert!(m.streams.iter().any(|s| s.kind == "Audio" && s.lang == "English"));
        let cancelled = CancelToken::new();
        cancelled.cancel();
        assert!(summary(&p, &cancelled).is_err());
    }
}

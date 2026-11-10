//! Providers: archives and disc images (7-Zip + our adapters), and Blu-ray folders.

use crate::listing::{self, Listing};
use crate::sevenzip::Progress;
use crate::view::ArchiveView;
use crate::{disc, stream};
use gt_core::CancelToken;
use gt_ui::widgets::card::{CardData, InfoCard};
use gt_ui::{Loaded, Probe, Provider, View, ViewEnv};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

/// Extensions previewed as archives or disc images.
pub const EXTS: &[&str] = &[
    "zip", "7z", "rar", "tar", "gz", "gzip", "tgz", "bz2", "tbz2", "tbz", "xz", "txz", "zst", "tzst", "lzma", "z", "cpio", "cpgz", "cpbz2", "deb", "rpm", "xar", "pkg",
    "dmg", "iso", "img", "cab", "wim", "swm", "esd", "arj", "lzh", "lha", "jar", "apk", "xpi", "epub", "001", "vhd", "vhdx", "vmdk", "squashfs", "apfs", "hfs",
];

/// How long `load` waits before showing a progress view instead.
const QUICK: Duration = Duration::from_millis(150);

pub struct ArchiveProvider;

fn magic(head: &[u8]) -> bool {
    head.starts_with(b"PK\x03\x04")
        || head.starts_with(b"7z\xBC\xAF\x27\x1C")
        || head.starts_with(b"Rar!\x1A\x07")
        || head.starts_with(&[0x1F, 0x8B])
        || head.starts_with(b"BZh")
        || head.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0])
        || head.starts_with(&[0x28, 0xB5, 0x2F, 0xFD])
        || head.starts_with(b"MSCF")
        || head.starts_with(b"xar!")
        || head.starts_with(b"!<arch>")
        || head.starts_with(&[0xED, 0xAB, 0xEE, 0xDB])
        || head.starts_with(b"MSWIM")
        || head.starts_with(b"IsZ!")
        || stream::is_tar(head)
        || stream::is_cpio(head)
}

impl Provider for ArchiveProvider {
    fn id(&self) -> &'static str {
        "archive"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir {
            return 0;
        }
        let e = p.ext.as_str();
        // ".xar" is also Xara's format: only the real XAR magic.
        if e == "xar" {
            return if p.head.starts_with(b"xar!") { 85 } else { 0 };
        }
        // Office/OpenDocument files are ZIPs, but other previewers own them.
        if matches!(e, "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" | "idml" | "usdz" | "afdesign" | "aftemplate") {
            return 0;
        }
        if e == "iso" || disc::EXTS.contains(&e) {
            // Raw .img/.bin might be anything; still worth a try (low score).
            return if matches!(e, "img" | "bin") { 40 } else { 80 };
        }
        if EXTS.contains(&e) {
            return 75;
        }
        if magic(&p.head) {
            return 55;
        }
        0
    }

    fn load(&self, p: &Probe, cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let progress = Arc::new(Progress::default());
        let (tx, rx) = std::sync::mpsc::channel();
        let (path, head, c, pr) = (p.path.clone(), p.head.clone(), cancel.clone(), progress.clone());
        std::thread::Builder::new()
            .name("gt-archive-open".into())
            .spawn(move || {
                let r = listing::open(&path, &head, pr, &c);
                let _ = tx.send(r);
            })
            .map_err(|e| e.to_string())?;
        match rx.recv_timeout(QUICK) {
            Ok(Ok(l)) => {
                tracing::info!("listed {} entries in {:.1} ms ({})", l.entries.len(), l.elapsed_ms, l.chain.join(" › "));
                Ok(Box::new(ArchiveLoaded::Ready(Box::new(l))))
            }
            Ok(Err(e)) => card_or_err(p, e),
            Err(_) => Ok(Box::new(ArchiveLoaded::Pending { path: p.path.clone(), progress, rx: Some(rx) })),
        }
    }
}

fn card_or_err(p: &Probe, e: String) -> Result<Box<dyn Loaded>, String> {
    // Recognised but unreadable (proprietary compression, password-protected
    // names...): an info card says why; anything else falls to other providers.
    if e.contains("proprietary") || e.contains("password") {
        let icon = gt_shell::info::image(&p.path.display().to_string(), 256, false).map(|b| (b.width, b.height, b.data));
        return Ok(Box::new(CardLoaded(CardData {
            icon,
            name: p.name(),
            kind: gt_shell::info::type_name(&p.path.display().to_string()),
            facts: vec![("Size".into(), gt_core::fmt::size_long(p.size))],
            note: Some(format!("{}{}.", e[..1].to_uppercase(), &e[1..])),
        })));
    }
    Err(e)
}

struct CardLoaded(CardData);

impl Loaded for CardLoaded {
    fn into_view(self: Box<Self>, env: &ViewEnv) -> Box<dyn View> {
        Box::new(InfoCard::new(&env.gfx.dc, self.0))
    }
}

enum ArchiveLoaded {
    Ready(Box<Listing>),
    Pending { path: std::path::PathBuf, progress: Arc<Progress>, rx: Option<Receiver<Result<Listing, String>>> },
}

impl Loaded for ArchiveLoaded {
    fn take_upgrade(&mut self) -> Option<gt_ui::Upgrade> {
        let ArchiveLoaded::Pending { rx, .. } = self else { return None };
        let rx = rx.take()?;
        Some(Box::new(move |cancel: &CancelToken| loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(r) => return Some(Box::new(r) as Box<dyn std::any::Any + Send>),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if !cancel.is_cancelled() => continue,
                _ => return None,
            }
        }))
    }

    fn into_view(self: Box<Self>, _env: &ViewEnv) -> Box<dyn View> {
        match *self {
            ArchiveLoaded::Ready(l) => Box::new(ArchiveView::new(*l)),
            ArchiveLoaded::Pending { path, progress, .. } => Box::new(ArchiveView::pending(path, progress)),
        }
    }
}

/// A folder holding a Blu-ray structure (or the BDMV folder itself).
pub struct BlurayFolderProvider;

impl Provider for BlurayFolderProvider {
    fn id(&self) -> &'static str {
        "bluray-folder"
    }

    fn score(&self, p: &Probe) -> u32 {
        if !p.is_dir {
            return 0;
        }
        let bd = p.path.join("BDMV").join("index.bdmv").is_file() || (p.path.file_name().is_some_and(|n| n.eq_ignore_ascii_case("BDMV")) && p.path.join("index.bdmv").is_file());
        if bd {
            95
        } else {
            0
        }
    }

    fn load(&self, p: &Probe, cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let l = listing::open_bd_folder(&p.path, cancel).ok_or("not a Blu-ray folder")?;
        Ok(Box::new(ArchiveLoaded::Ready(Box::new(l))))
    }
}

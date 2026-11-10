//! Archives and disc images (M4): a bridge to 7-Zip's `7z.dll`, streaming tar/cpio
//! listing, adapters for raw and compressed disc images, Debian/RPM metadata,
//! Blu-ray summaries (with playback and BDInfo) and nested previews.

pub mod bdinfo;
pub mod bdmv;
pub mod disc;
pub mod isz;
pub mod listing;
pub mod package;
pub mod provider;
pub mod rar;
pub mod sevenzip;
pub mod stream;
pub mod view;

use gt_ui::Registry;

pub fn register(r: &mut Registry) {
    r.add(provider::ArchiveProvider);
    r.add(provider::BlurayFolderProvider);
}

/// Load 7z.dll ahead of the first preview and drop stale extracted files.
pub fn prewarm() {
    std::thread::Builder::new()
        .name("gt-archive-warm".into())
        .spawn(|| {
            sevenzip::preload();
            view::clear_temp();
        })
        .ok();
}

pub use view::clear_temp;

//! Torrent, NZB, NFO/DOS art, text fallback, and the generic file/folder info cards.

pub mod highlight;
pub mod bencode;
pub mod card;
pub mod nfo;
pub mod nzb;
pub mod text;
pub mod torrent;

use gt_ui::Registry;

/// Load the syntax collection in the background.
pub fn prewarm() {
    highlight::prewarm();
}

pub fn register(r: &mut Registry) {
    r.add(torrent::TorrentProvider);
    r.add(nzb::NzbProvider);
    r.add(nfo::NfoProvider);
    r.add(text::TextProvider);
    r.add(card::FolderProvider);
    r.add(card::CardProvider);
}

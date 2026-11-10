//! Audio and video previews (M3): an embedded mpv player sharing one warm core,
//! MediaInfo summaries and reports, MIDI through a bundled SoundFont, playlists,
//! CD track stubs, DRM-protected files and DVD `.ifo` files.

pub mod cover;
pub mod engine;
pub mod mediainfo;
pub mod midi;
pub mod mpv;
pub mod playlist;
pub mod probe;
pub mod provider;
pub mod special;
pub mod transport;
pub mod view;

use gt_ui::Registry;
use windows::Win32::Foundation::HWND;

pub const VIDEO_EXTS: &[&str] = &[
    "avi", "divx", "xvid", "vob", "evo", "flv", "f4v", "jts", "mkv", "mk3d", "ogm", "ogv", "mpeg", "mpg", "m1v", "mpv", "mpe", "m2v", "m2t", "tp", "ts", "m2ts", "mts",
    "trp", "mov", "qt", "mp4", "m4v", "rv", "rm", "rmvb", "tivo", "wmv", "asf", "dvr-ms", "wtv", "webm", "3gp", "3g2", "mxf", "y4m", "h264", "264", "h265", "265", "hevc",
    "avc", "vc1", "dv", "nut", "ivf", "obu", "ifo",
];

pub const AUDIO_EXTS: &[&str] = &[
    "m4a", "m4p", "m4b", "m4r", "aac", "adts", "ac3", "eac3", "ec3", "aif", "aifc", "aiff", "au", "snd", "aa", "aax", "dts", "dtshd", "mka", "ape", "mac", "mpc",
    "mp+", "mpp", "mp3", "mpa", "mpga", "mp2", "mp1", "ogg", "oga", "opus", "spx", "ra", "wav", "w64", "rf64", "raw", "wv", "wma", "flac", "alac", "tta", "tak", "caf",
    "amr", "awb", "mlp", "thd", "truehd", "shn", "ofr", "voc", "gsm", "mod", "xm", "s3m", "it",
];

pub const MIDI_EXTS: &[&str] = &["mid", "midi", "rmi", "rmid", "rim", "kar"];

/// Anything this crate previews (the MediaInfo button uses it too).
pub fn is_media_ext(ext: &str) -> bool {
    VIDEO_EXTS.contains(&ext) || AUDIO_EXTS.contains(&ext) || MIDI_EXTS.contains(&ext) || ext == "cda"
}

pub fn register(r: &mut Registry) {
    r.add(provider::MediaProvider);
    r.add(provider::MidiProvider);
    r.add(provider::PlaylistProvider);
    r.add(provider::CdaProvider);
}

/// Warm everything up at viewer start: the video host window, mpv (in the
/// background) and MediaInfo.dll. The SoundFont loads with the first MIDI file.
pub fn prewarm(parent: HWND, wake_msg: u32) {
    engine::prewarm(parent, wake_msg);
    std::thread::Builder::new()
        .name("gt-media-warm".into())
        .spawn(mediainfo::preload)
        .ok();
}

/// Close the video player while the previewer is idle.
pub fn release() {
    engine::release();
}

/// Drain engine events when no media view is listening.
pub fn drain() {
    engine::with_existing(|e| {
        e.pump();
    });
}

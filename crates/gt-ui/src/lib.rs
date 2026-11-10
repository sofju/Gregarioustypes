//! Direct2D/DirectWrite widget layer: drawing context, theme, the `View` and
//! `Provider` traits that format crates implement, the title bar, and reusable views.

pub mod chrome;
pub mod cx;
pub mod geom;
pub mod provider;
pub mod scroll;
pub mod theme;
pub mod view;
pub mod widgets;

pub use cx::{Align, DrawCx, EventCx, Font, Fonts};
pub use geom::Rect;
pub use provider::{Loaded, Probe, Provider, Registry, Upgrade, ViewEnv};
pub use theme::{rgba, Color, Theme};
pub use view::{Action, Event, GpuCx, HostCx, MouseButton, ReportJob, Request, View};

/// Segoe Fluent Icons code points used across the UI.
pub mod glyph {
    pub const CLOSE: char = '\u{E8BB}';
    pub const FULLSCREEN: char = '\u{E740}';
    pub const BACK_TO_WINDOW: char = '\u{E73F}';
    pub const OPEN_WITH: char = '\u{E7AC}';
    pub const OPEN_IN_NEW: char = '\u{E8A7}';
    pub const PIN: char = '\u{E718}';
    pub const UNPIN: char = '\u{E77A}';
    pub const INFO: char = '\u{E946}';
    pub const COPY: char = '\u{E8C8}';
    pub const PREV: char = '\u{E76B}';
    pub const NEXT: char = '\u{E76C}';
    pub const CHEVRON_DOWN: char = '\u{E70D}';
    pub const CHEVRON_RIGHT: char = '\u{E76C}';
    pub const FOLDER: char = '\u{E8B7}';
    pub const FILE: char = '\u{E8A5}';
    pub const ZOOM_IN: char = '\u{E8A3}';
    pub const ZOOM_OUT: char = '\u{E71F}';
    pub const FONT: char = '\u{E8D2}';
    pub const COLOR: char = '\u{E790}';
    pub const WRAP: char = '\u{E751}';
    pub const CODE: char = '\u{E943}';
    pub const LINK: char = '\u{E71B}';
    pub const WARNING: char = '\u{E7BA}';
    pub const ROTATE: char = '\u{E7AD}';
    pub const SHARE: char = '\u{E72D}';
    pub const DOC: char = '\u{E8A5}';
    pub const REPORT: char = '\u{E9F9}';
    pub const PLAY: char = '\u{E768}';
    pub const PAUSE: char = '\u{E769}';
    pub const VOLUME: char = '\u{E767}';
    pub const VOLUME_MUTE: char = '\u{E74F}';
    pub const SUBTITLES: char = '\u{ED1E}';
    pub const AUDIO_TRACK: char = '\u{E8D6}';
    pub const REPEAT: char = '\u{E8EE}';
    pub const SAVE: char = '\u{E74E}';
    pub const MUSIC: char = '\u{EC4F}';
    pub const VIDEO: char = '\u{E714}';
    pub const LOCK: char = '\u{E72E}';
    pub const DISC: char = '\u{E958}';
    pub const LIST: char = '\u{EA37}';
}

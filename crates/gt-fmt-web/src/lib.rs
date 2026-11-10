//! Web previews in one shared, pre-warmed WebView2: HTML pages (scripts and
//! network off), Markdown rendered locally, and Flash (SWF) through Ruffle.

pub mod engine;
pub mod markdown;
pub mod view;

use gt_ui::Registry;
use windows::Win32::Foundation::HWND;

pub fn register(r: &mut Registry) {
    r.add(view::HtmlProvider);
    r.add(view::MarkdownProvider);
    r.add(view::SwfProvider);
}

/// Note the host window; WebView2 itself starts when a web file is first probed.
pub fn prewarm(parent: HWND, wake_msg: u32) {
    engine::init(parent, wake_msg);
}

/// Give the page the keyboard (after a click into it).
pub fn focus() {
    engine::focus();
}

/// Close WebView2 while the previewer is idle.
pub fn release() {
    engine::release();
}

/// Start WebView2 now (settings: keep engines ready).
pub fn warm() {
    engine::request_warm();
}

/// UI thread, on the wake message.
pub fn pump() {
    engine::pump();
}

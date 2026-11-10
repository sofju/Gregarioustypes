use crate::cx::{DrawCx, EventCx};
use crate::geom::Rect;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView};

/// What a view hosting a native child window (video player, web view) needs to know.
#[derive(Clone, Copy, Debug)]
pub struct HostCx {
    /// The preview window; native children are created inside it.
    pub parent: HWND,
    /// Message the view's engine posts to `parent` when it has news (see `View::wake`).
    pub wake_msg: u32,
    /// Windows HDR is on for the monitor showing the window.
    pub hdr: bool,
    /// Display peak in nits (override applied).
    pub peak_nits: f32,
    pub sdr_white_nits: f32,
    pub autoplay: bool,
}

/// Direct3D access for views that render on the FP16 scRGB layer.
pub struct GpuCx<'a> {
    pub device: &'a ID3D11Device,
    pub ctx: &'a ID3D11DeviceContext,
    pub rtv: &'a ID3D11RenderTargetView,
    pub target_w: u32,
    pub target_h: u32,
    /// Windows HDR is on for this monitor.
    pub hdr: bool,
    pub sdr_white_nits: f32,
    /// Display peak in nits (override applied).
    pub peak_nits: f32,
    pub scale: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Debug)]
pub enum Event {
    MouseMove { x: f32, y: f32 },
    MouseDown { x: f32, y: f32, button: MouseButton, double: bool, shift: bool },
    MouseUp { x: f32, y: f32, button: MouseButton },
    /// `lines` > 0 scrolls up / left.
    Wheel { x: f32, y: f32, lines: f32, horizontal: bool, ctrl: bool },
    Key { vk: u16, ctrl: bool, shift: bool },
    MouseLeave,
}

/// Work a report job does off the UI thread; `progress` updates the sheet's status.
pub type ReportJob = Box<dyn FnOnce(&gt_core::CancelToken, &dyn Fn(String)) -> Result<String, String> + Send>;

/// Something a view asks the window to do (polled with `View::take_request`).
pub enum Request {
    /// Preview a file (extracted from an archive...) in place, with a way back.
    OpenNested { path: std::path::PathBuf, title: String },
    /// Show another view in place (a Blu-ray title playing...), with a way back.
    PushView { view: Box<dyn View>, title: String },
    /// Show a text report generated off the UI thread (BDInfo...).
    Report { title: &'static str, job: ReportJob },
    /// Show this item of the current selection (index sheet).
    ShowItem(usize),
}

/// Extra title-bar button contributed by a view.
#[derive(Clone, Debug)]
pub struct Action {
    pub id: &'static str,
    pub glyph: char,
    pub tip: String,
    /// Optional text shown next to the glyph (e.g. the current encoding).
    pub label: Option<String>,
    pub active: bool,
}

impl Action {
    pub fn new(id: &'static str, glyph: char, tip: impl Into<String>) -> Self {
        Self { id, glyph, tip: tip.into(), label: None, active: false }
    }
    pub fn label(mut self, l: impl Into<String>) -> Self {
        self.label = Some(l.into());
        self
    }
    pub fn active(mut self, a: bool) -> Self {
        self.active = a;
        self
    }
}

/// A preview. Lives on the UI thread; built from a `Loaded` produced off-thread.
pub trait View {
    /// Short key used to remember window sizes per kind of content.
    fn kind(&self) -> &'static str {
        "doc"
    }

    /// Preferred content size in physical pixels, given the largest available area.
    fn preferred_size(&mut self, fonts: &mut crate::cx::Fonts, scale: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        let _ = (fonts, scale);
        (max_w * 0.7, max_h * 0.75)
    }

    fn draw(&mut self, cx: &mut DrawCx, rect: Rect);

    /// Draw on the FP16 scRGB layer (called before `draw` when `uses_hdr_layer`).
    fn draw_hdr(&mut self, cx: &mut DrawCx, rect: Rect) {
        let _ = (cx, rect);
    }

    /// Direct3D rendering on the FP16 scRGB layer (before `draw_hdr`).
    fn render_gpu(&mut self, gpu: &mut GpuCx, rect: Rect) {
        let _ = (gpu, rect);
    }

    /// A refined version of the content arrived (full-resolution decode...).
    fn upgrade(&mut self, env: &crate::provider::ViewEnv, payload: Box<dyn std::any::Any + Send>) -> bool {
        let _ = (env, payload);
        false
    }

    /// Time until the next animation frame, if animating.
    fn next_frame_in(&self) -> Option<std::time::Duration> {
        None
    }

    /// Advance animation; returns true when a redraw is needed.
    fn tick(&mut self, env: &crate::provider::ViewEnv) -> bool {
        let _ = env;
        false
    }

    fn event(&mut self, cx: &mut EventCx, rect: Rect, ev: &Event) {
        let _ = (cx, rect, ev);
    }

    /// View-specific title-bar buttons.
    fn actions(&self) -> Vec<Action> {
        Vec::new()
    }

    fn on_action(&mut self, id: &str, cx: &mut EventCx) {
        let _ = (id, cx);
    }

    /// Text for Ctrl+C (selection, or the whole content).
    fn copy_text(&self) -> Option<String> {
        None
    }

    /// Key/value facts for the Info panel.
    fn info(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Short status shown after the title ("CP437 · 80 cols").
    fn subtitle(&self) -> Option<String> {
        None
    }

    /// Does this view need keyboard focus when clicked (text selection etc.)?
    fn wants_focus(&self) -> bool {
        false
    }

    /// Does the view draw on the HDR (FP16) layer?
    fn uses_hdr_layer(&self) -> bool {
        false
    }

    /// The view became the visible one (show native children, start playback).
    fn attach(&mut self, host: &HostCx) {
        let _ = host;
    }

    /// The view stopped being visible (cached for later or about to be dropped).
    fn detach(&mut self) {}

    /// Position native children over `rect` (client pixels); called before each frame.
    fn layout_native(&mut self, rect: Rect, host: &HostCx) {
        let _ = (rect, host);
    }

    /// The engine posted `HostCx::wake_msg`; returns true when a redraw is needed.
    fn wake(&mut self) -> bool {
        false
    }

    /// A native child window draws the content area, so the UI layer must leave it clear.
    fn native_content(&self) -> bool {
        false
    }

    /// A pending request for the window (checked after events, actions and wake-ups).
    fn take_request(&mut self) -> Option<Request> {
        None
    }
}

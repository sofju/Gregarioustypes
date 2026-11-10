//! The preview window. A borderless, rounded, acrylic-backed popup that is shown
//! without taking focus (like Quick Look's panel), with two composition layers:
//! FP16 scRGB content below, BGRA8 chrome/UI on top.

mod index;
mod loader;
mod report;
mod views;

use crossbeam_channel::{Receiver, Sender};
use gt_core::settings::Settings;
use gt_core::CancelToken;
use gt_ipc::{Item, RectI, ToAgent, ToViewer};
use gt_render::{display, DisplayInfo, Gfx, Surface};
use gt_ui::chrome::{self, Chrome, ChromeState, Hit};
use gt_ui::cx::{Cursor, Fonts};
use gt_ui::widgets::bitmap_from_bgra;
use gt_ui::{glyph, rgba, Action, Align, DrawCx, Event, EventCx, Font, GpuCx, HostCx, MouseButton, Rect, Registry, Request, Theme, View, ViewEnv};
use loader::{LoadMsg, LoadResult, Prefetched};
use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::Instant;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Direct2D::{ID2D1SolidColorBrush, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE};
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWMWCP_ROUND, DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, GetMonitorInfoW, InvalidateRect, MonitorFromPoint, MonitorFromWindow, ScreenToClient, ValidateRect, DISPLAY_DEVICEW, HMONITOR,
    MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_C, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_F11, VK_I,
    VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;

pub enum Mode {
    Agent(String),
    Standalone(Vec<String>),
    HdrTest,
}

const WM_APP_IPC: u32 = WM_APP + 1;
const WM_APP_LOADED: u32 = WM_APP + 2;
const WM_APP_IPC_CLOSED: u32 = WM_APP + 3;
/// The media engine has events queued.
const WM_APP_WAKE: u32 = WM_APP + 4;
/// Show the save dialog for the report (outside the `App` borrow).
const WM_APP_SAVE: u32 = WM_APP + 5;
const TIMER_SHOW: usize = 1;
const TIMER_TOAST: usize = 2;
const TIMER_TIP: usize = 3;
const TIMER_ANIM: usize = 4;
/// Refresh the report sheet's progress while a job runs.
const TIMER_REPORT: usize = 5;
/// Fires after the previewer has been hidden for the configured idle time.
const TIMER_IDLE: usize = 6;
const INFO_W: f32 = 320.0;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static PENDING_SIZE: Cell<bool> = const { Cell::new(false) };
    static PENDING_PAINT: Cell<bool> = const { Cell::new(false) };
}

/// A preview we navigated away from inside the window (archive → entry...).
struct Frame {
    items: Vec<Item>,
    index: usize,
    view: Option<Box<dyn View>>,
    view_path: String,
    shown_path: String,
    /// Breadcrumb label for this level.
    title: String,
}

/// Work that must run after the `App` borrow is released (modal loops re-enter the
/// window procedure).
enum After {
    None,
    Drag(String),
    OpenWith(String),
    /// (suggested file name, text)
    SaveText(String, String),
}

struct App {
    hwnd: HWND,
    gfx: Gfx,
    surface: Surface,
    fonts: Fonts,
    theme: Theme,
    brush: ID2D1SolidColorBrush,
    settings: Arc<Settings>,
    registry: Arc<Registry>,
    tx: Option<gt_ipc::Sender>,
    standalone: bool,
    ipc_rx: Receiver<ToViewer>,
    load_tx: Sender<LoadMsg>,
    load_rx: Receiver<LoadMsg>,
    pool: loader::Pool,
    /// Neighbours decoded ahead of time (most recent last).
    prefetched: Vec<Prefetched>,
    prefetching: Vec<String>,
    prefetch_cancel: CancelToken,
    /// Recently shown views (path, view), so stepping back is instant.
    recent: Vec<(String, Box<dyn View>)>,
    /// Path of the view currently in `view` (empty for built-in views).
    shown_path: String,
    items: Vec<Item>,
    index: usize,
    view: Option<Box<dyn View>>,
    view_path: String,
    info_facts: Vec<(String, String)>,
    generation: u64,
    cancel: CancelToken,
    forced: Option<&'static str>,
    load_started: Instant,
    chrome: Chrome,
    open_with: Option<String>,
    visible: bool,
    pending_show: bool,
    fullscreen: bool,
    pinned: bool,
    /// The index sheet of a multi-selection is shown.
    index_sheet: bool,
    /// Subtitle and view buttons last shown (refreshed when a wake changes them).
    chrome_sig: String,
    info_open: bool,
    saved_rect: RECT,
    user_sized: bool,
    sizing: bool,
    monitor_work: RECT,
    monitor_full: RECT,
    scale: f32,
    display: DisplayInfo,
    capture_view: bool,
    tracking: bool,
    cursor: Cursor,
    chrome_down: Option<usize>,
    icon_down: Option<(f32, f32)>,
    toast: Option<String>,
    tip: bool,
    mouse: (f32, f32),
    /// MediaInfo (later BDInfo) report sheet over the content.
    report: Option<report::Report>,
    report_seq: u64,
    pending_save: Option<(String, String)>,
    /// Peak-brightness override from settings (re-read when the display is queried).
    peak_cache: Cell<Option<Option<f32>>>,
    /// Nested previews (entries opened from archives, Blu-ray titles...).
    nested: Vec<Frame>,
}

fn lo(v: isize) -> i32 {
    (v & 0xFFFF) as i16 as i32
}
fn hi(v: isize) -> i32 {
    ((v >> 16) & 0xFFFF) as i16 as i32
}

fn key_down(vk: u16) -> bool {
    unsafe { (GetKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

fn monitor_rects(mon: HMONITOR) -> (RECT, RECT) {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let _ = GetMonitorInfoW(mon, &mut mi);
    }
    (mi.rcWork, mi.rcMonitor)
}

fn monitor_dpi(mon: HMONITOR) -> f32 {
    let (mut x, mut y) = (96u32, 96u32);
    unsafe {
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut x, &mut y);
    }
    x as f32 / 96.0
}

/// Monitor hardware id (e.g. `MONITOR\SDC4229\...`) for the GDI device of `mon`.
fn monitor_id(mon: HMONITOR) -> String {
    unsafe {
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(mon, &mut mi.monitorInfo).as_bool() {
            return String::new();
        }
        let mut dd = DISPLAY_DEVICEW { cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        if !EnumDisplayDevicesW(PCWSTR(mi.szDevice.as_ptr()), 0, &mut dd, 0).as_bool() {
            return String::new();
        }
        let n = dd.DeviceID.iter().position(|&c| c == 0).unwrap_or(dd.DeviceID.len());
        String::from_utf16_lossy(&dd.DeviceID[..n])
    }
}

pub fn run(mode: Mode) {
    gt_core::log::init("viewer");
    let t0 = Instant::now();
    unsafe {
        let _ = OleInitialize(None);
    }
    let gfx = match Gfx::new() {
        Ok(g) => g,
        Err(e) => {
            tracing::error!("graphics init failed: {e}");
            return;
        }
    };
    let (standalone, ipc) = match &mode {
        Mode::Agent(tag) => match gt_ipc::connect(tag) {
            Ok(c) => (false, Some(c)),
            Err(e) => {
                tracing::error!("cannot connect to agent: {e}");
                return;
            }
        },
        Mode::Standalone(_) | Mode::HdrTest => (true, None),
    };
    let mut registry = Registry::default();
    gt_fmt_image::register(&mut registry);
    gt_fmt_misc::register(&mut registry);
    gt_fmt_media::register(&mut registry);
    gt_fmt_archive::register(&mut registry);
    gt_fmt_doc::register(&mut registry);
    gt_fmt_web::register(&mut registry);
    gt_fmt_design::register(&mut registry);
    gt_fmt_3d::register(&mut registry);
    let hwnd = create_window();
    let surface = Surface::new(&gfx, hwnd).expect("composition surface");
    let brush = unsafe { gfx.dc.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }, None) }.expect("brush");
    unsafe { gfx.dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE) };
    let fonts = Fonts::new(gfx.dwrite.clone());
    let (ipc_tx_chan, ipc_rx) = crossbeam_channel::unbounded();
    let (load_tx, load_rx) = crossbeam_channel::unbounded();
    let (rx_pipe, tx_pipe) = match ipc {
        Some((r, t)) => (Some(r), Some(t)),
        None => (None, None),
    };
    if let Some(rx) = rx_pipe {
        let h = hwnd.0 as isize;
        std::thread::Builder::new()
            .name("gt-ipc".into())
            .spawn(move || loop {
                match rx.recv::<ToViewer>() {
                    Ok(m) => {
                        let _ = ipc_tx_chan.send(m);
                        unsafe {
                            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_IPC, WPARAM(0), LPARAM(0));
                        }
                    }
                    Err(_) => {
                        unsafe {
                            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_IPC_CLOSED, WPARAM(0), LPARAM(0));
                        }
                        break;
                    }
                }
            })
            .expect("ipc thread");
    }
    let scale = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
    let app = App {
        hwnd,
        gfx,
        surface,
        fonts,
        theme: Theme::default(),
        brush,
        settings: Arc::new(Settings::load()),
        registry: Arc::new(registry),
        tx: tx_pipe,
        standalone,
        ipc_rx,
        load_tx,
        load_rx,
        pool: loader::Pool::new(3),
        prefetched: Vec::new(),
        prefetching: Vec::new(),
        prefetch_cancel: CancelToken::new(),
        recent: Vec::new(),
        shown_path: String::new(),
        items: Vec::new(),
        index: 0,
        view: None,
        view_path: String::new(),
        info_facts: Vec::new(),
        generation: 0,
        cancel: CancelToken::new(),
        forced: None,
        load_started: Instant::now(),
        chrome: Chrome::default(),
        open_with: None,
        visible: false,
        pending_show: false,
        fullscreen: false,
        pinned: false,
        index_sheet: false,
        chrome_sig: String::new(),
        info_open: false,
        saved_rect: RECT::default(),
        user_sized: false,
        sizing: false,
        monitor_work: RECT::default(),
        monitor_full: RECT::default(),
        scale: scale.max(1.0),
        display: DisplayInfo::default(),
        capture_view: false,
        tracking: false,
        cursor: Cursor::Arrow,
        chrome_down: None,
        icon_down: None,
        toast: None,
        tip: false,
        mouse: (0.0, 0.0),
        report: None,
        report_seq: 0,
        pending_save: None,
        peak_cache: Cell::new(None),
        nested: Vec::new(),
    };
    app.ipc(ToAgent::Ready { pid: std::process::id() });
    APP.with(|a| *a.borrow_mut() = Some(app));
    with_app(|a| {
        a.prewarm();
        a.apply_settings();
        // Conversions beyond the budget, from earlier sessions.
        let budget = a.settings.cache.budget_mb.saturating_mul(1 << 20);
        std::thread::spawn(move || gt_core::cache::trim(budget));
    });
    tracing::info!("viewer ready in {:?}", t0.elapsed());
    if let Mode::HdrTest = mode {
        with_app(|a| a.show_hdr_test());
    }
    if let Mode::Standalone(paths) = mode {
        let items = paths.into_iter().map(|p| Item { path: p, filesystem: true }).collect();
        with_app(|a| a.on_ipc(ToViewer::Show { items, index: 0, fullscreen: false, anchor: None, monitor: None, crashed: None }));
    }
    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // Nothing left to save: skip DLL teardown, where libmpv's worker threads
    // (already killed by ExitProcess) can leave the process hanging forever.
    tracing::info!("viewer exiting");
    unsafe {
        let _ = windows::Win32::System::Threading::TerminateProcess(windows::Win32::System::Threading::GetCurrentProcess(), 0);
    }
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut o| o.as_mut().map(f)))
}

fn create_window() -> HWND {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("Gregarioustypes.Viewer");
        let icon = LoadIconW(Some(hinst.into()), PCWSTR(1 as _)).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hIcon: icon,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class,
            w!("Gregarioustypes"),
            WS_POPUP | WS_THICKFRAME | WS_CAPTION | WS_SYSMENU,
            0,
            0,
            800,
            600,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .expect("viewer window");
        let dark: i32 = 1;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_ROUND as *const _ as _, 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_TRANSIENTWINDOW as *const _ as _, 4);
        let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
        hwnd
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCALCSIZE && wp.0 != 0 {
        return LRESULT(0); // the client area covers the whole window
    }
    let mut after = After::None;
    let r = APP.with(|cell| match cell.try_borrow_mut() {
        Ok(mut opt) => match opt.as_mut() {
            Some(app) => {
                let r = app.handle(msg, wp, lp, &mut after);
                app.flush_pending();
                r
            }
            None => None,
        },
        Err(_) => {
            // Re-entered from a nested message loop: defer the work.
            if msg == WM_SIZE {
                PENDING_SIZE.set(true);
            }
            if msg == WM_PAINT {
                PENDING_PAINT.set(true);
                unsafe {
                    let _ = ValidateRect(Some(hwnd), None);
                }
                return Some(LRESULT(0));
            }
            None
        }
    });
    match after {
        After::None => {}
        After::Drag(p) => {
            gt_shell::info::drag_file(&p);
        }
        After::OpenWith(p) => gt_shell::info::open_with(hwnd, &p),
        After::SaveText(name, text) => save_text(hwnd, &name, &text),
    }
    r.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wp, lp) })
}

impl App {
    fn ipc(&self, m: ToAgent) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(&m);
        }
    }

    fn report_visible(&self) {
        self.ipc(ToAgent::Visible { visible: self.visible, multi: self.items.len() > 1, pinned: self.pinned, hwnd: self.hwnd.0 as isize });
    }

    /// Render one invisible frame so D2D/DirectWrite/DComp first-use costs are paid
    /// at start-up instead of on the first Space press.
    fn prewarm(&mut self) {
        let t = Instant::now();
        // Show once far off-screen so DWM sets up composition and the backdrop now.
        unsafe {
            // Off-screen, z-order untouched: HWND_BOTTOM would strip the topmost flag.
            let _ = SetWindowPos(self.hwnd, None, -20000, -20000, 1600, 1000, SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW);
        }
        if self.surface.ui.ensure(&self.gfx, 1600, 1000).is_ok() && self.surface.ui.begin(&self.gfx).is_ok() {
            let mut cx = DrawCx { dc: &self.gfx.dc, brush: &self.brush, fonts: &mut self.fonts, theme: &self.theme, scale: self.scale, hdr_layer: false, sdr_scale: 1.0, headroom: 1.0 };
            unsafe {
                let _ = cx.dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            }
            let r = Rect::new(0.0, 0.0, 1600.0, 40.0);
            let sample = format!("Gregarioustypes 0123456789 éà {}", gt_ui::glyph::CLOSE);
            for f in [Font::Ui(12.5), Font::Semibold(13.0), Font::Bold(20.0), Font::Mono(13.0), Font::Icon(15.0)] {
                cx.text(&sample, r, f, cx.theme.text, Align::Left);
            }
            let _ = self.surface.ui.end(&self.gfx);
        }
        self.surface.commit(&self.gfx);
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.surface.ui.release(&self.gfx);
        self.surface.commit(&self.gfx);
        gt_fmt_media::prewarm(self.hwnd, WM_APP_WAKE);
        gt_fmt_archive::prewarm();
        gt_fmt_doc::prewarm();
        gt_fmt_misc::prewarm();
        gt_fmt_web::prewarm(self.hwnd, WM_APP_WAKE);
        tracing::info!("prewarm {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    }

    fn flush_pending(&mut self) {
        if PENDING_SIZE.take() {
            self.render();
        }
        if PENDING_PAINT.take() {
            self.render();
        }
    }

    fn client_size(&self) -> (u32, u32) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        ((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32)
    }

    fn chrome_h(&self) -> f32 {
        chrome::height(self.scale)
    }

    fn chrome_visible(&self) -> bool {
        !self.fullscreen || self.mouse.1 < self.chrome_h() * 1.5
    }

    fn content_rect(&self) -> Rect {
        let (w, h) = self.client_size();
        let top = if self.fullscreen { 0.0 } else { self.chrome_h() };
        let mut r = Rect::new(0.0, top, w as f32, h as f32 - top);
        if self.info_open {
            r.w = (r.w - (INFO_W * self.scale).round()).max(0.0);
        }
        r
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    // ------------------------------------------------------------ messages

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM, after: &mut After) -> Option<LRESULT> {
        match msg {
            WM_NCHITTEST => Some(LRESULT(self.hit_test(lo(lp.0), hi(lp.0)) as isize)),
            // Clicks on our own surfaces never activate the panel, like Quick Look.
            // (Clicks inside a web page are handled in WebView2's process; see
            // `set_activatable`.)
            WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
            WM_SETCURSOR => {
                if (lp.0 & 0xFFFF) as u32 == HTCLIENT {
                    let id = match self.cursor {
                        Cursor::Arrow => IDC_ARROW,
                        Cursor::Hand => IDC_HAND,
                        Cursor::IBeam => IDC_IBEAM,
                    };
                    unsafe {
                        SetCursor(LoadCursorW(None, id).ok());
                    }
                    return Some(LRESULT(1));
                }
                None
            }
            WM_PAINT => {
                self.render();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd), None);
                }
                Some(LRESULT(0))
            }
            WM_SIZE => {
                self.render();
                Some(LRESULT(0))
            }
            WM_SIZING => {
                self.sizing = true;
                None
            }
            WM_EXITSIZEMOVE => {
                if self.sizing {
                    self.user_sized = true;
                    self.sizing = false;
                }
                None
            }
            WM_GETMINMAXINFO => {
                let mmi = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
                mmi.ptMinTrackSize = POINT { x: (360.0 * self.scale) as i32, y: (200.0 * self.scale) as i32 };
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                self.scale = (hi(wp.0 as isize) as f32 / 96.0).max(1.0);
                let r = unsafe { &*(lp.0 as *const RECT) };
                unsafe {
                    let _ = SetWindowPos(self.hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                }
                self.update_chrome();
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
                self.refresh_display();
                self.invalidate();
                None
            }
            WM_MOVE => {
                self.refresh_display();
                None
            }
            WM_CLOSE => {
                tracing::info!("hide: WM_CLOSE");
                self.hide();
                Some(LRESULT(0))
            }
            WM_SYSCOMMAND => {
                if (wp.0 & 0xFFF0) as u32 == SC_KEYMENU {
                    self.toggle_fullscreen();
                    return Some(LRESULT(0));
                }
                None
            }
            WM_TIMER => {
                match wp.0 {
                    TIMER_SHOW => {
                        self.kill_timer(TIMER_SHOW);
                        if self.pending_show {
                            let name = self.items.get(self.index).map(|i| file_name(&i.path)).unwrap_or_default();
                            self.set_view(Box::new(views::LoadingView { name: name.clone() }), &name, true);
                            self.show_window();
                        }
                    }
                    TIMER_IDLE => {
                        self.kill_timer(TIMER_IDLE);
                        if !self.visible {
                            self.release_engines();
                        }
                    }
                    TIMER_TOAST => {
                        self.kill_timer(TIMER_TOAST);
                        self.toast = None;
                        self.invalidate();
                    }
                    TIMER_ANIM => {
                        self.kill_timer(TIMER_ANIM);
                        let env = ViewEnv { gfx: &self.gfx, settings: &self.settings, scale: self.scale };
                        if self.visible && self.view.as_mut().map(|v| v.tick(&env)).unwrap_or(false) {
                            self.render();
                        } else {
                            self.schedule_animation();
                        }
                        self.handle_requests();
                    }
                    TIMER_REPORT => {
                        if self.report.as_ref().is_some_and(|r| r.is_loading()) {
                            self.invalidate();
                        } else {
                            self.kill_timer(TIMER_REPORT);
                        }
                    }
                    TIMER_TIP => {
                        self.kill_timer(TIMER_TIP);
                        if self.chrome.hovered().is_some() {
                            self.tip = true;
                            self.invalidate();
                        }
                    }
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_APP_IPC => {
                while let Ok(m) = self.ipc_rx.try_recv() {
                    self.on_ipc(m);
                }
                Some(LRESULT(0))
            }
            WM_APP_WAKE => {
                let redraw = self.view.as_mut().is_some_and(|v| v.wake());
                gt_fmt_media::drain();
                gt_fmt_web::pump();
                // News from the view's engine may change its subtitle or buttons.
                if redraw && self.chrome_sig != self.view_chrome_sig() {
                    self.update_chrome();
                }
                if redraw && self.visible {
                    self.render();
                }
                self.handle_requests();
                Some(LRESULT(0))
            }
            WM_APP_SAVE => {
                if let Some((name, text)) = self.pending_save.take() {
                    *after = After::SaveText(name, text);
                }
                Some(LRESULT(0))
            }
            WM_APP_IPC_CLOSED => {
                tracing::info!("agent pipe closed; exiting");
                unsafe { PostQuitMessage(0) };
                Some(LRESULT(0))
            }
            WM_APP_LOADED => {
                while let Ok(m) = self.load_rx.try_recv() {
                    match m {
                        LoadMsg::Done(r) => self.on_loaded(r),
                        LoadMsg::Extras(x) if x.generation == self.generation => {
                            self.chrome.icon = x.icon.and_then(|b| bitmap_from_bgra(&self.gfx.dc, b.width, b.height, &b.data));
                            self.open_with = x.open_with;
                            self.update_chrome();
                            self.invalidate();
                        }
                        LoadMsg::Extras(_) => {}
                        LoadMsg::Upgrade { generation, payload } if generation == self.generation => {
                            let env = ViewEnv { gfx: &self.gfx, settings: &self.settings, scale: self.scale };
                            if let Some(v) = self.view.as_mut() {
                                if v.upgrade(&env, payload) {
                                    self.info_facts.clear();
                                    self.update_chrome();
                                    self.invalidate();
                                }
                            }
                        }
                        LoadMsg::Upgrade { .. } => {}
                        LoadMsg::Report { id, result } => {
                            if let Some(r) = self.report.as_mut().filter(|r| r.id == id) {
                                r.set_result(result);
                                self.invalidate();
                            }
                        }
                        LoadMsg::Prefetched(p) => {
                            self.prefetching.retain(|x| *x != p.path);
                            tracing::info!("prefetched {} via {} in {:.1} ms", p.path, p.provider, p.elapsed_ms);
                            self.prefetched.retain(|x| x.path != p.path);
                            self.prefetched.push(p);
                            while self.prefetched.len() > 4 {
                                self.prefetched.remove(0);
                            }
                        }
                    }
                }
                Some(LRESULT(0))
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                self.on_key(wp.0 as u16, msg == WM_SYSKEYDOWN);
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                self.on_mouse_move(lo(lp.0) as f32, hi(lp.0) as f32, after);
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                let mut redraw = self.chrome.set_hover(None);
                redraw |= self.tip;
                self.tip = false;
                self.dispatch(&Event::MouseLeave);
                if redraw {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                self.on_button_down(lo(lp.0) as f32, hi(lp.0) as f32, msg == WM_LBUTTONDBLCLK);
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                self.on_button_up(lo(lp.0) as f32, hi(lp.0) as f32, after);
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                let mut pt = POINT { x: lo(lp.0), y: hi(lp.0) };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut pt);
                }
                let delta = hi(wp.0 as isize) as f32 / 120.0;
                let horizontal = msg == WM_MOUSEHWHEEL;
                let ctrl = (wp.0 & 0x0008) != 0;
                let ev = Event::Wheel { x: pt.x as f32, y: pt.y as f32, lines: if horizontal { -delta } else { delta }, horizontal, ctrl };
                self.dispatch(&ev);
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    fn kill_timer(&self, id: usize) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), id);
        }
    }

    fn hit_test(&self, sx: i32, sy: i32) -> u32 {
        let mut wr = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut wr);
        }
        let (x, y) = (sx - wr.left, sy - wr.top);
        let (w, h) = (wr.right - wr.left, wr.bottom - wr.top);
        if !self.fullscreen {
            let b = (6.0 * self.scale) as i32;
            let (l, r, t, btm) = (x < b, x >= w - b, y < b / 2 + 1, y >= h - b);
            match (l, r, t, btm) {
                (true, _, true, _) => return HTTOPLEFT,
                (_, true, true, _) => return HTTOPRIGHT,
                (true, _, _, true) => return HTBOTTOMLEFT,
                (_, true, _, true) => return HTBOTTOMRIGHT,
                (true, _, _, _) => return HTLEFT,
                (_, true, _, _) => return HTRIGHT,
                (_, _, true, _) => return HTTOP,
                (_, _, _, true) => return HTBOTTOM,
                _ => {}
            }
        }
        if self.chrome_visible() && (y as f32) < self.chrome_h() {
            if let Hit::Caption = self.chrome.hit(x as f32, y as f32, self.scale) {
                return HTCAPTION;
            }
        }
        HTCLIENT
    }

    // ------------------------------------------------------------ IPC

    fn on_ipc(&mut self, m: ToViewer) {
        match m {
            ToViewer::Show { items, index, fullscreen, monitor, crashed, .. } => {
                if items.is_empty() {
                    return;
                }
                let was_visible = self.visible;
                self.leave_nested();
                self.items = items;
                self.index = index.min(self.items.len() - 1);
                if !was_visible {
                    self.pick_monitor(monitor);
                    self.fullscreen = false;
                    self.user_sized = false;
                    self.info_open = false;
                    if fullscreen {
                        self.fullscreen = true;
                    }
                }
                if let Some(path) = crashed {
                    let l = loader::error_card(&path, "Preview failed", "The previewer crashed while opening this file and was restarted.");
                    let r = LoadResult { generation: self.generation + 1, path: path.clone(), provider: "crash", result: Ok(l), elapsed_ms: 0.0 };
                    self.generation += 1;
                    self.pending_show = !self.visible;
                    self.on_loaded(r);
                    return;
                }
                self.start_load();
            }
            ToViewer::Update { items, index, .. } => {
                if !self.visible && !self.pending_show {
                    return;
                }
                let same = self.items.get(self.index).map(|i| &i.path) == items.get(index).map(|i| &i.path);
                if self.nested.is_empty() || !same {
                    self.leave_nested();
                } else {
                    // Same selection while browsing inside an archive: stay there.
                    return;
                }
                self.items = items;
                self.index = index.min(self.items.len().saturating_sub(1));
                if !same {
                    self.start_load();
                } else {
                    self.update_chrome();
                    self.report_visible();
                    self.invalidate();
                }
            }
            ToViewer::Step(d) => self.step(d),
            ToViewer::Prefetch(items) => self.prefetch(items),
            ToViewer::Hide => {
                tracing::info!("hide requested by the agent");
                self.hide();
            }
            ToViewer::ToggleFullscreen => self.toggle_fullscreen(),
            ToViewer::Command(c) if c == "hdr-test" => self.show_hdr_test(),
            ToViewer::Command(c) if c == "reload-settings" => {
                self.settings = Arc::new(Settings::load());
                self.apply_settings();
                self.peak_cache.set(None);
                // Previews decoded with the old settings (engines per type, text size...).
                self.prefetched.clear();
                self.recent.clear();
                self.invalidate();
            }
            ToViewer::Command(_) => {}
            ToViewer::Quit => unsafe { PostQuitMessage(0) },
        }
    }

    fn pick_monitor(&mut self, monitor: Option<RectI>) {
        let mon = unsafe {
            match monitor {
                Some(r) => MonitorFromPoint(POINT { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 }, MONITOR_DEFAULTTONEAREST),
                None => {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST)
                }
            }
        };
        let (work, full) = monitor_rects(mon);
        self.monitor_work = work;
        self.monitor_full = full;
        self.scale = monitor_dpi(mon).max(1.0);
        self.display = self.query_display(mon);
    }

    fn query_display(&self, mon: HMONITOR) -> DisplayInfo {
        self.peak_cache.set(None);
        let info = display::query(mon);
        let id = monitor_id(mon);
        tracing::info!("display {} ({id}): hdr={} sdr_white={:.0} nits, DXGI peak {:.0} / full-frame {:.0} nits", info.device, info.hdr, info.sdr_white_nits, info.max_nits, info.max_full_frame_nits);
        info
    }

    fn refresh_display(&mut self) {
        if !self.visible {
            return;
        }
        let mon = unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST) };
        let d = self.query_display(mon);
        if d.hdr != self.display.hdr || d.sdr_white_nits != self.display.sdr_white_nits || d.device != self.display.device {
            tracing::info!("display changed: {} hdr={} sdr_white={:.0} peak={:.0}", d.device, d.hdr, d.sdr_white_nits, d.max_nits);
            self.display = d;
            self.invalidate();
        }
    }

    fn peak_override(&self) -> Option<f32> {
        if let Some(p) = self.peak_cache.get() {
            return p;
        }
        let p = Settings::load().display.peak_nits_override.or(self.settings.display.peak_nits_override);
        self.peak_cache.set(Some(p));
        p
    }

    /// What views hosting a native child (the video player) need to know.
    fn host_cx(&self) -> HostCx {
        let peak = self.peak_override();
        let d = &self.display;
        HostCx {
            parent: self.hwnd,
            wake_msg: WM_APP_WAKE,
            hdr: d.hdr,
            peak_nits: if d.hdr { peak.unwrap_or(d.max_nits).max(d.sdr_white_nits) } else { 80.0 },
            sdr_white_nits: if d.hdr { d.sdr_white_nits } else { 80.0 },
            autoplay: self.settings.autoplay,
        }
    }

    // ------------------------------------------------------------ loading

    fn prefetch(&mut self, items: Vec<Item>) {
        if !self.visible && !self.pending_show {
            return;
        }
        self.prefetch_cancel.cancel();
        self.prefetch_cancel = CancelToken::new();
        let wanted: Vec<String> = items.iter().map(|i| i.path.clone()).collect();
        self.prefetched.retain(|p| wanted.contains(&p.path) || self.items.iter().any(|i| i.path == p.path));
        for item in items {
            let current = self.items.get(self.index).map(|i| i.path == item.path).unwrap_or(false);
            if current || self.recent.iter().any(|(p, _)| *p == item.path) || self.prefetched.iter().any(|p| p.path == item.path) || self.prefetching.contains(&item.path) {
                continue;
            }
            self.prefetching.push(item.path.clone());
            self.pool.prefetch(
                self.registry.clone(),
                item,
                self.settings.clone(),
                self.load_tx.clone(),
                self.hwnd.0 as isize,
                WM_APP_LOADED,
                self.screen_size(),
                self.prefetch_cancel.clone(),
            );
        }
    }

    fn start_load(&mut self) {
        let Some(item) = self.items.get(self.index).cloned() else { return };
        self.index_sheet = false;
        // A view shown moments ago: reuse it as is.
        if self.forced.is_none() {
            if let Some(i) = self.recent.iter().position(|(p, _)| *p == item.path) {
                let (path, view) = self.recent.remove(i);
                self.cancel.cancel();
                self.cancel = CancelToken::new();
                self.generation += 1;
                self.load_started = Instant::now();
                self.ipc(ToAgent::Loading { path: path.clone() });
                let first = !self.visible;
                if first {
                    self.pending_show = true;
                }
                self.chrome.icon = None;
                self.open_with = None;
                self.info_facts.clear();
                let name = file_name(&path);
                self.view_path = path.clone();
                self.set_view(view, &name, first || !self.user_sized);
                if self.pending_show {
                    self.show_window();
                } else {
                    self.invalidate();
                }
                tracing::info!("{path} from recent views");
                let (tx, hwnd) = (self.load_tx.clone(), self.hwnd.0 as isize);
                self.pool.extras(self.generation, path, item.filesystem, tx, hwnd, WM_APP_LOADED);
                return;
            }
        }
        // A neighbour decoded ahead of time: show it immediately.
        if self.forced.is_none() {
            if let Some(i) = self.prefetched.iter().position(|p| p.path == item.path) {
                let p = self.prefetched.remove(i);
                self.cancel.cancel();
                self.cancel = CancelToken::new();
                self.generation += 1;
                self.load_started = Instant::now();
                if !self.visible {
                    self.pending_show = true;
                }
                self.ipc(ToAgent::Loading { path: item.path.clone() });
                let generation = self.generation;
                tracing::info!("{} from prefetch cache", p.path);
                self.on_loaded(LoadResult { generation, path: p.path.clone(), provider: p.provider, result: Ok(p.loaded), elapsed_ms: 0.0 });
                if let Some(payload) = p.upgrade {
                    let env = ViewEnv { gfx: &self.gfx, settings: &self.settings, scale: self.scale };
                    if let Some(v) = self.view.as_mut() {
                        v.upgrade(&env, payload);
                    }
                }
                // Title-bar icon and default app still come from the pool.
                let (tx, hwnd, path) = (self.load_tx.clone(), self.hwnd.0 as isize, item.path.clone());
                self.pool.extras(generation, path, item.filesystem, tx, hwnd, WM_APP_LOADED);
                return;
            }
        }
        self.cancel.cancel();
        self.cancel = CancelToken::new();
        self.generation += 1;
        self.load_started = Instant::now();
        if !self.visible {
            self.pending_show = true;
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_SHOW, 80, None);
            }
        }
        self.ipc(ToAgent::Loading { path: item.path.clone() });
        self.pool.load(
            self.registry.clone(),
            item,
            self.settings.clone(),
            self.cancel.clone(),
            self.forced.take(),
            self.generation,
            self.load_tx.clone(),
            self.hwnd.0 as isize,
            WM_APP_LOADED,
            self.screen_size(),
        );
    }

    /// Physical size of the target monitor (decoders use it to pick a reduced resolution).
    fn screen_size(&self) -> (u32, u32) {
        let r = self.monitor_full;
        let (w, h) = ((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32);
        if w == 0 {
            (3840, 2160)
        } else {
            (w, h)
        }
    }

    fn on_loaded(&mut self, r: LoadResult) {
        if r.generation != self.generation {
            return; // superseded by a newer selection
        }
        tracing::info!("{} via {} in {:.1} ms (request → result {:.1} ms)", r.path, r.provider, r.elapsed_ms, self.load_started.elapsed().as_secs_f64() * 1000.0);
        let loaded = match r.result {
            Ok(l) => l,
            Err(e) if e == "cancelled" => return,
            Err(e) => loader::error_card(&r.path, "No preview available", &e),
        };
        let env = ViewEnv { gfx: &self.gfx, settings: &self.settings, scale: self.scale };
        let view = loaded.into_view(&env);
        if self.view_path != r.path {
            self.chrome.icon = None;
            self.open_with = None;
        }
        self.info_facts.clear(); // computed when the Info panel opens
        let name = file_name(&r.path);
        self.view_path = r.path;
        let first = self.pending_show;
        self.set_view(view, &name, first || !self.user_sized);
        if self.pending_show {
            self.show_window();
        } else {
            self.invalidate();
        }
    }

    /// Make sure the panel really is topmost. Windows can demote it (another app
    /// taking the foreground while it is hidden...) and then silently ignores
    /// further HWND_TOPMOST requests until the window is made non-topmost first:
    /// the preview would open hidden behind Explorer.
    fn ensure_topmost(&self) {
        unsafe {
            if GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) & WS_EX_TOPMOST.0 as isize != 0 {
                return;
            }
            let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
            let _ = SetWindowPos(self.hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, flags);
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
            let ok = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) & WS_EX_TOPMOST.0 as isize != 0;
            tracing::warn!("preview was not topmost; restored: {ok}");
        }
    }

    /// Toggle WS_EX_NOACTIVATE (off only while the user works inside a web page).
    fn set_activatable(&self, on: bool) {
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            let flag = WS_EX_NOACTIVATE.0 as isize;
            let want = if on { ex & !flag } else { ex | flag };
            if want != ex {
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, want);
            }
        }
    }

    fn set_view(&mut self, view: Box<dyn View>, title: &str, relayout: bool) {
        self.set_activatable(false);
        if let Some(mut old) = self.view.take() {
            old.detach();
            let old_path = std::mem::take(&mut self.shown_path);
            if !old_path.is_empty() && old_path != self.view_path && old.kind() != "loading" {
                self.recent.retain(|(p, _)| *p != old_path);
                self.recent.push((old_path, old));
                while self.recent.len() > 2 {
                    self.recent.remove(0);
                }
            }
        }
        let real = view.kind() != "loading";
        if real {
            self.shown_path = self.view_path.clone();
        }
        self.view = Some(view);
        let host = self.host_cx();
        if let Some(v) = self.view.as_mut() {
            v.attach(&host);
        }
        // A web page gets the keyboard when clicked. The click is handled in
        // WebView2's own process, so WS_EX_NOACTIVATE has to be off for it to
        // activate the panel (showing still never takes focus).
        let web = self.view.as_ref().is_some_and(|v| matches!(v.kind(), "html" | "markdown" | "swf" | "3d"));
        self.set_activatable(web);
        let title = if self.nested.is_empty() { title.to_string() } else { self.nested.iter().map(|f| f.title.as_str()).chain(std::iter::once(title)).collect::<Vec<_>>().join("  ›  ") };
        let title = title.as_str();
        if real && self.report.as_ref().is_some_and(|r| r.path != self.view_path) {
            if self.mediainfo_supported() {
                let full = self.report.as_ref().is_some_and(|r| r.full);
                self.open_report(full);
            } else {
                self.report = None;
            }
        }
        self.chrome.title = title.to_string();
        self.cursor = Cursor::Arrow;
        self.capture_view = false;
        self.update_chrome();
        if relayout && (self.visible || self.pending_show) {
            self.place();
        }
    }

    fn view_chrome_sig(&self) -> String {
        let Some(v) = self.view.as_ref() else { return String::new() };
        let acts: Vec<(&str, bool, Option<String>)> = v.actions().into_iter().map(|a| (a.id, a.active, a.label)).collect();
        format!("{:?}|{acts:?}", v.subtitle())
    }

    fn update_chrome(&mut self) {
        self.chrome_sig = self.view_chrome_sig();
        let actions = self.view.as_ref().map(|v| v.actions()).unwrap_or_default();
        self.chrome.subtitle = self.view.as_ref().and_then(|v| v.subtitle());
        let st = ChromeState {
            multi: (self.items.len() > 1).then_some((self.index, self.items.len())),
            pinned: self.pinned,
            fullscreen: self.fullscreen,
            open_with: self.open_with.clone(),
            info_open: self.info_open,
            has_file: !self.view_path.is_empty(),
            extra: self.app_actions(),
        };
        self.chrome.set_buttons(&actions, &st);
        let (w, _) = self.client_size();
        self.chrome.layout(&mut self.fonts, self.scale, w as f32);
    }

    fn mediainfo_supported(&self) -> bool {
        if self.view_path.is_empty() {
            return false;
        }
        let ext = gt_core::sniff::ext(std::path::Path::new(&self.view_path));
        gt_fmt_media::mediainfo::supports(&ext) && gt_fmt_media::mediainfo::available()
    }

    /// Window-level title-bar buttons.
    fn app_actions(&self) -> Vec<Action> {
        let mut v = Vec::new();
        if let Some(f) = self.nested.last() {
            v.push(Action::new("back", '\u{E72B}', format!("Back to {} (Backspace)", f.title)));
        }
        if self.items.len() > 1 && self.nested.is_empty() {
            v.push(Action::new("index", '\u{E8A9}', "Index sheet (Ctrl+Enter)").active(self.index_sheet));
        }
        // Views with their own MediaInfo (Blu-ray discs: the main title) replace ours.
        let own = self.view.as_ref().is_some_and(|v| v.actions().iter().any(|a| a.id == "mediainfo"));
        if self.mediainfo_supported() && !own {
            v.push(Action::new("mediainfo", glyph::REPORT, "MediaInfo report (Ctrl+M)").label("MediaInfo").active(self.report.is_some()));
        }
        v
    }

    /// Act on what the view asked for (nested preview, pushed view, report).
    fn handle_requests(&mut self) {
        for _ in 0..4 {
            let Some(req) = self.view.as_mut().and_then(|v| v.take_request()) else { return };
            match req {
                Request::OpenNested { path, title } => {
                    tracing::info!("nested preview of {}", path.display());
                    // Browsing inside a container keeps the window where and how big it is.
                    self.user_sized = true;
                    self.push_frame(&title);
                    self.items = vec![Item { path: path.display().to_string(), filesystem: true }];
                    self.index = 0;
                    self.start_load();
                }
                Request::PushView { view, title } => {
                    self.user_sized = true;
                    self.push_frame(&title);
                    self.items.clear();
                    self.index = 0;
                    self.view_path.clear();
                    self.chrome.icon = None;
                    self.open_with = None;
                    self.info_facts.clear();
                    self.set_view(view, &title, !self.user_sized);
                    self.invalidate();
                }
                Request::Report { title, job } => self.open_job_report(title, job),
                Request::ShowItem(i) => {
                    if i < self.items.len() {
                        self.index = i;
                        self.start_load();
                    }
                }
            }
        }
    }

    /// Remember the current preview so `go_back` can return to it.
    fn push_frame(&mut self, _next_title: &str) {
        let mut view = self.view.take();
        if let Some(v) = view.as_mut() {
            v.detach();
        }
        let title = if self.view_path.is_empty() { self.chrome.title.rsplit("  ›  ").next().unwrap_or("").to_string() } else { file_name(&self.view_path) };
        self.cancel.cancel();
        self.nested.push(Frame { items: std::mem::take(&mut self.items), index: self.index, view, view_path: std::mem::take(&mut self.view_path), shown_path: std::mem::take(&mut self.shown_path), title });
        self.report = None;
    }

    fn go_back(&mut self) {
        let Some(f) = self.nested.pop() else { return };
        self.cancel.cancel();
        self.generation += 1;
        if let Some(mut v) = self.view.take() {
            v.detach();
        }
        self.items = f.items;
        self.index = f.index;
        self.view_path = f.view_path;
        self.shown_path = String::new();
        self.report = None;
        self.chrome.icon = None;
        self.open_with = None;
        self.info_facts.clear();
        match f.view {
            Some(v) => {
                let name = if self.view_path.is_empty() { f.title.clone() } else { file_name(&self.view_path) };
                self.set_view(v, &name, !self.user_sized);
                self.shown_path = f.shown_path;
                if !self.view_path.is_empty() {
                    let (tx, hwnd) = (self.load_tx.clone(), self.hwnd.0 as isize);
                    let filesystem = self.items.get(self.index).map(|i| i.filesystem).unwrap_or(true);
                    self.pool.extras(self.generation, self.view_path.clone(), filesystem, tx, hwnd, WM_APP_LOADED);
                }
            }
            None => self.start_load(),
        }
        self.update_chrome();
        self.invalidate();
    }

    /// Drop every nested level (the selection in Explorer changed, or hiding).
    fn leave_nested(&mut self) {
        for mut f in self.nested.drain(..) {
            if let Some(v) = f.view.as_mut() {
                v.detach();
            }
        }
    }

    /// A report produced by a view's job (BDInfo...), with live progress.
    fn open_job_report(&mut self, title: &'static str, job: gt_ui::ReportJob) {
        self.report_seq += 1;
        let id = self.report_seq;
        let mut r = report::Report::new(title, self.view_path.clone(), false, id);
        r.switchable = false;
        let (progress, cancel) = (r.progress.clone(), r.cancel.clone());
        self.report = Some(r);
        let (tx, hwnd) = (self.load_tx.clone(), self.hwnd.0 as isize);
        self.pool.run(move || {
            let p2 = progress.clone();
            let result = job(&cancel, &move |s: String| *p2.lock() = s);
            if cancel.is_cancelled() {
                return;
            }
            let _ = tx.send(LoadMsg::Report { id, result });
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_LOADED, WPARAM(0), LPARAM(0));
            }
        });
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_REPORT, 300, None);
        }
        self.update_chrome();
        self.invalidate();
    }

    fn toggle_report(&mut self) {
        if self.report.take().is_some() {
            self.update_chrome();
            self.invalidate();
            return;
        }
        if self.mediainfo_supported() {
            self.open_report(false);
        }
    }

    /// (Re)generate the MediaInfo report for the current file off the UI thread.
    fn open_report(&mut self, full: bool) {
        self.report_seq += 1;
        let id = self.report_seq;
        let path = self.view_path.clone();
        match self.report.as_mut() {
            Some(r) => {
                r.path = path.clone();
                r.full = full;
                r.loading(id);
            }
            None => self.report = Some(report::Report::new("MediaInfo", path.clone(), full, id)),
        }
        let (tx, hwnd) = (self.load_tx.clone(), self.hwnd.0 as isize);
        self.pool.run(move || {
            let t = Instant::now();
            let result = gt_fmt_media::mediainfo::report(std::path::Path::new(&path), full);
            tracing::info!("MediaInfo report ({}) in {:.1} ms", if full { "full" } else { "summary" }, t.elapsed().as_secs_f64() * 1000.0);
            let _ = tx.send(LoadMsg::Report { id, result });
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_LOADED, WPARAM(0), LPARAM(0));
            }
        });
        self.update_chrome();
        self.invalidate();
    }

    /// Size and position the window for the current view.
    fn place(&mut self) {
        let (work, full) = (self.monitor_work, self.monitor_full);
        if work.right <= work.left {
            return;
        }
        let rect = if self.fullscreen {
            full
        } else {
            let s = self.scale;
            let ch = self.chrome_h();
            let (mw, mh) = ((work.right - work.left) as f32 * 0.88, (work.bottom - work.top) as f32 * 0.88);
            let info = if self.info_open { (INFO_W * s).round() } else { 0.0 };
            let (pw, ph) = match self.view.as_mut() {
                Some(v) => v.preferred_size(&mut self.fonts, s, mw - info, mh - ch),
                None => (640.0 * s, 400.0 * s),
            };
            let w = (pw + info).clamp(460.0 * s, mw).round() as i32;
            let h = (ph + ch).clamp(240.0 * s, mh).round() as i32;
            let (cx, cy) = if self.visible && !self.pending_show {
                let mut r = RECT::default();
                unsafe {
                    let _ = GetWindowRect(self.hwnd, &mut r);
                }
                ((r.left + r.right) / 2, (r.top + r.bottom) / 2)
            } else {
                ((work.left + work.right) / 2, (work.top + work.bottom) / 2)
            };
            let x = (cx - w / 2).clamp(work.left, (work.right - w).max(work.left));
            let y = (cy - h / 2).clamp(work.top, (work.bottom - h).max(work.top));
            RECT { left: x, top: y, right: x + w, bottom: y + h }
        };
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top, SWP_NOACTIVATE);
        }
        if self.visible {
            self.ensure_topmost();
        }
        self.update_chrome();
    }

    /// Hidden for a while: close the heavy engines and trim the disk cache.
    fn release_engines(&mut self) {
        let t = Instant::now();
        if !self.settings.engines.keep_warm {
            gt_fmt_web::release();
            gt_fmt_media::release();
        }
        let budget = self.settings.cache.budget_mb.saturating_mul(1 << 20);
        std::thread::spawn(move || gt_core::cache::trim(budget));
        unsafe {
            // Give the idle process's memory back to the system.
            let _ = windows::Win32::System::ProcessStatus::EmptyWorkingSet(windows::Win32::System::Threading::GetCurrentProcess());
        }
        tracing::info!("idle: engines released in {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
    }

    fn apply_settings(&mut self) {
        let idle = self.settings.engines.idle_minutes;
        gt_core::settings::set_idle_minutes(idle);
        if self.settings.engines.keep_warm {
            gt_fmt_web::warm();
        }
        // Not shown since start-up (or the delay changed): count from now.
        self.kill_timer(TIMER_IDLE);
        if idle > 0 && !self.visible {
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_IDLE, idle.saturating_mul(60_000), None);
            }
        }
    }

    fn show_window(&mut self) {
        self.kill_timer(TIMER_SHOW);
        self.kill_timer(TIMER_IDLE);
        self.pending_show = false;
        self.visible = true;
        self.place();
        let corner: DWM_WINDOW_CORNER_PREFERENCE = if self.fullscreen { DWMWCP_DONOTROUND } else { DWMWCP_ROUND };
        unsafe {
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as _, 4);
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
        self.ensure_topmost();
        self.render();
        self.report_visible();
        tracing::info!("shown {:.1} ms after request", self.load_started.elapsed().as_secs_f64() * 1000.0);
    }

    fn hide(&mut self) {
        self.set_activatable(false);
        self.cancel.cancel();
        self.prefetch_cancel.cancel();
        self.prefetched.clear();
        self.prefetching.clear();
        self.recent.clear();
        self.shown_path.clear();
        self.kill_timer(TIMER_SHOW);
        self.pending_show = false;
        if self.visible {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
        self.visible = false;
        if let Some(mut v) = self.view.take() {
            v.detach();
        }
        self.report = None;
        let had_nested = !self.nested.is_empty();
        self.leave_nested();
        if had_nested {
            std::thread::spawn(gt_fmt_archive::clear_temp);
        }
        self.view_path.clear();
        self.toast = None;
        self.tip = false;
        if self.fullscreen {
            self.fullscreen = false;
        }
        self.surface.content.release(&self.gfx);
        self.surface.ui.release(&self.gfx);
        self.surface.commit(&self.gfx);
        self.report_visible();
        let idle = self.settings.engines.idle_minutes;
        if idle > 0 {
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_IDLE, idle.saturating_mul(60_000), None);
            }
        }
        if self.standalone {
            unsafe { PostQuitMessage(0) };
        }
    }

    /// Show every selected item as a thumbnail grid, or go back to the current one.
    fn toggle_index(&mut self) {
        if self.index_sheet {
            self.start_load();
            return;
        }
        if self.items.len() < 2 {
            return;
        }
        // Drop a load in flight; the current item's view waits in `recent`.
        self.cancel.cancel();
        self.cancel = CancelToken::new();
        self.generation += 1;
        let paths = self.items.iter().map(|i| i.path.clone()).collect();
        self.view_path.clear();
        self.chrome.icon = None;
        self.open_with = None;
        self.info_facts.clear();
        self.set_view(Box::new(index::IndexView::new(paths, self.index)), "Index sheet", !self.user_sized);
        self.index_sheet = true;
        self.update_chrome();
        self.invalidate();
    }

    fn step(&mut self, d: i32) {
        if self.items.len() < 2 {
            return;
        }
        let n = self.items.len() as i32;
        self.index = ((self.index as i32 + d).rem_euclid(n)) as usize;
        self.start_load();
    }

    fn navigate(&mut self, d: i32) {
        if self.items.len() > 1 {
            self.step(d);
        } else {
            self.ipc(ToAgent::Navigate(d));
        }
    }

    fn toggle_fullscreen(&mut self) {
        if !self.visible {
            return;
        }
        if self.fullscreen {
            self.fullscreen = false;
            let r = self.saved_rect;
            unsafe {
                let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_ROUND as *const _ as _, 4);
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOACTIVATE);
            }
        } else {
            unsafe {
                let _ = GetWindowRect(self.hwnd, &mut self.saved_rect);
                let mon = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
                let (_, full) = monitor_rects(mon);
                self.monitor_full = full;
                self.fullscreen = true;
                let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_DONOTROUND as *const _ as _, 4);
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), full.left, full.top, full.right - full.left, full.bottom - full.top, SWP_NOACTIVATE);
            }
        }
        self.update_chrome();
        self.invalidate();
    }

    fn show_hdr_test(&mut self) {
        if !self.visible {
            self.items = vec![];
            self.pick_monitor(None);
            self.user_sized = false;
        } else {
            let mon = unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST) };
            self.display = self.query_display(mon);
        }
        self.view_path.clear();
        self.open_with = None;
        self.chrome.icon = None;
        self.info_facts.clear();
        let v = views::HdrTest { info: self.display.clone(), peak_override: self.peak_override() };
        self.pending_show = !self.visible;
        self.set_view(Box::new(v), "HDR test pattern", true);
        if self.pending_show {
            self.show_window();
        }
    }

    // ------------------------------------------------------------ input

    /// Send an event to the view and apply the flags it sets.
    fn dispatch(&mut self, ev: &Event) {
        let rect = self.content_rect();
        if let Some(mut rep) = self.report.take() {
            let inside = match *ev {
                Event::MouseMove { x, y } | Event::MouseDown { x, y, .. } | Event::Wheel { x, y, .. } => rect.contains(x, y) || self.capture_view,
                _ => true,
            };
            if inside {
                let mut cx = EventCx::new(self.hwnd, self.scale, &mut self.fonts);
                let click = rep.event(&mut cx, rect, ev);
                let fx = Effects::from(&cx);
                let full = rep.full;
                let name = format!("{} - {}.txt", file_name(&rep.path), rep.title);
                let text = rep.copy_text();
                self.report = Some(rep);
                self.apply(fx);
                match click {
                    report::Click::None => {}
                    report::Click::Mode(f) => {
                        if f != full {
                            self.open_report(f);
                        }
                    }
                    report::Click::Copy => {
                        if let Some(t) = self.report.as_ref().and_then(|r| r.text.clone()) {
                            gt_shell::info::set_clipboard_text(self.hwnd, &t);
                            self.show_toast("Report copied".into());
                        }
                    }
                    report::Click::Save => {
                        if let Some(t) = self.report.as_ref().and_then(|r| r.text.clone()).or(text) {
                            self.pending_save = Some((name, t));
                            unsafe {
                                let _ = PostMessageW(Some(self.hwnd), WM_APP_SAVE, WPARAM(0), LPARAM(0));
                            }
                        }
                    }
                    report::Click::Close => {
                        self.report = None;
                        self.update_chrome();
                        self.invalidate();
                    }
                }
                return;
            }
            self.report = Some(rep);
        }
        let Some(mut view) = self.view.take() else { return };
        let mut cx = EventCx::new(self.hwnd, self.scale, &mut self.fonts);
        view.event(&mut cx, rect, ev);
        let fx = Effects::from(&cx);
        self.view = Some(view);
        self.apply(fx);
        self.handle_requests();
    }

    fn apply(&mut self, fx: Effects) {
        if let Some(c) = fx.capture {
            self.capture_view = c;
            unsafe {
                if c {
                    SetCapture(self.hwnd);
                } else {
                    let _ = ReleaseCapture();
                }
            }
        }
        self.cursor = fx.cursor;
        if let Some(text) = fx.copy {
            gt_shell::info::set_clipboard_text(self.hwnd, &text);
        }
        if let Some(t) = fx.toast {
            self.show_toast(t);
        }
        if fx.fullscreen {
            self.toggle_fullscreen();
        }
        if let Some(p) = fx.switch_provider {
            self.forced = Some(p);
            self.start_load();
            return;
        }
        if fx.relayout {
            self.update_chrome();
            if !self.user_sized && !self.fullscreen {
                self.place();
            }
        }
        if fx.redraw || fx.relayout {
            self.update_chrome();
            self.invalidate();
        }
    }

    fn show_toast(&mut self, t: String) {
        self.toast = Some(t);
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_TOAST, 1600, None);
        }
        self.invalidate();
    }

    fn on_key(&mut self, vk: u16, sys: bool) {
        let ctrl = key_down(VK_CONTROL.0);
        let shift = key_down(VK_SHIFT.0);
        match vk {
            v if v == VK_SPACE.0 && sys => self.toggle_fullscreen(),
            v if v == VK_ESCAPE.0 && self.report.is_some() => {
                self.report = None;
                self.update_chrome();
                self.invalidate();
            }
            v if (v == VK_ESCAPE.0 || v == 0x08) && !self.nested.is_empty() => self.go_back(),
            v if v == VK_SPACE.0 || v == VK_ESCAPE.0 => {
                tracing::info!("hide: key {v:#x} in the preview");
                self.hide();
            }
            0x4D if ctrl => self.toggle_report(),
            v if v == VK_RETURN.0 && ctrl => self.toggle_index(),
            v if v == VK_RETURN.0 => {
                if !self.view_path.is_empty() {
                    gt_shell::info::open_default(&self.view_path);
                    self.hide();
                }
            }
            v if v == VK_F11.0 => self.toggle_fullscreen(),
            v if (v == VK_LEFT.0 || v == VK_UP.0) && !ctrl => self.navigate(-1),
            v if (v == VK_RIGHT.0 || v == VK_DOWN.0) && !ctrl => self.navigate(1),
            v if v == VK_C.0 && ctrl && self.report.is_some() => {
                if let Some(t) = self.report.as_ref().and_then(|r| r.copy_text()) {
                    gt_shell::info::set_clipboard_text(self.hwnd, &t);
                    self.show_toast("Copied".into());
                }
            }
            v if v == VK_C.0 && ctrl => {
                if let Some(t) = self.view.as_ref().and_then(|v| v.copy_text()) {
                    gt_shell::info::set_clipboard_text(self.hwnd, &t);
                    self.show_toast("Copied".into());
                }
            }
            v if v == VK_I.0 && ctrl => self.toggle_info(),
            _ => self.dispatch(&Event::Key { vk, ctrl, shift }),
        }
    }

    fn toggle_info(&mut self) {
        self.info_open = !self.info_open;
        if self.info_open && self.info_facts.is_empty() && !self.view_path.is_empty() {
            self.info_facts = file_facts(&self.view_path);
        }
        if !self.fullscreen && !self.user_sized {
            self.place();
        }
        self.update_chrome();
        self.invalidate();
    }

    fn track(&mut self) {
        if !self.tracking {
            let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
            unsafe {
                let _ = TrackMouseEvent(&mut t);
            }
            self.tracking = true;
        }
    }

    fn on_mouse_move(&mut self, x: f32, y: f32, after: &mut After) {
        self.track();
        let was_chrome = self.chrome_visible();
        self.mouse = (x, y);
        if was_chrome != self.chrome_visible() {
            self.invalidate();
        }
        if let Some((dx, dy)) = self.icon_down {
            if ((x - dx).abs() > 4.0 || (y - dy).abs() > 4.0) && !self.view_path.is_empty() {
                self.icon_down = None;
                unsafe {
                    let _ = ReleaseCapture();
                }
                *after = After::Drag(self.view_path.clone());
            }
            return;
        }
        if self.capture_view {
            self.dispatch(&Event::MouseMove { x, y });
            return;
        }
        let in_chrome = self.chrome_visible() && y < self.chrome_h();
        let hover = if in_chrome {
            match self.chrome.hit(x, y, self.scale) {
                Hit::Button(i) => Some(i),
                _ => None,
            }
        } else {
            None
        };
        if self.chrome.set_hover(hover) {
            self.tip = false;
            if hover.is_some() {
                unsafe {
                    SetTimer(Some(self.hwnd), TIMER_TIP, 650, None);
                }
            }
            self.invalidate();
        }
        if in_chrome {
            self.cursor = Cursor::Arrow;
        } else {
            self.dispatch(&Event::MouseMove { x, y });
        }
    }

    fn on_button_down(&mut self, x: f32, y: f32, double: bool) {
        tracing::debug!("button down at {x},{y} (chrome {}, h {})", self.chrome_visible(), self.chrome_h());
        if self.chrome_visible() && y < self.chrome_h() {
            match self.chrome.hit(x, y, self.scale) {
                Hit::Button(i) => {
                    self.chrome_down = Some(i);
                    self.chrome.set_pressed(Some(i));
                    self.invalidate();
                }
                Hit::Icon => {
                    self.icon_down = Some((x, y));
                    unsafe {
                        SetCapture(self.hwnd);
                    }
                }
                _ => {}
            }
            return;
        }
        // Clicking content that needs the keyboard takes focus (text selection, copy).
        if self.view.as_ref().is_some_and(|v| v.wants_focus()) || (self.report.is_some() && self.content_rect().contains(x, y)) {
            unsafe {
                let _ = SetForegroundWindow(self.hwnd);
                let _ = SetFocus(Some(self.hwnd));
            }
        }
        let shift = key_down(VK_SHIFT.0);
        self.dispatch(&Event::MouseDown { x, y, button: MouseButton::Left, double, shift });
    }

    fn on_button_up(&mut self, x: f32, y: f32, after: &mut After) {
        if self.icon_down.take().is_some() {
            unsafe {
                let _ = ReleaseCapture();
            }
            return;
        }
        if let Some(i) = self.chrome_down.take() {
            self.chrome.set_pressed(None);
            if let Hit::Button(j) = self.chrome.hit(x, y, self.scale) {
                if i == j {
                    let id = self.chrome.buttons[i].id.clone();
                    self.on_chrome_button(&id, after);
                }
            }
            self.invalidate();
            return;
        }
        self.dispatch(&Event::MouseUp { x, y, button: MouseButton::Left });
    }

    fn on_chrome_button(&mut self, id: &str, after: &mut After) {
        tracing::debug!("title-bar button {id}");
        match id {
            "close" => {
                tracing::info!("hide: close button");
                self.hide();
            }
            "fullscreen" => self.toggle_fullscreen(),
            "pin" => {
                self.pinned = !self.pinned;
                self.report_visible();
                self.update_chrome();
                self.show_toast(if self.pinned { "Kept open".into() } else { "Closes when you switch apps".into() });
            }
            "info" => self.toggle_info(),
            "open" => {
                if !self.view_path.is_empty() {
                    gt_shell::info::open_default(&self.view_path);
                    self.hide();
                }
            }
            "openwith" => {
                if !self.view_path.is_empty() {
                    *after = After::OpenWith(self.view_path.clone());
                }
            }
            "prev" => self.step(-1),
            "next" => self.step(1),
            "app:mediainfo" => self.toggle_report(),
            "app:back" => self.go_back(),
            "app:index" => self.toggle_index(),
            other => {
                if let Some(vid) = other.strip_prefix("view:") {
                    let Some(mut view) = self.view.take() else { return };
                    let mut cx = EventCx::new(self.hwnd, self.scale, &mut self.fonts);
                    view.on_action(vid, &mut cx);
                    let fx = Effects::from(&cx);
                    self.view = Some(view);
                    self.apply(fx);
                    self.handle_requests();
                }
            }
        }
    }

    // ------------------------------------------------------------ drawing

    fn render(&mut self) {
        if !self.visible {
            return;
        }
        let (w, h) = self.client_size();
        let hdr = self.view.as_ref().is_some_and(|v| v.uses_hdr_layer());
        let content = self.content_rect();
        let peak = self.peak_override();
        let sdr_scale = self.display.sdr_scale();
        let headroom = self.display.headroom(peak);
        let native = self.view.as_ref().is_some_and(|v| v.native_content());
        if native {
            let host = self.host_cx();
            if let Some(v) = self.view.as_mut() {
                v.layout_native(content, &host);
            }
        }
        if hdr && self.surface.content.ensure(&self.gfx, w, h).is_ok() {
            // Background in linear scRGB: the window colour at SDR white, or black in full screen.
            let (bg, alpha) = if self.fullscreen { (0.0, 1.0) } else { (0.01166 * sdr_scale, 0.94) };
            if let Ok(rtv) = self.surface.content.rtv(&self.gfx) {
                unsafe { self.gfx.d3d_ctx.ClearRenderTargetView(&rtv, &[bg * alpha, bg * alpha, bg * alpha, alpha]) };
                let peak_nits = if self.display.hdr { peak.unwrap_or(self.display.max_nits).max(self.display.sdr_white_nits) } else { 80.0 };
                let mut gpu = GpuCx {
                    device: &self.gfx.d3d,
                    ctx: &self.gfx.d3d_ctx,
                    rtv: &rtv,
                    target_w: w,
                    target_h: h,
                    hdr: self.display.hdr,
                    sdr_white_nits: if self.display.hdr { self.display.sdr_white_nits } else { 80.0 },
                    peak_nits,
                    scale: self.scale,
                };
                if let Some(v) = self.view.as_mut() {
                    v.render_gpu(&mut gpu, content);
                }
            }
            if self.surface.content.begin(&self.gfx).is_ok() {
                let mut cx = DrawCx { dc: &self.gfx.dc, brush: &self.brush, fonts: &mut self.fonts, theme: &self.theme, scale: self.scale, hdr_layer: true, sdr_scale, headroom };
                if let Some(v) = self.view.as_mut() {
                    v.draw_hdr(&mut cx, content);
                }
                let _ = self.surface.content.end_draw(&self.gfx);
            }
            if std::env::var_os("GT_READBACK").is_some() {
                let r = (content.x as u32, content.y as u32, content.w as u32, content.h as u32);
                if let Some((max, p99, mean)) = self.surface.content.readback_stats(&self.gfx, r) {
                    tracing::info!(
                        "readback {}: max {:.3} ({:.0} nits), p99 {:.3} ({:.0} nits), mean {:.3} | hdr={} sdr_white={:.0} peak={:.0}",
                        self.view_path,
                        max,
                        max * 80.0,
                        p99,
                        p99 * 80.0,
                        mean,
                        self.display.hdr,
                        self.display.sdr_white_nits,
                        peak.unwrap_or(self.display.max_nits)
                    );
                }
            }
            let _ = self.surface.content.present();
        } else if !hdr && self.surface.content.is_allocated() {
            self.surface.content.release(&self.gfx);
        }
        if self.surface.ui.ensure(&self.gfx, w, h).is_err() || self.surface.ui.begin(&self.gfx).is_err() {
            return;
        }
        let full = Rect::new(0.0, 0.0, w as f32, h as f32);
        let show_chrome = self.chrome_visible();
        {
            let mut cx = DrawCx { dc: &self.gfx.dc, brush: &self.brush, fonts: &mut self.fonts, theme: &self.theme, scale: self.scale, hdr_layer: false, sdr_scale, headroom };
            unsafe {
                let _ = cx.dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            }
            let bg = if self.fullscreen { rgba(0.0, 0.0, 0.0, 1.0) } else { cx.theme.window_bg };
            if native {
                // The video window shows through the content area: paint around it only.
                cx.fill(Rect::new(0.0, 0.0, full.w, content.y), bg);
                cx.fill(Rect::new(content.right(), content.y, full.w - content.right(), content.h), bg);
            } else if !hdr {
                cx.fill(full, bg);
            }
            if let Some(v) = self.view.as_mut() {
                cx.clip(content);
                v.draw(&mut cx, content);
                cx.unclip();
            }
            if let Some(rep) = self.report.as_mut() {
                cx.clip(content);
                rep.draw(&mut cx, content);
                cx.unclip();
            }
            if self.info_open {
                draw_info_panel(&mut cx, Rect::new(content.right(), content.y, full.w - content.right(), content.h), &self.info_facts, self.view.as_deref());
            }
            if show_chrome {
                self.chrome.layout(cx.fonts, self.scale, w as f32);
                self.chrome.draw(&mut cx, w as f32);
                if self.tip {
                    if let (Some(tip), Some(i)) = (self.chrome.tooltip().map(|s| s.to_string()), self.chrome.hovered()) {
                        draw_tooltip(&mut cx, &tip, self.chrome.buttons[i].rect, full);
                    }
                }
            }
            if let Some(t) = &self.toast {
                draw_toast(&mut cx, t, full);
            }
        }
        let _ = self.surface.ui.end(&self.gfx);
        self.surface.commit(&self.gfx);
        self.schedule_animation();
    }

    fn schedule_animation(&self) {
        if let Some(d) = self.view.as_ref().and_then(|v| v.next_frame_in()) {
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_ANIM, (d.as_millis() as u32).max(1), None);
            }
        }
    }
}

/// Flags copied out of an `EventCx` so the borrow on `fonts` can end.
struct Effects {
    redraw: bool,
    capture: Option<bool>,
    cursor: Cursor,
    toast: Option<String>,
    copy: Option<String>,
    relayout: bool,
    switch_provider: Option<&'static str>,
    fullscreen: bool,
}

impl From<&EventCx<'_>> for Effects {
    fn from(cx: &EventCx) -> Self {
        Self {
            redraw: cx.redraw,
            capture: cx.capture,
            cursor: cx.cursor,
            toast: cx.toast.clone(),
            copy: cx.copy.clone(),
            relayout: cx.relayout,
            switch_provider: cx.switch_provider,
            fullscreen: cx.fullscreen,
        }
    }
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string())
}

fn file_facts(path: &str) -> Vec<(String, String)> {
    let mut v = Vec::new();
    if let Ok(m) = std::fs::metadata(path) {
        if !m.is_dir() {
            v.push(("Size".into(), gt_core::fmt::size_long(m.len())));
        }
        if let Ok(t) = m.modified() {
            v.push(("Modified".into(), gt_core::fmt::system_time(t)));
        }
        if let Ok(t) = m.created() {
            v.push(("Created".into(), gt_core::fmt::system_time(t)));
        }
    }
    let kind = gt_shell::info::type_name(path);
    if !kind.is_empty() {
        v.insert(0, ("Kind".into(), kind));
    }
    v.push(("Where".into(), std::path::Path::new(path).parent().map(|p| p.display().to_string()).unwrap_or_default()));
    v
}

fn draw_info_panel(cx: &mut DrawCx, r: Rect, facts: &[(String, String)], view: Option<&dyn View>) {
    cx.fill(r, cx.theme.panel_bg);
    cx.vline(r.x, r.y, r.bottom(), cx.theme.separator);
    let pad = cx.px(16.0);
    let mut y = r.y + pad;
    let w = r.w - pad * 2.0;
    cx.text("Info", Rect::new(r.x + pad, y, w, cx.px(22.0)), Font::Semibold(14.0), cx.theme.text, Align::Left);
    y += cx.px(32.0);
    let mut rows: Vec<(String, String)> = facts.to_vec();
    if let Some(v) = view {
        let extra = v.info();
        if !extra.is_empty() {
            rows.push((String::new(), String::new()));
            rows.extend(extra);
        }
    }
    for (k, val) in rows {
        if y > r.bottom() - pad {
            break;
        }
        if k.is_empty() && val.is_empty() {
            cx.hline(r.x + pad, r.right() - pad, y + cx.px(6.0), cx.theme.separator);
            y += cx.px(14.0);
            continue;
        }
        if !k.is_empty() {
            cx.text(&k, Rect::new(r.x + pad, y, w, cx.px(18.0)), Font::Ui(11.5), cx.theme.text3, Align::Left);
            y += cx.px(18.0);
        }
        let used = cx.text_wrapped(&val, Rect::new(r.x + pad, y, w, cx.px(80.0)), Font::Ui(12.5), cx.theme.text);
        y += used.max(cx.px(18.0)) + cx.px(8.0);
    }
}

/// "Save as" for report text (runs a modal loop: call outside the `App` borrow).
fn save_text(hwnd: HWND, name: &str, text: &str) {
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileSaveDialog, IFileSaveDialog, SIGDN_FILESYSPATH};
    let clean: String = name.chars().map(|c| if r#"\/:*?"<>|"#.contains(c) { '_' } else { c }).collect();
    unsafe {
        let Ok(dlg) = CoCreateInstance::<_, IFileSaveDialog>(&FileSaveDialog, None, CLSCTX_INPROC_SERVER) else { return };
        let types = [COMDLG_FILTERSPEC { pszName: w!("Text file"), pszSpec: w!("*.txt") }];
        let _ = dlg.SetFileTypes(&types);
        let _ = dlg.SetDefaultExtension(w!("txt"));
        let _ = dlg.SetFileName(&HSTRING::from(clean));
        if dlg.Show(Some(hwnd)).is_err() {
            return;
        }
        let Ok(item) = dlg.GetResult() else { return };
        let Ok(p) = item.GetDisplayName(SIGDN_FILESYSPATH) else { return };
        let path = p.to_string().unwrap_or_default();
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
        if let Err(e) = std::fs::write(&path, text.replace('\n', "\r\n")) {
            tracing::warn!("saving {path}: {e}");
        }
    }
}

fn draw_tooltip(cx: &mut DrawCx, tip: &str, anchor: Rect, bounds: Rect) {
    let (tw, th) = cx.measure(tip, Font::Ui(12.0));
    let pad = cx.px(8.0);
    let w = tw + pad * 2.0;
    let h = th + pad;
    let x = (anchor.x + anchor.w / 2.0 - w / 2.0).clamp(bounds.x + 4.0, bounds.right() - w - 4.0);
    let y = anchor.bottom() + cx.px(4.0);
    let r = Rect::new(x, y, w, h);
    cx.round(r, cx.px(4.0), rgba(0.2, 0.2, 0.21, 0.98));
    cx.text(tip, r, Font::Ui(12.0), cx.theme.text, Align::Center);
}

fn draw_toast(cx: &mut DrawCx, t: &str, bounds: Rect) {
    let (tw, th) = cx.measure(t, Font::Semibold(13.0));
    let w = tw + cx.px(32.0);
    let h = th + cx.px(16.0);
    let r = Rect::new(bounds.x + (bounds.w - w) / 2.0, bounds.bottom() - h - cx.px(28.0), w, h);
    cx.round(r, h / 2.0, rgba(0.08, 0.08, 0.09, 0.9));
    cx.text(t, r, Font::Semibold(13.0), cx.theme.text, Align::Center);
}

#[allow(dead_code)]
fn _unused(_: HSTRING) {}

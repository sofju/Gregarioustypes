//! Explorer worker: a COM STA thread that resolves the selection of the foreground
//! Explorer window (Windows 11 tabs included) or the desktop, follows selection and
//! navigation changes through connection-point events, and moves the selection on
//! request.

use crate::hook::{ShellSurface, WM_GT_CLOSE, WM_GT_FULLSCREEN, WM_GT_STEP, WM_GT_TOGGLE};
use crate::util::take_pwstr;
use gt_ipc::{Item, RectI, ToViewer};
use windows::core::{implement, Interface, IUnknown, Result, GUID, PCWSTR};
use windows::Win32::Foundation::{E_NOTIMPL, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IConnectionPoint, IConnectionPointContainer, IDispatch, IDispatch_Impl, IServiceProvider, ITypeInfo, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED, DISPATCH_FLAGS, DISPPARAMS, EXCEPINFO,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::Shell::{
    DShellFolderViewEvents, DShellFolderViewEvents_Impl, DWebBrowserEvents2, DWebBrowserEvents2_Impl, IFolderView2, IShellBrowser, IShellItem, IShellItemArray,
    IShellView, IShellWindows, IWebBrowser2, ShellWindows, CSIDL_DESKTOP, SID_STopLevelBrowser, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_FILESYSPATH, SVGIO_BACKGROUND,
    SVGIO_FLAG_VIEWORDER, SVGIO_SELECTION, SVSI_DESELECTOTHERS, SVSI_ENSUREVISIBLE, SVSI_FOCUSED, SVSI_SELECT, SWC_DESKTOP, SWFO_NEEDDISPATCH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, FindWindowExW, GetMessageW, KillTimer, PeekMessageW, PostThreadMessageW, SetTimer, TranslateMessage, MSG, PM_NOREMOVE, WM_APP, WM_TIMER,
    WM_USER,
};

pub const WM_GT_SELCHANGED: u32 = WM_APP + 20;
pub const WM_GT_NAVCOMPLETE: u32 = WM_APP + 21;
/// Move the Explorer selection; lparam = delta. Sent when the viewer has focus.
pub const WM_GT_NAVIGATE: u32 = WM_APP + 22;
/// The viewer was hidden; drop event subscriptions.
pub const WM_GT_UNTRACK: u32 = WM_APP + 23;

const DISPID_SELECTIONCHANGED: i32 = 200;
const DISPID_NAVIGATECOMPLETE2: i32 = 252;
const DISPID_DOCUMENTCOMPLETE: i32 = 259;

pub type Emit = Box<dyn Fn(ToViewer) + Send>;

/// Start the worker; returns its thread id (target for hook and agent messages).
pub fn spawn(emit: Emit) -> u32 {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("gt-explorer".into())
        .spawn(move || unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let mut msg = MSG::default();
            let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE); // create the queue
            let _ = tx.send(GetCurrentThreadId());
            let mut w = Worker { emit, windows: None, tracked: None, dialog: None, sel_timer: 0, nav_timer: 0, thread: GetCurrentThreadId() };
            // Create ShellWindows up front so the first Space press doesn't pay for it.
            let _ = w.shell_windows();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.hwnd.is_invalid() {
                    w.handle(&msg);
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .expect("explorer thread");
    rx.recv().expect("explorer thread id")
}

pub fn post(thread: u32, msg: u32, l: isize) {
    unsafe {
        let _ = PostThreadMessageW(thread, msg, WPARAM(0), LPARAM(l));
    }
}

struct Tracked {
    browser: IShellBrowser,
    web: Option<IWebBrowser2>,
    fv: IFolderView2,
    view_conn: Option<(IConnectionPoint, u32)>,
    web_conn: Option<(IConnectionPoint, u32)>,
    items: Vec<Item>,
}

/// A file dialog being previewed: its selection is polled while the preview shows.
struct DialogTrack {
    hwnd: HWND,
    link: crate::dialog::Link,
    items: Vec<Item>,
    timer: usize,
}

struct Worker {
    emit: Emit,
    windows: Option<IShellWindows>,
    tracked: Option<Tracked>,
    dialog: Option<DialogTrack>,
    sel_timer: usize,
    nav_timer: usize,
    thread: u32,
}

/// Paths from a dialog: file-system paths have a drive or UNC root.
fn dialog_item(p: String) -> Item {
    let b = p.as_bytes();
    let sep = |c: u8| c == 0x5C || c == b'/';
    let fs = (b.len() > 2 && b[1] == b':' && sep(b[2])) || (b.len() > 2 && sep(b[0]) && sep(b[1]));
    Item { path: p, filesystem: fs }
}

fn variant_i4(v: i32) -> VARIANT {
    let mut var = VARIANT::default();
    unsafe {
        let inner = &mut *var.Anonymous.Anonymous;
        inner.vt = VT_I4;
        inner.Anonymous.lVal = v;
    }
    var
}

fn item_of(si: &IShellItem) -> Option<Item> {
    unsafe {
        if let Ok(p) = si.GetDisplayName(SIGDN_FILESYSPATH) {
            if let Some(path) = take_pwstr(p) {
                return Some(Item { path, filesystem: true });
            }
        }
        let p = si.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING).ok()?;
        take_pwstr(p).map(|path| Item { path, filesystem: false })
    }
}

/// Selected items in view order, and the index of the focused one within them.
fn selection(fv: &IFolderView2) -> (Vec<Item>, usize) {
    unsafe {
        let mut items = Vec::new();
        if let Ok(arr) = fv.Items::<IShellItemArray>(SVGIO_SELECTION | SVGIO_FLAG_VIEWORDER) {
            let n = arr.GetCount().unwrap_or(0);
            for i in 0..n {
                if let Ok(si) = arr.GetItemAt(i) {
                    if let Some(it) = item_of(&si) {
                        items.push(it);
                    }
                }
            }
        }
        let focused = fv.GetFocusedItem().ok().and_then(|i| fv.GetItem::<IShellItem>(i).ok()).and_then(|si| item_of(&si));
        let index = focused.and_then(|f| items.iter().position(|it| it.path == f.path)).unwrap_or(0);
        (items, index)
    }
}

/// Previous and next items (view order) around the focused item, for prefetching.
fn neighbours(fv: &IFolderView2) -> Vec<Item> {
    let mut v = Vec::new();
    unsafe {
        let Ok(cur) = fv.GetFocusedItem() else { return v };
        for prev in [false, true] {
            if let Ok(i) = fv.GetVisibleItem(cur, prev) {
                if i >= 0 && i != cur {
                    if let Some(it) = fv.GetItem::<IShellItem>(i).ok().and_then(|si| item_of(&si)) {
                        if it.filesystem {
                            v.push(it);
                        }
                    }
                }
            }
        }
    }
    v
}

fn monitor_rect(hwnd: HWND) -> Option<RectI> {
    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        GetMonitorInfoW(mon, &mut mi).as_bool().then(|| RectI { l: mi.rcWork.left, t: mi.rcWork.top, r: mi.rcWork.right, b: mi.rcWork.bottom })
    }
}

fn connect(source: &IUnknown, iid: &GUID, sink: &IUnknown) -> Option<(IConnectionPoint, u32)> {
    unsafe {
        let cpc: IConnectionPointContainer = source.cast().ok()?;
        let cp = cpc.FindConnectionPoint(iid).ok()?;
        let cookie = cp.Advise(sink).ok()?;
        Some((cp, cookie))
    }
}

fn disconnect(conn: &mut Option<(IConnectionPoint, u32)>) {
    if let Some((cp, cookie)) = conn.take() {
        unsafe {
            let _ = cp.Unadvise(cookie);
        }
    }
}

impl Worker {
    fn handle(&mut self, msg: &MSG) {
        match msg.message {
            WM_GT_TOGGLE => {
                let fg = HWND(msg.wParam.0 as *mut _);
                if crate::util::class_name(fg) == "#32770" {
                    self.open_dialog(fg, msg.lParam.0 != 0);
                } else {
                    self.open(fg, msg.lParam.0 != 0);
                }
            }
            WM_GT_CLOSE => {
                (self.emit)(ToViewer::Hide);
                self.untrack();
            }
            WM_GT_FULLSCREEN => (self.emit)(ToViewer::ToggleFullscreen),
            WM_GT_STEP => (self.emit)(ToViewer::Step(msg.lParam.0 as i32)),
            WM_GT_NAVIGATE => self.navigate(msg.lParam.0 as i32),
            WM_GT_UNTRACK => self.untrack(),
            WM_GT_SELCHANGED => self.sel_timer = self.restart_timer(self.sel_timer, 30),
            WM_GT_NAVCOMPLETE => self.nav_timer = self.restart_timer(self.nav_timer, 120),
            WM_TIMER => {
                let id = msg.wParam.0;
                if self.dialog.as_ref().is_some_and(|d| d.timer == id) && id != 0 {
                    self.refresh_dialog();
                } else if id == self.sel_timer && id != 0 {
                    self.sel_timer = self.kill(id);
                    self.refresh(false);
                } else if id == self.nav_timer && id != 0 {
                    self.nav_timer = self.kill(id);
                    self.refresh(true);
                }
            }
            _ => {}
        }
    }

    fn restart_timer(&self, old: usize, ms: u32) -> usize {
        unsafe {
            if old != 0 {
                let _ = KillTimer(None, old);
            }
            SetTimer(None, 0, ms, None)
        }
    }

    fn kill(&self, id: usize) -> usize {
        unsafe {
            let _ = KillTimer(None, id);
        }
        0
    }

    fn shell_windows(&mut self) -> Option<IShellWindows> {
        if self.windows.is_none() {
            self.windows = unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_ALL) }.map_err(|e| tracing::warn!("ShellWindows: {e}")).ok();
        }
        self.windows.clone()
    }

    fn find_browser(&mut self, fg: HWND) -> Option<(IShellBrowser, Option<IWebBrowser2>)> {
        let sw = self.shell_windows()?;
        unsafe {
            if crate::hook::shell_surface(fg) == Some(ShellSurface::Desktop) {
                let loc = variant_i4(CSIDL_DESKTOP as i32);
                let root = VARIANT::default();
                let mut hwnd = 0i32;
                let disp = sw.FindWindowSW(&loc, &root, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH).ok()?;
                let sp: IServiceProvider = disp.cast().ok()?;
                let b: IShellBrowser = sp.QueryService(&SID_STopLevelBrowser).ok()?;
                return Some((b, None));
            }
            // Windows 11 tabs: every tab is a separate shell window sharing the frame HWND;
            // the active tab is the first ShellTabWindowClass child.
            let active_tab = FindWindowExW(Some(fg), None, windows::core::w!("ShellTabWindowClass"), PCWSTR::null()).ok();
            let n = sw.Count().unwrap_or(0);
            for i in 0..n {
                let Ok(disp) = sw.Item(&variant_i4(i)) else { continue };
                let Ok(wb) = disp.cast::<IWebBrowser2>() else { continue };
                let Ok(h) = wb.HWND() else { continue };
                if h.0 != fg.0 as isize {
                    continue;
                }
                let Ok(sp) = wb.cast::<IServiceProvider>() else { continue };
                let Ok(b) = sp.QueryService::<IShellBrowser>(&SID_STopLevelBrowser) else { continue };
                if let Some(tab) = active_tab {
                    if let Ok(bh) = b.GetWindow() {
                        if bh != tab {
                            continue;
                        }
                    }
                }
                return Some((b, Some(wb)));
            }
        }
        None
    }

    fn open(&mut self, fg: HWND, fullscreen: bool) {
        let t0 = std::time::Instant::now();
        let Some((browser, web)) = self.find_browser(fg) else {
            tracing::warn!("no shell browser for foreground window");
            return;
        };
        let Some(fv) = (unsafe { browser.QueryActiveShellView() }).ok().and_then(|v| v.cast::<IFolderView2>().ok()) else { return };
        let (items, index) = selection(&fv);
        if items.is_empty() {
            return;
        }
        tracing::info!("selection resolved in {:?}: {} item(s)", t0.elapsed(), items.len());
        self.untrack();
        let mut t = Tracked { browser, web, fv, view_conn: None, web_conn: None, items: items.clone() };
        self.attach(&mut t);
        self.tracked = Some(t);
        (self.emit)(ToViewer::Show { items, index, fullscreen, anchor: None, monitor: monitor_rect(fg), crashed: None });
        if let Some(t) = &self.tracked {
            (self.emit)(ToViewer::Prefetch(neighbours(&t.fv)));
        }
    }

    fn attach(&self, t: &mut Tracked) {
        unsafe {
            if let Ok(view) = t.browser.QueryActiveShellView() {
                if let Ok(fv) = view.cast::<IFolderView2>() {
                    t.fv = fv;
                }
                if let Ok(disp) = view.GetItemObject::<IDispatch>(SVGIO_BACKGROUND) {
                    let sink: IUnknown = ViewSink { thread: self.thread }.into();
                    t.view_conn = connect(&disp.cast().unwrap(), &DShellFolderViewEvents::IID, &sink);
                }
            }
            if t.web_conn.is_none() {
                if let Some(wb) = &t.web {
                    let sink: IUnknown = WebSink { thread: self.thread }.into();
                    t.web_conn = connect(&wb.cast().unwrap(), &DWebBrowserEvents2::IID, &sink);
                }
            }
        }
    }

    fn untrack(&mut self) {
        if let Some(mut t) = self.tracked.take() {
            disconnect(&mut t.view_conn);
            disconnect(&mut t.web_conn);
        }
        if let Some(d) = self.dialog.take() {
            self.kill(d.timer);
            crate::hook::DIALOG.store(0, std::sync::atomic::Ordering::Release);
        }
    }

    /// Space in another app's Open/Save dialog.
    fn open_dialog(&mut self, fg: HWND, fullscreen: bool) {
        let t0 = std::time::Instant::now();
        self.untrack();
        let Some(mut link) = crate::dialog::Link::new(fg) else { return };
        let Some((folder, paths, index)) = link.query(fg, 400) else {
            tracing::warn!("file dialog did not answer");
            return;
        };
        tracing::info!("dialog selection resolved in {:?}: {} item(s) in {folder}", t0.elapsed(), paths.len());
        if paths.is_empty() {
            return;
        }
        let items: Vec<Item> = paths.into_iter().map(dialog_item).collect();
        crate::hook::DIALOG.store(fg.0 as isize, std::sync::atomic::Ordering::Release);
        let timer = unsafe { SetTimer(None, 0, 150, None) };
        self.dialog = Some(DialogTrack { hwnd: fg, link, items: items.clone(), timer });
        (self.emit)(ToViewer::Show { items, index, fullscreen, anchor: None, monitor: monitor_rect(fg), crashed: None });
    }

    /// Follow the dialog's selection; the dialog closing ends the preview.
    fn refresh_dialog(&mut self) {
        let Some(mut d) = self.dialog.take() else { return };
        if !unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(d.hwnd)) }.as_bool() {
            self.kill(d.timer);
            crate::hook::DIALOG.store(0, std::sync::atomic::Ordering::Release);
            (self.emit)(ToViewer::Hide);
            return;
        }
        if let Some((_, paths, index)) = d.link.query(d.hwnd, 150) {
            let items: Vec<Item> = paths.into_iter().map(dialog_item).collect();
            if !items.is_empty() && items != d.items {
                d.items = items.clone();
                (self.emit)(ToViewer::Update { items, index, anchor: None });
            }
        }
        self.dialog = Some(d);
    }

    fn refresh(&mut self, after_navigation: bool) {
        let Some(mut t) = self.tracked.take() else { return };
        if after_navigation {
            disconnect(&mut t.view_conn);
            self.attach(&mut t);
        }
        let (items, index) = selection(&t.fv);
        if !items.is_empty() && items != t.items {
            t.items = items.clone();
            (self.emit)(ToViewer::Update { items, index, anchor: None });
            (self.emit)(ToViewer::Prefetch(neighbours(&t.fv)));
        }
        self.tracked = Some(t);
    }

    fn navigate(&mut self, delta: i32) {
        let Some(t) = &self.tracked else { return };
        unsafe {
            let Ok(cur) = t.fv.GetFocusedItem() else { return };
            if let Ok(next) = t.fv.GetVisibleItem(cur, delta < 0) {
                if next >= 0 && next != cur {
                    let flags = SVSI_SELECT.0 | SVSI_DESELECTOTHERS.0 | SVSI_ENSUREVISIBLE.0 | SVSI_FOCUSED.0;
                    let _ = t.fv.SelectItem(next, flags as u32);
                }
            }
        }
    }
}

macro_rules! dispatch_stub {
    () => {
        fn GetTypeInfoCount(&self) -> Result<u32> {
            Ok(0)
        }
        fn GetTypeInfo(&self, _itinfo: u32, _lcid: u32) -> Result<ITypeInfo> {
            Err(E_NOTIMPL.into())
        }
        fn GetIDsOfNames(&self, _riid: *const GUID, _names: *const PCWSTR, _n: u32, _lcid: u32, _ids: *mut i32) -> Result<()> {
            Err(E_NOTIMPL.into())
        }
    };
}

#[implement(DShellFolderViewEvents)]
struct ViewSink {
    thread: u32,
}

impl IDispatch_Impl for ViewSink_Impl {
    dispatch_stub!();
    fn Invoke(&self, id: i32, _: *const GUID, _: u32, _: DISPATCH_FLAGS, _: *const DISPPARAMS, _: *mut VARIANT, _: *mut EXCEPINFO, _: *mut u32) -> Result<()> {
        if id == DISPID_SELECTIONCHANGED {
            post(self.thread, WM_GT_SELCHANGED, 0);
        }
        Ok(())
    }
}
impl DShellFolderViewEvents_Impl for ViewSink_Impl {}

#[implement(DWebBrowserEvents2)]
struct WebSink {
    thread: u32,
}

impl IDispatch_Impl for WebSink_Impl {
    dispatch_stub!();
    fn Invoke(&self, id: i32, _: *const GUID, _: u32, _: DISPATCH_FLAGS, _: *const DISPPARAMS, _: *mut VARIANT, _: *mut EXCEPINFO, _: *mut u32) -> Result<()> {
        if id == DISPID_NAVIGATECOMPLETE2 || id == DISPID_DOCUMENTCOMPLETE {
            post(self.thread, WM_GT_NAVCOMPLETE, 0);
        }
        Ok(())
    }
}
impl DWebBrowserEvents2_Impl for WebSink_Impl {}

#[allow(dead_code)]
fn _assert_unused(_: IShellView) {}

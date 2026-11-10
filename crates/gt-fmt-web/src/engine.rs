//! One WebView2 for the whole viewer, created ahead of time and reused by every
//! web preview (HTML, Markdown, SWF), the way the video engine keeps mpv warm.
//!
//! Every request goes through `WebResourceRequested`: pages and their files are
//! served from disk under `https://gt.localhost/<drive>/<path>`, generated pages
//! (Markdown, the Ruffle and 3D host pages) and converted files from memory,
//! bundled libraries from `deps/` (`__ruffle__/`, `__three__/`), and anything
//! else (the network) is refused. Scripts are off except for our own pages.
//!
//! Our own pages may talk to the viewer with web messages: `{"gtReady":true}`
//! asks for `Site::hello`, `{"gtKey":vk}` presses a key in the viewer, anything
//! else is queued for the view (`take_messages`). A page with `Site::reuse`
//! stays loaded when its view goes away, so the next file of the same kind
//! only needs a message instead of a navigation.

use parking_lot::Mutex;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::*;
use windows::core::{Interface, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Com::IStream;
use windows::Win32::UI::Shell::{SHCreateMemStream, SHCreateStreamOnFileEx, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_SHOWNORMAL, WM_KEYDOWN};

pub const HOST: &str = "https://gt.localhost/";

/// Content served at `https://gt.localhost/__blob__/<name>`.
#[derive(Clone)]
pub enum Blob {
    Mem(Arc<Vec<u8>>),
    File(PathBuf),
}

/// What the engine serves for the current preview.
#[derive(Clone, Default)]
pub struct Site {
    /// Generated page served at `https://gt.localhost/__page__`.
    pub page: Option<String>,
    /// Files under this folder (and below) may be served.
    pub root: Option<PathBuf>,
    pub scripts: bool,
    /// The page may post web messages to the viewer.
    pub messages: bool,
    /// Posted to the page when it reports `{"gtReady":true}`, or right away
    /// when an already loaded page is reused.
    pub hello: Option<String>,
    /// Keep the page loaded after its view goes away; this message is posted
    /// to it then (to drop what it shows).
    pub reuse: Option<String>,
    pub blobs: Vec<(String, Blob, &'static str)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Creating,
    Ready,
    Failed,
}

struct Web {
    state: State,
    error: String,
    parent: HWND,
    wake_msg: u32,
    controller: Option<ICoreWebView2Controller>,
    webview: Option<ICoreWebView2>,
    site: Arc<Mutex<Site>>,
    /// Navigation waiting for the controller.
    pending: Option<(String, u32)>,
    /// URL being shown and its background colour (0xRRGGBB).
    current: Option<String>,
    /// The new page has finished loading: show the controller.
    loaded: Arc<Mutex<bool>>,
    visible: bool,
    bounds: RECT,
    /// A reusable page is kept loaded but hidden, with nothing to show.
    parked: bool,
    /// Messages from the page for the view.
    inbox: Arc<Mutex<Vec<String>>>,
    /// The current page reported `{"gtReady":true}`.
    page_ready: Arc<std::sync::atomic::AtomicBool>,
}

thread_local! {
    static WEB: RefCell<Option<Web>> = const { RefCell::new(None) };
}

/// `C:\a b\c.html` → `https://gt.localhost/C/a%20b/c.html`.
pub fn url_for(path: &Path) -> String {
    let s = path.display().to_string();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s).replace('\\', "/");
    let s = s.replacen(':', "", 1);
    let mut out = String::from(HOST);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn path_for(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix(HOST)?;
    let rest = rest.split(['?', '#']).next()?;
    let mut bytes = Vec::new();
    let b = rest.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                bytes.push(v);
                i += 3;
                continue;
            }
        }
        bytes.push(b[i]);
        i += 1;
    }
    let s = String::from_utf8_lossy(&bytes).to_string();
    let (drive, tail) = s.split_once('/')?;
    if drive.len() != 1 || !drive.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let p = PathBuf::from(format!("{drive}:\\{}", tail.replace('/', "\\")));
    // No escaping the folder with "..": normalise lexically.
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    Some(out)
}

fn mime(path: &Path) -> &'static str {
    match path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).as_deref() {
        Some("html" | "htm" | "xhtml" | "shtml") => "text/html",
        Some("css") => "text/css",
        Some("js" | "mjs") => "text/javascript",
        Some("json" | "map") => "application/json",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("bmp") => "image/bmp",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("swf") => "application/x-shockwave-flash",
        Some("mp4" | "m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("txt" | "md" | "markdown") => "text/plain; charset=utf-8",
        Some("glb") => "model/gltf-binary",
        Some("gltf") => "model/gltf+json",
        Some("ktx2") => "image/ktx2",
        _ => "application/octet-stream",
    }
}

fn respond(env: &ICoreWebView2Environment, args: &ICoreWebView2WebResourceRequestedEventArgs, stream: Option<IStream>, status: i32, reason: &str, ctype: &str) {
    let headers = HSTRING::from(format!("Content-Type: {ctype}\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *"));
    unsafe {
        if let Ok(r) = env.CreateWebResourceResponse(stream.as_ref(), status, &HSTRING::from(reason), &headers) {
            let _ = args.SetResponse(&r);
        }
    }
}

fn mem_stream(bytes: &[u8]) -> Option<IStream> {
    unsafe { SHCreateMemStream(Some(bytes)) }
}

fn serve(env: &ICoreWebView2Environment, site: &Arc<Mutex<Site>>, args: &ICoreWebView2WebResourceRequestedEventArgs) {
    let uri = unsafe {
        let Ok(req) = args.Request() else { return };
        let mut p = PWSTR::null();
        if req.Uri(&mut p).is_err() {
            return;
        }
        take_pwstr(p)
    };
    let site = site.lock().clone();
    tracing::debug!("web request {uri}");
    if !uri.starts_with(HOST) {
        // The network is off in previews.
        respond(env, args, None, 403, "Blocked", "text/plain");
        return;
    }
    let tail = &uri[HOST.len()..];
    if tail.starts_with("__page__") {
        let page = site.page.unwrap_or_default();
        respond(env, args, mem_stream(page.as_bytes()), 200, "OK", "text/html; charset=utf-8");
        return;
    }
    // Bundled libraries.
    for (prefix, dir) in [("__ruffle__/", "ruffle"), ("__three__/", "three")] {
        if let Some(f) = tail.strip_prefix(prefix) {
            let name = f.split(['?', '#']).next().unwrap_or("");
            let p = gt_core::paths::deps_dir().join(dir).join(name.replace('/', "\\"));
            if !name.contains("..") && p.is_file() {
                serve_file(env, args, &p);
            } else {
                respond(env, args, None, 404, "Not Found", "text/plain");
            }
            return;
        }
    }
    if let Some(f) = tail.strip_prefix("__blob__/") {
        let name = f.split(['?', '#']).next().unwrap_or("");
        match site.blobs.iter().find(|(n, _, _)| n == name) {
            Some((_, Blob::Mem(b), ctype)) => respond(env, args, mem_stream(b), 200, "OK", ctype),
            Some((_, Blob::File(p), _)) => serve_file(env, args, p),
            None => respond(env, args, None, 404, "Not Found", "text/plain"),
        }
        return;
    }
    let Some(path) = path_for(&uri) else {
        respond(env, args, None, 404, "Not Found", "text/plain");
        return;
    };
    let allowed = site.root.as_ref().is_some_and(|r| path.starts_with(r));
    if !allowed {
        respond(env, args, None, 403, "Blocked", "text/plain");
        return;
    }
    // Markdown links open rendered.
    let is_md = path.extension().is_some_and(|e| crate::markdown::EXTS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str()));
    if is_md && path.is_file() {
        if let Ok(html) = crate::markdown::render_file(&path) {
            respond(env, args, mem_stream(html.as_bytes()), 200, "OK", "text/html; charset=utf-8");
            return;
        }
    }
    serve_file(env, args, &path);
}

fn serve_file(env: &ICoreWebView2Environment, args: &ICoreWebView2WebResourceRequestedEventArgs, path: &Path) {
    use windows::Win32::System::Com::{STGM_READ, STGM_SHARE_DENY_NONE};
    let stream = unsafe { SHCreateStreamOnFileEx(&HSTRING::from(path.as_os_str()), (STGM_READ | STGM_SHARE_DENY_NONE).0, 0, false, None) };
    match stream {
        Ok(s) => respond(env, args, Some(s), 200, "OK", mime(path)),
        Err(_) => respond(env, args, None, 404, "Not Found", "text/plain"),
    }
}

fn open_external(url: &str) {
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
        unsafe {
            ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(url), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        }
    }
}

fn post_wake(parent: HWND, msg: u32) {
    unsafe {
        let _ = PostMessageW(Some(parent), msg, WPARAM(0), LPARAM(0));
    }
}

fn setup(env: ICoreWebView2Environment, controller: ICoreWebView2Controller) -> windows::core::Result<()> {
    let webview = unsafe { controller.CoreWebView2()? };
    unsafe {
        let s = webview.Settings()?;
        s.SetAreDevToolsEnabled(false)?;
        s.SetIsStatusBarEnabled(false)?;
        s.SetIsWebMessageEnabled(false)?;
        s.SetAreHostObjectsAllowed(false)?;
        s.SetIsZoomControlEnabled(true)?;
        s.SetIsScriptEnabled(false)?;
        controller.SetIsVisible(false)?;
        if let Ok(c2) = controller.cast::<ICoreWebView2Controller2>() {
            let _ = c2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR { A: 255, R: 28, G: 28, B: 29 });
        }
        webview.AddWebResourceRequestedFilter(&HSTRING::from("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)?;
    }
    let (site, loaded, parent, wake, inbox, page_ready) = WEB.with(|w| {
        let w = w.borrow();
        let w = w.as_ref().unwrap();
        (w.site.clone(), w.loaded.clone(), w.parent, w.wake_msg, w.inbox.clone(), w.page_ready.clone())
    });
    let site_msg = site.clone();
    let mut token = 0i64;
    let env2 = env.clone();
    let handler = WebResourceRequestedEventHandler::create(Box::new(move |_wv, args| {
        if let Some(a) = args {
            serve(&env2, &site, &a);
        }
        Ok(())
    }));
    unsafe { webview.add_WebResourceRequested(&handler, &mut token)? };
    let starting = NavigationStartingEventHandler::create(Box::new(move |_wv, args| {
        let Some(a) = args else { return Ok(()) };
        unsafe {
            let mut p = PWSTR::null();
            a.Uri(&mut p)?;
            let uri = take_pwstr(p);
            let mut user = windows::core::BOOL::default();
            let _ = a.IsUserInitiated(&mut user);
            // Links leave the preview: open them in the browser.
            if !uri.starts_with(HOST) && uri != "about:blank" {
                a.SetCancel(true)?;
                if user.as_bool() {
                    open_external(&uri);
                }
            }
        }
        Ok(())
    }));
    unsafe { webview.add_NavigationStarting(&starting, &mut token)? };
    let new_window = NewWindowRequestedEventHandler::create(Box::new(move |_wv, args| {
        if let Some(a) = args {
            unsafe {
                a.SetHandled(true)?;
                let mut p = PWSTR::null();
                if a.Uri(&mut p).is_ok() {
                    open_external(&take_pwstr(p));
                }
            }
        }
        Ok(())
    }));
    unsafe { webview.add_NewWindowRequested(&new_window, &mut token)? };
    let completed = NavigationCompletedEventHandler::create(Box::new(move |_wv, args| {
        if let Some(a) = args {
            let mut ok = windows::core::BOOL::default();
            let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
            unsafe {
                let _ = a.IsSuccess(&mut ok);
                let _ = a.WebErrorStatus(&mut status);
            }
            tracing::debug!("web navigation completed: success={} status={}", ok.as_bool(), status.0);
        }
        *loaded.lock() = true;
        post_wake(parent, wake);
        Ok(())
    }));
    unsafe { webview.add_NavigationCompleted(&completed, &mut token)? };
    // Our own pages talk back (3D viewer: ready, keys, statistics).
    let received = WebMessageReceivedEventHandler::create(Box::new(move |wv, args| {
        let (Some(wv), Some(a)) = (wv, args) else { return Ok(()) };
        unsafe {
            let mut src = PWSTR::null();
            let _ = a.Source(&mut src);
            if !take_pwstr(src).starts_with(HOST) {
                return Ok(());
            }
            let mut p = PWSTR::null();
            if a.WebMessageAsJson(&mut p).is_err() {
                return Ok(());
            }
            let json = take_pwstr(p);
            let v: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
            if v["gtReady"].as_bool() == Some(true) {
                page_ready.store(true, std::sync::atomic::Ordering::Release);
                if let Some(h) = site_msg.lock().hello.clone() {
                    let _ = wv.PostWebMessageAsJson(&HSTRING::from(h));
                }
            } else if let Some(vk) = v["gtKey"].as_u64() {
                let _ = PostMessageW(Some(parent), WM_KEYDOWN, WPARAM(vk as usize), LPARAM(0));
            } else {
                inbox.lock().push(json);
                post_wake(parent, wake);
            }
        }
        Ok(())
    }));
    unsafe { webview.add_WebMessageReceived(&received, &mut token)? };
    // Esc inside the page still closes the preview.
    let keys = AcceleratorKeyPressedEventHandler::create(Box::new(move |_c, args| {
        if let Some(a) = args {
            unsafe {
                let mut vk = 0u32;
                let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                let _ = a.VirtualKey(&mut vk);
                let _ = a.KeyEventKind(&mut kind);
                if vk == 0x1B && kind == COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN {
                    a.SetHandled(true)?;
                    let _ = PostMessageW(Some(parent), WM_KEYDOWN, WPARAM(0x1B), LPARAM(0));
                }
            }
        }
        Ok(())
    }));
    unsafe { controller.add_AcceleratorKeyPressed(&keys, &mut token)? };
    let pending = WEB.with(|w| {
        let mut w = w.borrow_mut();
        let w = w.as_mut().unwrap();
        w.controller = Some(controller);
        w.webview = Some(webview);
        w.state = State::Ready;
        w.pending.take()
    });
    if let Some((url, bg)) = pending {
        navigate(&url, bg);
    }
    post_wake(parent, wake);
    Ok(())
}

fn fail(e: String) {
    tracing::warn!("WebView2: {e}");
    WEB.with(|w| {
        if let Some(w) = w.borrow_mut().as_mut() {
            w.state = State::Failed;
            w.error = e;
            post_wake(w.parent, w.wake_msg);
        }
    });
}

static PARENT: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static WAKE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static WANT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Remember the window that will host web previews (nothing is started yet:
/// WebView2 costs ~120 MB, so it starts when a web file is first selected).
pub fn init(parent: HWND, wake_msg: u32) {
    use std::sync::atomic::Ordering;
    PARENT.store(parent.0 as isize, Ordering::Release);
    WAKE.store(wake_msg, Ordering::Release);
}

/// Any thread: a web file is selected or about to be; start WebView2 now.
pub fn request_warm() {
    use std::sync::atomic::Ordering;
    let parent = PARENT.load(Ordering::Acquire);
    if parent == 0 || WANT.swap(true, Ordering::AcqRel) {
        return;
    }
    post_wake(HWND(parent as *mut _), WAKE.load(Ordering::Acquire));
}

/// A reusable page to load hidden as soon as nothing else is shown.
static PRELOAD: parking_lot::Mutex<Option<(String, Site)>> = parking_lot::Mutex::new(None);

/// Any thread: a file for this reusable page (`Site::reuse`) is selected; load
/// the page hidden ahead of time so showing it only takes a message.
pub fn request_preload(url: &str, site: Site) {
    {
        let mut p = PRELOAD.lock();
        if p.as_ref().is_some_and(|(u, _)| u == url) {
            return;
        }
        *p = Some((url.to_string(), site));
    }
    WANT.store(false, std::sync::atomic::Ordering::Release);
    request_warm();
}

/// UI thread, on the wake message: start WebView2 if it was asked for, and
/// load a requested page hidden when nothing is shown.
pub fn pump() {
    use std::sync::atomic::Ordering;
    let parent = PARENT.load(Ordering::Acquire);
    if parent != 0 && WANT.load(Ordering::Acquire) {
        prewarm(HWND(parent as *mut _), WAKE.load(Ordering::Acquire));
    }
    let start = WEB.with(|w| {
        let mut w = w.borrow_mut();
        let Some(w) = w.as_mut() else { return None };
        if w.state != State::Ready {
            return None;
        }
        let (url, site) = PRELOAD.lock().take()?;
        // Something is shown, or this page is already loaded: nothing to do.
        if w.current.is_some() {
            return None;
        }
        *w.site.lock() = site;
        *w.loaded.lock() = false;
        w.current = Some(url.clone());
        w.parked = true;
        Some(url)
    });
    if let Some(url) = start {
        tracing::debug!("preloading {url}");
        navigate(&url, 0x1F1F21);
    }
}

/// Create the WebView2 environment and controller in the background (UI thread,
/// asynchronous; callbacks arrive through the message loop).
pub fn prewarm(parent: HWND, wake_msg: u32) {
    let exists = WEB.with(|w| w.borrow().is_some());
    if exists {
        return;
    }
    WEB.with(|w| {
        *w.borrow_mut() = Some(Web {
            state: State::Creating,
            error: String::new(),
            parent,
            wake_msg,
            controller: None,
            webview: None,
            site: Arc::default(),
            pending: None,
            current: None,
            loaded: Arc::default(),
            visible: false,
            bounds: RECT::default(),
            parked: false,
            inbox: Arc::default(),
            page_ready: Arc::default(),
        })
    });
    let t = std::time::Instant::now();
    let data = gt_core::paths::cache_dir().join("webview2");
    let options = CoreWebView2EnvironmentOptions::default();
    unsafe {
        // Autoplay for SWF sound; no first-run or sync noise.
        options.set_scroll_bar_style(COREWEBVIEW2_SCROLLBAR_STYLE_FLUENT_OVERLAY);
        options.set_additional_browser_arguments("--autoplay-policy=no-user-gesture-required --disable-features=msSmartScreenProtection,msEdgeEnhancedSecurityMode".to_string());
    }
    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |hr, env| {
        if let Err(e) = hr {
            fail(format!("environment: {e}"));
            return Ok(());
        }
        let Some(env) = env else {
            fail("no environment".into());
            return Ok(());
        };
        tracing::info!("WebView2 environment in {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
        let env2 = env.clone();
        let done = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |hr, c| {
            match (hr, c) {
                (Ok(()), Some(c)) => {
                    tracing::info!("WebView2 controller in {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
                    if let Err(e) = setup(env2, c) {
                        fail(format!("setup: {e}"));
                    }
                }
                (Err(e), _) => fail(format!("controller: {e}")),
                _ => fail("no controller".into()),
            }
            Ok(())
        }));
        unsafe {
            if let Err(e) = env.CreateCoreWebView2Controller(parent, &done) {
                fail(format!("controller: {e}"));
            }
        }
        Ok(())
    }));
    let r = unsafe { CreateCoreWebView2EnvironmentWithOptions(PCWSTR::null(), &HSTRING::from(data.as_os_str()), &ICoreWebView2EnvironmentOptions::from(options), &handler) };
    if let Err(e) = r {
        fail(format!("WebView2 runtime unavailable: {e}"));
    }
}

/// Show `site` at `url` (background colour 0xRRGGBB until the page paints).
pub fn show(parent: HWND, wake_msg: u32, site: Site, url: String, bg: u32) {
    prewarm(parent, wake_msg);
    // The same page is loaded or loading (parked, preloaded): hand it the new
    // content. A page still loading gets it when it reports ready.
    let (reused, loaded) = WEB.with(|w| {
        let mut w = w.borrow_mut();
        let w = w.as_mut().unwrap();
        let same = w.state == State::Ready && w.parked && w.current.as_deref() == Some(url.as_str()) && site.reuse.is_some();
        w.inbox.lock().clear();
        if same {
            *w.site.lock() = site.clone();
            w.parked = false;
        }
        (same, w.page_ready.load(std::sync::atomic::Ordering::Acquire))
    });
    if reused && !loaded {
        return;
    }
    if reused {
        if let (Some(wv), Some(h)) = (webview(), site.hello.as_ref()) {
            unsafe {
                let _ = wv.PostWebMessageAsJson(&HSTRING::from(h.as_str()));
            }
        }
        return;
    }
    let ready = WEB.with(|w| {
        let mut w = w.borrow_mut();
        let w = w.as_mut().unwrap();
        *w.site.lock() = site.clone();
        *w.loaded.lock() = false;
        w.parked = false;
        w.current = Some(url.clone());
        if w.state != State::Ready {
            w.pending = Some((url.clone(), bg));
        }
        w.state == State::Ready
    });
    if ready {
        if let Some(wv) = webview() {
            unsafe {
                if let Ok(s) = wv.Settings() {
                    let _ = s.SetIsScriptEnabled(site.scripts);
                }
            }
        }
        navigate(&url, bg);
    }
}

fn webview() -> Option<ICoreWebView2> {
    WEB.with(|w| w.borrow().as_ref().and_then(|w| w.webview.clone()))
}

fn controller() -> Option<ICoreWebView2Controller> {
    WEB.with(|w| w.borrow().as_ref().and_then(|w| w.controller.clone()))
}

fn navigate(url: &str, bg: u32) {
    let (Some(wv), Some(c)) = (webview(), controller()) else { return };
    WEB.with(|w| {
        if let Some(w) = w.borrow().as_ref() {
            w.page_ready.store(false, std::sync::atomic::Ordering::Release);
        }
    });
    let (scripts, messages) = WEB.with(|w| {
        w.borrow()
            .as_ref()
            .map(|w| {
                let s = w.site.lock();
                (s.scripts, s.messages)
            })
            .unwrap_or((false, false))
    });
    unsafe {
        if let Ok(s) = wv.Settings() {
            let _ = s.SetIsScriptEnabled(scripts);
            let _ = s.SetIsWebMessageEnabled(messages);
        }
        if let Ok(c2) = c.cast::<ICoreWebView2Controller2>() {
            let _ = c2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR { A: 255, R: (bg >> 16) as u8, G: (bg >> 8) as u8, B: bg as u8 });
        }
        let _ = c.SetZoomFactor(1.0);
        let _ = wv.Navigate(&HSTRING::from(url));
    }
}

/// Position the page (client pixels of the preview window); shown once loaded.
pub fn place(rect: RECT) {
    let Some(c) = controller() else { return };
    WEB.with(|w| {
        let mut w = w.borrow_mut();
        let Some(w) = w.as_mut() else { return };
        let loaded = *w.loaded.lock();
        if rect != w.bounds {
            w.bounds = rect;
            unsafe {
                let _ = c.SetBounds(rect);
            }
        }
        let want = loaded && w.current.is_some() && !w.parked;
        if want != w.visible {
            w.visible = want;
            unsafe {
                let _ = c.SetIsVisible(want);
            }
        }
    });
}

/// Is the page on screen (the UI layer must leave its area clear)?
pub fn showing() -> bool {
    WEB.with(|w| w.borrow().as_ref().is_some_and(|w| !w.parked && (w.visible || (*w.loaded.lock() && w.current.is_some()))))
}

pub fn error() -> Option<String> {
    WEB.with(|w| w.borrow().as_ref().and_then(|w| (w.state == State::Failed).then(|| w.error.clone())))
}

/// Stop showing anything (sound off, page released). A reusable page that
/// finished loading stays loaded, hidden, and is told to drop its content.
pub fn hide() {
    let c = controller();
    let wv = webview();
    let park = WEB.with(|w| {
        let mut w = w.borrow_mut();
        let Some(w) = w.as_mut() else { return None };
        let msg = w.site.lock().reuse.clone();
        let msg = msg.filter(|_| *w.loaded.lock() && w.current.is_some() && w.state == State::Ready);
        if msg.is_some() {
            w.parked = true;
            w.visible = false;
            w.pending = None;
            w.inbox.lock().clear();
        }
        msg
    });
    if let Some(m) = park {
        unsafe {
            if let Some(c) = c {
                let _ = c.SetIsVisible(false);
            }
            if let Some(wv) = wv {
                let _ = wv.PostWebMessageAsJson(&HSTRING::from(m));
            }
        }
        return;
    }
    WEB.with(|w| {
        if let Some(w) = w.borrow_mut().as_mut() {
            w.current = None;
            w.pending = None;
            w.visible = false;
            *w.loaded.lock() = false;
            *w.site.lock() = Site::default();
        }
    });
    unsafe {
        if let Some(c) = c {
            let _ = c.SetIsVisible(false);
        }
        if let Some(wv) = wv {
            let _ = wv.Navigate(&HSTRING::from("about:blank"));
        }
    }
}

/// Close WebView2 (the previewer has been idle); the next web file starts it again.
pub fn release() {
    let web = WEB.with(|w| w.borrow_mut().take());
    if let Some(w) = web {
        if let Some(c) = w.controller {
            unsafe {
                let _ = c.Close();
            }
        }
        tracing::info!("WebView2 released");
    }
    WANT.store(false, std::sync::atomic::Ordering::Release);
    PRELOAD.lock().take();
}

/// Messages the page posted for the view since the last call.
pub fn take_messages() -> Vec<String> {
    WEB.with(|w| w.borrow().as_ref().map(|w| std::mem::take(&mut *w.inbox.lock())).unwrap_or_default())
}

/// Post a JSON message to the page.
pub fn post(json: &str) {
    if let Some(wv) = webview() {
        unsafe {
            let _ = wv.PostWebMessageAsJson(&HSTRING::from(json));
        }
    }
}

/// Give the page keyboard focus (after a click into it).
pub fn focus() {
    if let Some(c) = controller() {
        unsafe {
            let _ = c.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_round_trip() {
        let p = Path::new(r"C:\Users\me\My Site\index é.html");
        let u = url_for(p);
        assert_eq!(u, "https://gt.localhost/C/Users/me/My%20Site/index%20%C3%A9.html");
        assert_eq!(path_for(&u).unwrap(), PathBuf::from(r"C:\Users\me\My Site\index é.html"));
        assert_eq!(path_for("https://gt.localhost/C/a/b/../c.png?x=1").unwrap(), PathBuf::from(r"C:\a\c.png"));
        assert!(path_for("https://gt.localhost/__page__").is_none());
    }
}

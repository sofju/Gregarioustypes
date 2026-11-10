//! The settings window (`--settings`, started from the tray): a normal window
//! hosting its own WebView2 with an embedded page. Every change is saved to
//! settings.toml at once; the agent is told, and it updates itself and the viewer.

use gt_core::settings::Settings;
use std::cell::RefCell;
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::*;
use windows::core::{w, Interface, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{SHCreateMemStream, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::*;

const PAGE: &str = include_str!("../assets/settings.html");
const URL: &str = "https://gt.localhost/settings";
pub const CLASS: PCWSTR = w!("Gregarioustypes.Settings");
/// Posted to the agent's window when settings.toml changed.
pub const CHANGED_MESSAGE: PCWSTR = w!("Gregarioustypes.SettingsChanged");

thread_local! {
    static CONTROLLER: RefCell<Option<ICoreWebView2Controller>> = const { RefCell::new(None) };
}

/// Engines offered per file type: provider id and description.
const ENGINES: &[(&str, &str)] = &[
    ("text", "Text (source)"),
    ("card", "File information only"),
    ("image", "Image"),
    ("media", "Video and audio player"),
    ("html", "Web page"),
    ("markdown", "Markdown, rendered"),
    ("pdf", "PDF"),
    ("svg", "SVG drawing"),
    ("postscript", "PostScript and EPS"),
    ("office", "Office document (LibreOffice)"),
    ("sheet", "Spreadsheet grid"),
    ("rtf", "Rich text"),
    ("archive", "Archive or disc image"),
    ("nfo", "DOS text and art (NFO)"),
    ("torrent", "Torrent"),
    ("playlist", "Playlist"),
    ("gltf", "3D model (glTF)"),
    ("usd", "3D model (USD)"),
    ("fbx", "3D model (FBX, OBJ)"),
    ("swf", "Flash (Ruffle)"),
];

/// Bundled components: name, folder under deps (None: built in), version.
const COMPONENTS: &[(&str, Option<&str>, &str)] = &[
    ("mpv and FFmpeg", Some("mpv"), "shinchiro build 2026-11-07"),
    ("MediaInfo", Some("mediainfo"), "26.10"),
    ("PDFium", Some("pdfium"), "Chromium 8138"),
    ("Ghostscript", Some("ghostscript"), "10.08.0"),
    ("LibreOffice", Some("libreoffice"), "26.8.2"),
    ("7-Zip", Some("7zip"), "26.06"),
    ("Blender", Some("blender"), "5.2.3 LTS"),
    ("three.js", Some("three"), "r187"),
    ("Ruffle", Some("ruffle"), "nightly 2026-11-08"),
    ("GeneralUser GS SoundFont", Some("soundfont"), "2.0"),
    ("Ultimate Oldschool PC Font Pack", Some("fonts"), "2.2"),
    ("ufbx", None, "0.23"),
    ("syntect, with bat's syntax collection", None, "5.3 · bat 0.26"),
    ("Alembic", None, "1.8"),
    ("libjpeg-turbo, libheif, libavif, libjxl, LibRaw, OpenEXR…", None, "vcpkg"),
];

fn system_dark() -> bool {
    let mut v: u32 = 1;
    let mut n = 4u32;
    unsafe {
        let _ = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut n),
        );
    }
    v == 0
}

fn open(p: &std::path::Path) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(p.as_os_str()), PCWSTR::null(), PCWSTR::null(), windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL);
    }
}

/// Tell the agent (it updates the hook, autostart and the viewer).
fn notify_agent() {
    unsafe {
        let msg = RegisterWindowMessageW(CHANGED_MESSAGE);
        if let Ok(h) = FindWindowW(w!("Gregarioustypes.Agent"), PCWSTR::null()) {
            let _ = PostMessageW(Some(h), msg, WPARAM(0), LPARAM(0));
        }
    }
}

fn reply(wv: &ICoreWebView2, v: serde_json::Value) {
    unsafe {
        let _ = wv.PostWebMessageAsJson(&HSTRING::from(v.to_string()));
    }
}

fn cache_text() -> String {
    gt_core::fmt::size(gt_core::cache::size())
}

fn on_message(wv: &ICoreWebView2, json: &str) {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    if v["get"].as_bool() == Some(true) {
        let s = Settings::load();
        let deps = gt_core::paths::deps_dir();
        let comps: Vec<(String, String)> = COMPONENTS
            .iter()
            .map(|(n, dir, ver)| {
                let present = dir.is_none_or(|d| deps.join(d).exists());
                (n.to_string(), if present { ver.to_string() } else { "not installed".into() })
            })
            .collect();
        let engines: Vec<(&str, &str)> = ENGINES.to_vec();
        reply(wv, serde_json::json!({ "settings": serde_json::to_value(&s).unwrap_or_default(), "engines": engines }));
        reply(wv, serde_json::json!({ "info": { "version": env!("CARGO_PKG_VERSION"), "config": Settings::path().display().to_string(), "components": comps } }));
        reply(wv, serde_json::json!({ "cache": cache_text() }));
    } else if let Some(s) = v.get("set") {
        match serde_json::from_value::<Settings>(s.clone()) {
            Ok(s) => {
                s.save();
                notify_agent();
            }
            Err(e) => tracing::warn!("settings page sent invalid settings: {e}"),
        }
    } else if let Some(a) = v["action"].as_str() {
        match a {
            "clear-cache" => {
                let freed = gt_core::cache::clear();
                tracing::info!("cache cleared: {}", gt_core::fmt::size(freed));
                reply(wv, serde_json::json!({ "cache": cache_text() }));
            }
            "open-logs" => open(&gt_core::paths::log_dir()),
            "open-config" => open(&Settings::path()),
            _ => {}
        }
    }
}

fn setup(env: ICoreWebView2Environment, controller: ICoreWebView2Controller, hwnd: HWND, section: &str) -> windows::core::Result<()> {
    let webview = unsafe { controller.CoreWebView2()? };
    unsafe {
        let s = webview.Settings()?;
        s.SetAreDevToolsEnabled(false)?;
        s.SetIsStatusBarEnabled(false)?;
        s.SetIsZoomControlEnabled(false)?;
        s.SetAreHostObjectsAllowed(false)?;
        s.SetIsWebMessageEnabled(true)?;
        if let Ok(s3) = s.cast::<ICoreWebView2Settings3>() {
            let _ = s3.SetAreBrowserAcceleratorKeysEnabled(false);
        }
        if let Ok(c2) = controller.cast::<ICoreWebView2Controller2>() {
            let (r, g, b) = if system_dark() { (32, 32, 32) } else { (243, 243, 243) };
            let _ = c2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR { A: 255, R: r, G: g, B: b });
        }
        webview.AddWebResourceRequestedFilter(&HSTRING::from("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)?;
    }
    let mut token = 0i64;
    let handler = WebResourceRequestedEventHandler::create(Box::new(move |_wv, args| {
        let Some(a) = args else { return Ok(()) };
        unsafe {
            let req = a.Request()?;
            let mut p = PWSTR::null();
            req.Uri(&mut p)?;
            let uri = p.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
            let (stream, status, ctype) = if uri.split('#').next() == Some(URL) { (SHCreateMemStream(Some(PAGE.as_bytes())), 200, "text/html; charset=utf-8") } else { (None, 404, "text/plain") };
            let headers = HSTRING::from(format!("Content-Type: {ctype}"));
            if let Ok(r) = env.CreateWebResourceResponse(stream.as_ref(), status, &HSTRING::from(if status == 200 { "OK" } else { "Not Found" }), &headers) {
                let _ = a.SetResponse(&r);
            }
        }
        Ok(())
    }));
    unsafe { webview.add_WebResourceRequested(&handler, &mut token)? };
    // Nothing leaves the page.
    let starting = NavigationStartingEventHandler::create(Box::new(move |_wv, args| {
        if let Some(a) = args {
            unsafe {
                let mut p = PWSTR::null();
                a.Uri(&mut p)?;
                let uri = p.to_string().unwrap_or_default();
                windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
                if uri.split('#').next() != Some(URL) {
                    a.SetCancel(true)?;
                }
            }
        }
        Ok(())
    }));
    unsafe { webview.add_NavigationStarting(&starting, &mut token)? };
    let received = WebMessageReceivedEventHandler::create(Box::new(move |wv, args| {
        if let (Some(wv), Some(a)) = (wv, args) {
            unsafe {
                let mut p = PWSTR::null();
                if a.WebMessageAsJson(&mut p).is_ok() {
                    let json = p.to_string().unwrap_or_default();
                    windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
                    on_message(&wv, &json);
                }
            }
        }
        Ok(())
    }));
    unsafe { webview.add_WebMessageReceived(&received, &mut token)? };
    unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        controller.SetBounds(rc)?;
        controller.SetIsVisible(true)?;
        let url = if section.is_empty() { URL.to_string() } else { format!("{URL}#{section}") };
        webview.Navigate(&HSTRING::from(url))?;
    }
    CONTROLLER.with(|c| *c.borrow_mut() = Some(controller));
    Ok(())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_SIZE => {
            CONTROLLER.with(|c| {
                if let Some(c) = c.borrow().as_ref() {
                    let mut rc = RECT::default();
                    unsafe {
                        let _ = GetClientRect(hwnd, &mut rc);
                        let _ = c.SetBounds(rc);
                    }
                }
            });
            LRESULT(0)
        }
        WM_SETFOCUS => {
            CONTROLLER.with(|c| {
                if let Some(c) = c.borrow().as_ref() {
                    unsafe {
                        let _ = c.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
                    }
                }
            });
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let r = unsafe { &*(lp.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let mmi = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
            let k = unsafe { GetDpiForWindow(hwnd) }.max(96) as i32;
            mmi.ptMinTrackSize.x = 640 * k / 96;
            mmi.ptMinTrackSize.y = 460 * k / 96;
            LRESULT(0)
        }
        WM_DESTROY => {
            CONTROLLER.with(|c| {
                if let Some(c) = c.borrow_mut().take() {
                    unsafe {
                        let _ = c.Close();
                    }
                }
            });
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// `section`: "general", "formats", "display", "text", "engines" or "about".
pub fn run(section: &str) {
    let section = section.to_string();
    gt_core::log::init("settings");
    unsafe {
        // One settings window: bring the open one forward.
        if let Ok(h) = FindWindowW(CLASS, PCWSTR::null()) {
            let _ = ShowWindow(h, SW_RESTORE);
            let _ = SetForegroundWindow(h);
            return;
        }
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let icon = LoadImageW(Some(hinst.into()), PCWSTR(1 as _), IMAGE_ICON, 0, 0, LR_DEFAULTSIZE).map(|h| HICON(h.0)).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: CLASS,
            hIcon: icon,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // 980 × 700 DIP, centred on the primary monitor's work area.
        let mut work = RECT::default();
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut RECT as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        let dpi = windows::Win32::UI::HiDpi::GetDpiForSystem().max(96) as i32;
        let (w, h) = ((980 * dpi / 96).min(work.right - work.left), (700 * dpi / 96).min(work.bottom - work.top));
        let (x, y) = (work.left + (work.right - work.left - w) / 2, work.top + (work.bottom - work.top - h) / 2);
        let Ok(hwnd) = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!("Gregarioustypes settings"), WS_OVERLAPPEDWINDOW, x, y, w, h, None, None, Some(hinst.into()), None) else {
            return;
        };
        let dark: windows::core::BOOL = system_dark().into();
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
        let mica: i32 = 2;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &mica as *const _ as *const _, 4);
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let data = gt_core::paths::cache_dir().join("webview2-settings");
        let created = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |hr, env| {
            let Some(env) = env.filter(|_| hr.is_ok()) else {
                tracing::error!("WebView2 environment: {hr:?}");
                return Ok(());
            };
            let env2 = env.clone();
            let done = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |hr, c| {
                match c.filter(|_| hr.is_ok()) {
                    Some(c) => {
                        if let Err(e) = setup(env2.clone(), c, hwnd, &section) {
                            tracing::error!("settings page: {e}");
                        }
                    }
                    None => tracing::error!("WebView2 controller: {hr:?}"),
                }
                Ok(())
            }));
            env.CreateCoreWebView2Controller(hwnd, &done)?;
            Ok(())
        }));
        if let Err(e) = CreateCoreWebView2EnvironmentWithOptions(PCWSTR::null(), &HSTRING::from(data.as_os_str()), None, &created) {
            tracing::error!("WebView2 unavailable: {e}");
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

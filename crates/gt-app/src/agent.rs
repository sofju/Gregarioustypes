//! The always-running agent: tray icon, Space hook, Explorer integration, and the
//! supervisor that keeps a warm viewer process alive (and restarts it after a crash).

use gt_core::settings::Settings;
use gt_ipc::{ServerPipes, ToAgent, ToViewer};
use gt_shell::{explorer, foreground, hook};
use parking_lot::Mutex;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::{Shell_NotifyIconW, ShellExecuteW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICON_VERSION_4};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, LoadImageW,
    PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, TrackPopupMenu, TranslateMessage, HICON, IMAGE_ICON,
    LR_DEFAULTCOLOR, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, SM_CXSMICON, SM_CYSMICON, SW_SHOWNORMAL, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE,
    WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONDBLCLK, WM_NULL, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
};

const WM_TRAY: u32 = WM_APP + 1;

/// Shared agent state.
struct Agent {
    sender: Mutex<Option<gt_ipc::Sender>>,
    viewer_pid: AtomicU32,
    viewer_hwnd: AtomicIsize,
    pinned: AtomicBool,
    loading: Mutex<Option<String>>,
    visible_since_load: AtomicBool,
    quitting: AtomicBool,
    explorer_tid: AtomicU32,
    settings: Mutex<Settings>,
}

static AGENT: OnceLock<Arc<Agent>> = OnceLock::new();

fn agent() -> &'static Arc<Agent> {
    AGENT.get().expect("agent")
}

impl Agent {
    fn send(&self, msg: ToViewer) {
        if let Some(s) = self.sender.lock().as_ref() {
            if let Err(e) = s.send(&msg) {
                tracing::warn!("send to viewer failed: {e}");
            }
        }
    }
}

pub fn run() {
    gt_core::log::init("agent");
    unsafe {
        let _mutex = CreateMutexW(None, true, w!("Local\\Gregarioustypes.Agent"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            tracing::info!("agent already running");
            return;
        }
        std::mem::forget(_mutex);
    }
    let settings = Settings::load();
    apply_settings(&settings);
    let a = Arc::new(Agent {
        sender: Mutex::new(None),
        viewer_pid: AtomicU32::new(0),
        viewer_hwnd: AtomicIsize::new(0),
        pinned: AtomicBool::new(false),
        loading: Mutex::new(None),
        visible_since_load: AtomicBool::new(false),
        quitting: AtomicBool::new(false),
        explorer_tid: AtomicU32::new(0),
        settings: Mutex::new(settings),
    });
    let _ = AGENT.set(a.clone());

    let tid = explorer::spawn(Box::new(|msg| agent().send(msg)));
    a.explorer_tid.store(tid, Ordering::Release);
    hook::start(tid);
    std::thread::Builder::new().name("gt-supervisor".into()).spawn(supervise).expect("supervisor");
    tray_loop();
}

/// Hook switches and autostart follow the settings.
fn apply_settings(s: &Settings) {
    hook::ENABLED.store(s.enabled, Ordering::Release);
    hook::ALT_SPACE.store(s.alt_space_fullscreen, Ordering::Release);
    hook::FILE_DIALOGS.store(s.file_dialogs, Ordering::Release);
    if gt_shell::autostart::is_set() != s.start_with_windows {
        gt_shell::autostart::set(s.start_with_windows);
    }
}

/// The settings window saved a change.
fn reload_settings() {
    let a = agent();
    let s = Settings::load();
    apply_settings(&s);
    *a.settings.lock() = s;
    a.send(ToViewer::Command("reload-settings".into()));
    tracing::info!("settings reloaded");
}

fn open_settings() {
    if let Ok(exe) = std::env::current_exe() {
        if let Err(e) = Command::new(exe).arg("--settings").spawn() {
            tracing::warn!("cannot open settings: {e}");
        }
    }
}

/// Keep one viewer process alive and pump its messages.
fn supervise() {
    let exe = std::env::current_exe().expect("exe path");
    let mut generation = 0u32;
    let mut recent: Vec<Instant> = Vec::new();
    let mut crashed: Option<String> = None;
    while !agent().quitting.load(Ordering::Acquire) {
        generation += 1;
        let tag = format!("{}.{generation}", std::process::id());
        let pipes = match ServerPipes::create(&tag) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("cannot create pipes: {e}");
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        let mut child = match Command::new(&exe).args(["--viewer", "--tag", &tag]).spawn() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("cannot start viewer: {e}");
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        let Some((sender, receiver)) = accept_or_exit(pipes, &mut child, &tag) else {
            tracing::warn!("viewer exited before connecting");
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };
        *agent().sender.lock() = Some(sender);
        tracing::info!("viewer {} connected", child.id());
        if let Some(path) = crashed.take() {
            agent().send(ToViewer::Show {
                items: vec![gt_ipc::Item { path: path.clone(), filesystem: true }],
                index: 0,
                fullscreen: false,
                anchor: None,
                monitor: None,
                crashed: Some(path),
            });
        }
        while let Ok(msg) = receiver.recv::<ToAgent>() {
            handle(msg);
        }
        *agent().sender.lock() = None;
        let was_visible = hook::VIEWER_VISIBLE.swap(false, Ordering::AcqRel);
        hook::VIEWER_MULTI.store(false, Ordering::Release);
        let status = child.wait().ok();
        let clean = status.map(|s| s.success()).unwrap_or(false);
        if !clean && !agent().quitting.load(Ordering::Acquire) {
            tracing::error!("viewer crashed ({status:?})");
            if was_visible || agent().visible_since_load.load(Ordering::Acquire) {
                crashed = agent().loading.lock().clone();
            }
        }
        recent.retain(|t| t.elapsed() < Duration::from_secs(20));
        recent.push(Instant::now());
        if recent.len() > 4 {
            tracing::error!("viewer restarting too often; backing off");
            std::thread::sleep(Duration::from_secs(5));
        }
    }
}

fn accept_or_exit(pipes: ServerPipes, child: &mut Child, tag: &str) -> Option<(gt_ipc::Sender, gt_ipc::Receiver)> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(pipes.accept());
    });
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(c)) => return Some(c),
            Ok(Err(e)) => {
                tracing::warn!("accept failed: {e}");
                return None;
            }
            Err(_) => {
                if let Ok(Some(_)) = child.try_wait() {
                    // Unblock ConnectNamedPipe by connecting to ourselves, then drop it.
                    let _ = gt_ipc::connect(tag);
                    let _ = rx.recv_timeout(Duration::from_secs(1));
                    return None;
                }
            }
        }
    }
}

fn handle(msg: ToAgent) {
    let a = agent();
    match msg {
        ToAgent::Ready { pid } => a.viewer_pid.store(pid, Ordering::Release),
        ToAgent::Visible { visible, multi, pinned, hwnd } => {
            hook::VIEWER_VISIBLE.store(visible, Ordering::Release);
            hook::VIEWER_MULTI.store(multi && visible, Ordering::Release);
            a.pinned.store(pinned, Ordering::Release);
            a.viewer_hwnd.store(hwnd, Ordering::Release);
            a.visible_since_load.store(visible, Ordering::Release);
            if !visible {
                explorer::post(a.explorer_tid.load(Ordering::Acquire), explorer::WM_GT_UNTRACK, 0);
            }
        }
        ToAgent::Navigate(d) => explorer::post(a.explorer_tid.load(Ordering::Acquire), explorer::WM_GT_NAVIGATE, d as isize),
        ToAgent::Loading { path } => *a.loading.lock() = Some(path),
    }
}

/// Foreground changed: Quick Look hides when you switch to another app.
fn on_foreground(hwnd: HWND) {
    let a = agent();
    if !hook::VIEWER_VISIBLE.load(Ordering::Acquire) || a.pinned.load(Ordering::Acquire) || !a.settings.lock().hide_on_focus_loss {
        return;
    }
    let pid = foreground::process_id(hwnd);
    if pid == 0 || pid == a.viewer_pid.load(Ordering::Acquire) || foreground::is_explorer_process(pid) {
        return;
    }
    tracing::info!("foreground moved to pid {pid} ({}); hiding preview", foreground::process_image(pid).unwrap_or_default());
    a.send(ToViewer::Hide);
}

// ---------------------------------------------------------------- tray

const ID_ENABLED: usize = 1;
const ID_AUTOSTART: usize = 2;
const ID_HDR: usize = 3;
const ID_SETTINGS: usize = 4;
const ID_LOGS: usize = 5;
const ID_RESTART: usize = 6;
const ID_EXIT: usize = 7;

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static SETTINGS_CHANGED: AtomicU32 = AtomicU32::new(0);

fn tray_icon() -> HICON {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let (cx, cy) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        LoadImageW(Some(hinst.into()), PCWSTR(1 as _), IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR).map(|h| HICON(h.0)).unwrap_or_default()
    }
}

fn notify_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: 1, ..Default::default() };
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = tray_icon();
    let tip: Vec<u16> = "Gregarioustypes — press Space in Explorer".encode_utf16().collect();
    nid.szTip[..tip.len()].copy_from_slice(&tip);
    nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    nid
}

fn add_tray(hwnd: HWND) {
    unsafe {
        let nid = notify_data(hwnd);
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
    }
}

fn open_path(p: &std::path::Path) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(p.as_os_str()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

fn show_menu(hwnd: HWND) {
    let a = agent();
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let enabled = hook::ENABLED.load(Ordering::Acquire);
        let autostart = gt_shell::autostart::is_set();
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w!("Gregarioustypes 0.1"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING | if enabled { MF_CHECKED } else { MF_STRING }, ID_ENABLED, w!("&Enabled (Space previews)"));
        let _ = AppendMenuW(menu, MF_STRING | if autostart { MF_CHECKED } else { MF_STRING }, ID_AUTOSTART, w!("Start with &Windows"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_HDR, w!("Show &HDR test pattern"));
        let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS, w!("&Settings…"));
        let _ = AppendMenuW(menu, MF_STRING, ID_LOGS, w!("Open &log folder"));
        let _ = AppendMenuW(menu, MF_STRING, ID_RESTART, w!("&Restart previewer"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, w!("E&xit"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None).0 as usize;
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match cmd {
            ID_ENABLED => {
                let mut s = a.settings.lock();
                s.enabled = !enabled;
                s.save();
                apply_settings(&s);
            }
            ID_AUTOSTART => {
                let mut s = a.settings.lock();
                s.start_with_windows = !autostart;
                s.save();
                apply_settings(&s);
            }
            ID_HDR => a.send(ToViewer::Command("hdr-test".into())),
            ID_SETTINGS => open_settings(),
            ID_LOGS => open_path(&gt_core::paths::log_dir()),
            ID_RESTART => a.send(ToViewer::Quit),
            ID_EXIT => {
                a.quitting.store(true, Ordering::Release);
                a.send(ToViewer::Quit);
                PostQuitMessage(0);
            }
            _ => {}
        }
    }
}

unsafe extern "system" fn tray_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            let ev = (lp.0 & 0xFFFF) as u32;
            if ev == WM_LBUTTONDBLCLK {
                open_settings();
            } else if ev == WM_RBUTTONUP || ev == WM_CONTEXTMENU || ev == 0x0400 {
                show_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_COMMAND => LRESULT(0),
        WM_DESTROY => {
            unsafe {
                let nid = notify_data(hwnd);
                let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            add_tray(hwnd);
            LRESULT(0)
        }
        m if m != 0 && m == SETTINGS_CHANGED.load(Ordering::Relaxed) => {
            reload_settings();
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn tray_loop() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("Gregarioustypes.Agent");
        let wc = WNDCLASSW { lpfnWndProc: Some(tray_proc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);
        SETTINGS_CHANGED.store(RegisterWindowMessageW(crate::settings_ui::CHANGED_MESSAGE), Ordering::Relaxed);
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("Gregarioustypes"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, Some(hinst.into()), None).expect("tray window");
        add_tray(hwnd);
        foreground::start(Box::new(on_foreground));
        tracing::info!("agent ready");
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let nid = notify_data(hwnd);
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        agent().quitting.store(true, Ordering::Release);
        agent().send(ToViewer::Quit);
        std::thread::sleep(Duration::from_millis(200));
    }
}

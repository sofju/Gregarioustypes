//! Low-level keyboard hook. Runs on its own thread and does a few microseconds of
//! work per key: it only decides whether to swallow Space/Esc/arrows and posts a
//! message to the Explorer worker thread.

use crate::util::{class_name, parent_class};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_LWIN, VK_RIGHT, VK_RWIN, VK_SHIFT, VK_SPACE, VK_UP};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetGUIThreadInfo, GetMessageW, GetWindowThreadProcessId, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, GUITHREADINFO, GUI_INMENUMODE, GUI_POPUPMENUMODE, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN,
    LLKHF_UP, MSG, WH_KEYBOARD_LL, WM_APP,
};

pub const WM_GT_TOGGLE: u32 = WM_APP + 10; // wparam = foreground hwnd, lparam = 1 for full screen
pub const WM_GT_CLOSE: u32 = WM_APP + 11;
pub const WM_GT_STEP: u32 = WM_APP + 12; // lparam = delta
pub const WM_GT_FULLSCREEN: u32 = WM_APP + 13;

/// Master switch (tray "Enabled").
pub static ENABLED: AtomicBool = AtomicBool::new(true);
/// Alt+Space opens in full screen.
pub static ALT_SPACE: AtomicBool = AtomicBool::new(true);
/// Space also previews in other apps' Open/Save dialogs.
pub static FILE_DIALOGS: AtomicBool = AtomicBool::new(true);
/// The file dialog the preview follows (0: none).
pub static DIALOG: AtomicIsize = AtomicIsize::new(0);
/// Viewer state mirrored from IPC.
pub static VIEWER_VISIBLE: AtomicBool = AtomicBool::new(false);
pub static VIEWER_MULTI: AtomicBool = AtomicBool::new(false);

static TARGET_THREAD: AtomicU32 = AtomicU32::new(0);
static SPACE_SWALLOWED: AtomicBool = AtomicBool::new(false);
static ESC_SWALLOWED: AtomicBool = AtomicBool::new(false);
static ARROW_SWALLOWED: AtomicU32 = AtomicU32::new(0);
static LAST_TYPEAHEAD: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellSurface {
    Explorer,
    Desktop,
    /// Another app's Open/Save dialog.
    Dialog,
}

/// Is this top-level window an Explorer folder window, the desktop, or the
/// file dialog the preview is following?
pub fn shell_surface(fg: HWND) -> Option<ShellSurface> {
    match class_name(fg).as_str() {
        "CabinetWClass" | "ExploreWClass" => Some(ShellSurface::Explorer),
        "Progman" | "WorkerW" => Some(ShellSurface::Desktop),
        "#32770" if fg.0 as isize == DIALOG.load(Ordering::Acquire) => Some(ShellSurface::Dialog),
        _ => None,
    }
}

/// The foreground window is Explorer/desktop *and* keyboard focus is in the item view
/// (not the rename box, address bar, search box or navigation pane).
pub fn item_view_focused(fg: HWND) -> Option<ShellSurface> {
    let surface = match shell_surface(fg) {
        Some(s) => s,
        None if FILE_DIALOGS.load(Ordering::Relaxed) && class_name(fg) == "#32770" => ShellSurface::Dialog,
        None => return None,
    };
    let tid = unsafe { GetWindowThreadProcessId(fg, None) };
    let mut gti = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
    unsafe { GetGUIThreadInfo(tid, &mut gti) }.ok()?;
    if (gti.flags.0 & (GUI_INMENUMODE.0 | GUI_POPUPMENUMODE.0)) != 0 || gti.hwndFocus.is_invalid() {
        return None;
    }
    let fc = class_name(gti.hwndFocus);
    let ok = match surface {
        ShellSurface::Explorer | ShellSurface::Dialog => fc == "DirectUIHWND",
        ShellSurface::Desktop => fc == "SysListView32",
    };
    (ok && parent_class(gti.hwndFocus) == "SHELLDLL_DefView").then_some(surface)
}

fn key_down(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 }
}

fn post(msg: u32, w: usize, l: isize) {
    let tid = TARGET_THREAD.load(Ordering::Acquire);
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, msg, WPARAM(w), LPARAM(l));
        }
    }
}

/// Returns true when the key must be swallowed.
fn handle(vk: u16, up: bool, alt: bool) -> bool {
    let now = unsafe { GetTickCount64() };
    match vk {
        v if v == VK_SPACE.0 => {
            if up {
                return SPACE_SWALLOWED.swap(false, Ordering::AcqRel);
            }
            if SPACE_SWALLOWED.load(Ordering::Acquire) {
                return true; // auto-repeat of a swallowed press
            }
            if !ENABLED.load(Ordering::Relaxed) || key_down(VK_CONTROL.0) || key_down(VK_SHIFT.0) || key_down(VK_LWIN.0) || key_down(VK_RWIN.0) {
                return false;
            }
            if alt && !ALT_SPACE.load(Ordering::Relaxed) {
                return false;
            }
            let fg = unsafe { GetForegroundWindow() };
            if VIEWER_VISIBLE.load(Ordering::Acquire) {
                if shell_surface(fg).is_some() {
                    post(if alt { WM_GT_FULLSCREEN } else { WM_GT_CLOSE }, 0, 0);
                    SPACE_SWALLOWED.store(true, Ordering::Release);
                    return true;
                }
                return false; // the viewer (or another app) handles it
            }
            // Explorer type-ahead: a space typed right after letters belongs to the search.
            if now.saturating_sub(LAST_TYPEAHEAD.load(Ordering::Relaxed)) < 700 {
                return false;
            }
            if item_view_focused(fg).is_some() {
                post(WM_GT_TOGGLE, fg.0 as usize, alt as isize);
                SPACE_SWALLOWED.store(true, Ordering::Release);
                return true;
            }
            false
        }
        v if v == VK_ESCAPE.0 => {
            if up {
                return ESC_SWALLOWED.swap(false, Ordering::AcqRel);
            }
            if VIEWER_VISIBLE.load(Ordering::Acquire) && shell_surface(unsafe { GetForegroundWindow() }).is_some() {
                if !ESC_SWALLOWED.swap(true, Ordering::AcqRel) {
                    post(WM_GT_CLOSE, 0, 0);
                }
                return true;
            }
            false
        }
        v if v == VK_LEFT.0 || v == VK_RIGHT.0 || v == VK_UP.0 || v == VK_DOWN.0 => {
            let bit = 1u32 << (v - VK_LEFT.0);
            if up {
                return ARROW_SWALLOWED.fetch_and(!bit, Ordering::AcqRel) & bit != 0;
            }
            // Within a multi-selection the arrows step through the selected items,
            // like Quick Look; otherwise Explorer moves the selection and we follow it.
            if VIEWER_VISIBLE.load(Ordering::Acquire) && VIEWER_MULTI.load(Ordering::Acquire) && shell_surface(unsafe { GetForegroundWindow() }).is_some() {
                ARROW_SWALLOWED.fetch_or(bit, Ordering::AcqRel);
                let delta = if v == VK_LEFT.0 || v == VK_UP.0 { -1 } else { 1 };
                post(WM_GT_STEP, 0, delta);
                return true;
            }
            false
        }
        // Letters and digits feed Explorer's type-ahead search.
        0x30..=0x39 | 0x41..=0x5A | 0x60..=0x69 if !up => {
            if !key_down(VK_CONTROL.0) && !alt {
                LAST_TYPEAHEAD.store(now, Ordering::Relaxed);
            }
            false
        }
        _ => false,
    }
}

unsafe extern "system" fn proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let up = kb.flags.0 & LLKHF_UP.0 != 0;
        let alt = kb.flags.0 & LLKHF_ALTDOWN.0 != 0;
        if handle(kb.vkCode as u16, up, alt) {
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Install the hook on a dedicated thread. Messages go to `target_thread`.
pub fn start(target_thread: u32) {
    TARGET_THREAD.store(target_thread, Ordering::Release);
    std::thread::Builder::new()
        .name("gt-hook".into())
        .spawn(|| unsafe {
            let hmod = GetModuleHandleW(None).ok();
            let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(proc), hmod.map(|m| m.into()), 0) {
                Ok(h) => h,
                Err(e) => {
                    tracing::error!("SetWindowsHookExW failed: {e}");
                    return;
                }
            };
            tracing::info!("keyboard hook installed");
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = UnhookWindowsHookEx(hook);
        })
        .expect("hook thread");
}

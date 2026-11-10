//! Foreground-window tracking (out-of-context WinEvent hook) and process helpers.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::OnceLock;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS};

type Callback = Box<dyn Fn(HWND) + Send + Sync>;
static CALLBACK: OnceLock<Callback> = OnceLock::new();

unsafe extern "system" fn proc(_: HWINEVENTHOOK, event: u32, hwnd: HWND, _obj: i32, _child: i32, _thread: u32, _time: u32) {
    if event == EVENT_SYSTEM_FOREGROUND {
        if let Some(cb) = CALLBACK.get() {
            cb(hwnd);
        }
    }
}

/// Must be called on a thread that pumps messages.
pub fn start(cb: Callback) {
    let _ = CALLBACK.set(cb);
    unsafe {
        SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(proc), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
    }
}

pub fn process_id(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

pub fn process_image(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Is `pid` an explorer.exe process (folder windows may run in several)?
pub fn is_explorer_process(pid: u32) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<u32, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = cache.lock().get(&pid) {
        return *v;
    }
    let v = process_image(pid).map(|p| p.to_ascii_lowercase().ends_with("\\explorer.exe")).unwrap_or(false);
    cache.lock().insert(pid, v);
    v
}

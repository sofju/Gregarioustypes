//! Space in other apps' Open/Save dialogs. The dialog's selection lives in its
//! own process, so `gt_dialoghook.dll` is loaded there by a `WH_GETMESSAGE`
//! hook on the dialog's thread; a posted query message makes it read the
//! selection from the dialog's shell view and write it to shared memory.
//! 32-bit apps get the 32-bit DLL through `gt_dialoghook32.exe`, which installs
//! the hook for them (a 64-bit process can't hook a 32-bit one).
//! Layout of the shared block: see gt-dialoghook.

use crate::util::wide;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, FreeLibrary, HANDLE, HMODULE, HWND, INVALID_HANDLE_VALUE, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Memory::{CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE};
use windows::Win32::System::Threading::{CreateEventW, IsWow64Process2, OpenProcess, ResetEvent, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::System::SystemInformation::IMAGE_FILE_MACHINE_UNKNOWN;
use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, PostMessageW, RegisterWindowMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, HOOKPROC, WH_GETMESSAGE};

const MAGIC: u32 = 0x5344_5447;
const MAP_SIZE: usize = 4 << 20;
const HEADER: usize = 32;

fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_default()
}

enum Hook {
    /// 64-bit app: our own hook.
    Native { hook: HHOOK, dll: HMODULE },
    /// 32-bit app: held by the 32-bit helper until it exits.
    Helper(Child),
}

/// The shared block and its "answer written" event.
struct Shared {
    map: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    event: HANDLE,
}

impl Shared {
    fn new() -> Option<Shared> {
        unsafe {
            let map = CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, 0, MAP_SIZE as u32, w!(r"Local\Gregarioustypes.DialogSelection")).ok()?;
            let view = MapViewOfFile(map, FILE_MAP_ALL_ACCESS, 0, 0, MAP_SIZE);
            let event = CreateEventW(None, false, false, w!(r"Local\Gregarioustypes.DialogSelectionReady")).unwrap_or_default();
            let s = Shared { map, view, event };
            (!view.Value.is_null() && !event.is_invalid()).then_some(s)
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        unsafe {
            if !self.view.Value.is_null() {
                let _ = UnmapViewOfFile(self.view);
            }
            let _ = CloseHandle(self.map);
            if !self.event.is_invalid() {
                let _ = CloseHandle(self.event);
            }
        }
    }
}

/// A live link to one dialog thread (the hook stays installed while it exists).
pub struct Link {
    pub thread: u32,
    hook: Hook,
    shared: Shared,
    query: u32,
    seq: u32,
}

unsafe impl Send for Link {}

fn is_32bit(pid: u32) -> bool {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut process = IMAGE_FILE_MACHINE_UNKNOWN;
        let mut native = IMAGE_FILE_MACHINE_UNKNOWN;
        let ok = IsWow64Process2(h, &mut process, Some(&mut native)).is_ok();
        let _ = CloseHandle(h);
        ok && process != IMAGE_FILE_MACHINE_UNKNOWN
    }
}

fn install(thread: u32, pid: u32) -> Option<Hook> {
    unsafe {
        if is_32bit(pid) {
            let exe = exe_dir().join("gt_dialoghook32.exe");
            return match Command::new(&exe).arg(thread.to_string()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() {
                Ok(mut c) => {
                    // "ok" once its hook is installed (nothing if it failed and exited).
                    let mut buf = [0u8; 3];
                    let ready = c.stdout.as_mut().is_some_and(|o| std::io::Read::read_exact(o, &mut buf).is_ok() && &buf[..2] == b"ok");
                    if !ready {
                        tracing::info!("32-bit dialog helper could not hook thread {thread}");
                        let _ = c.kill();
                        let _ = c.wait();
                        return None;
                    }
                    Some(Hook::Helper(c))
                }
                Err(e) => {
                    tracing::info!("32-bit dialog: {} unavailable ({e})", exe.display());
                    None
                }
            };
        }
        let path = wide(&exe_dir().join("gt_dialoghook.dll").display().to_string());
        let Ok(dll) = LoadLibraryW(PCWSTR(path.as_ptr())) else {
            tracing::warn!("gt_dialoghook.dll not found next to the executable");
            return None;
        };
        let proc: HOOKPROC = std::mem::transmute(GetProcAddress(dll, windows::core::s!("GetMsgProc")));
        match SetWindowsHookExW(WH_GETMESSAGE, proc, Some(dll.into()), thread) {
            Ok(hook) => Some(Hook::Native { hook, dll }),
            Err(e) => {
                tracing::warn!("dialog hook on thread {thread}: {e}");
                let _ = FreeLibrary(dll);
                None
            }
        }
    }
}

impl Link {
    pub fn new(dialog: HWND) -> Option<Link> {
        let mut pid = 0u32;
        let thread = unsafe { GetWindowThreadProcessId(dialog, Some(&mut pid)) };
        if thread == 0 {
            return None;
        }
        // The shared block exists before the hook can answer.
        let shared = Shared::new()?;
        let hook = install(thread, pid)?;
        let query = unsafe { RegisterWindowMessageW(w!("Gregarioustypes.DialogQuery")) };
        Some(Link { thread, hook, shared, query, seq: 0 })
    }

    /// The dialog's folder, selected items (view order) and the focused one's index.
    pub fn query(&mut self, dialog: HWND, timeout_ms: u32) -> Option<(String, Vec<String>, usize)> {
        unsafe {
            self.seq = self.seq.wrapping_add(1).max(1);
            let base = self.shared.view.Value as *mut u32;
            base.write_volatile(0);
            let _ = ResetEvent(self.shared.event);
            PostMessageW(Some(dialog), self.query, WPARAM(self.seq as usize), LPARAM(0)).ok()?;
            if WaitForSingleObject(self.shared.event, timeout_ms) != WAIT_OBJECT_0 {
                return None;
            }
            let h: Vec<u32> = (0..8).map(|i| base.add(i).read_volatile()).collect();
            if h[0] != MAGIC || h[1] != self.seq || h[2] != 1 {
                return None;
            }
            let (count, focused, len) = (h[3] as usize, h[4] as usize, h[5] as usize);
            let data = std::slice::from_raw_parts((self.shared.view.Value as *const u8).add(HEADER) as *const u16, len.min((MAP_SIZE - HEADER) / 2));
            let mut parts = data.split(|c| *c == 0).map(String::from_utf16_lossy);
            let folder = parts.next().unwrap_or_default();
            let items: Vec<String> = parts.take(count).collect();
            Some((folder, items, focused.min(count.saturating_sub(1))))
        }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        unsafe {
            match &mut self.hook {
                Hook::Native { hook, dll } => {
                    let _ = UnhookWindowsHookEx(*hook);
                    let _ = FreeLibrary(*dll);
                }
                Hook::Helper(child) => {
                    // Closing its stdin ends the helper, which removes its hook.
                    drop(child.stdin.take());
                    let _ = child.wait();
                }
            }
        }
    }
}

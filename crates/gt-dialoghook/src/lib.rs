//! Runs inside another app's process (loaded by a `WH_GETMESSAGE` hook on its
//! file dialog's thread). When the agent posts the query message to the dialog,
//! asks the dialog for its shell browser (`WM_GETISHELLBROWSER`), reads the
//! selection from its folder view in view order, writes the paths to the shared
//! memory block and signals the agent.
//!
//! Shared block (`MAP_NAME`): eight u32 — magic, sequence, status, item count,
//! focused index, UTF-16 length, 2 reserved — then UTF-16 strings, each ending
//! with 0: the folder, then every selected item. Must match gt-shell's dialog.rs.

#![allow(clippy::missing_safety_doc)]

use std::sync::atomic::{AtomicU32, Ordering};
use windows::core::{w, Interface};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Memory::{MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_WRITE};
use windows::Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE};
use windows::Win32::UI::Shell::{IFolderView2, IShellBrowser, IShellItem, IShellItemArray, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_FILESYSPATH, SVGIO_FLAG_VIEWORDER, SVGIO_SELECTION};
use windows::Win32::UI::WindowsAndMessaging::{CallNextHookEx, RegisterWindowMessageW, SendMessageW, MSG, PM_REMOVE, WM_NULL, WM_USER};

const MAGIC: u32 = 0x5344_5447; // "GTDS"
const MAP_SIZE: usize = 4 << 20;
const HEADER: usize = 32;
const WM_GETISHELLBROWSER: u32 = WM_USER + 7;

static QUERY: AtomicU32 = AtomicU32::new(0);

fn query_message() -> u32 {
    let q = QUERY.load(Ordering::Relaxed);
    if q != 0 {
        return q;
    }
    let q = unsafe { RegisterWindowMessageW(w!("Gregarioustypes.DialogQuery")) };
    QUERY.store(q, Ordering::Relaxed);
    q
}

unsafe fn path_of(si: &IShellItem) -> Option<Vec<u16>> {
    let p = si.GetDisplayName(SIGDN_FILESYSPATH).or_else(|_| si.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING)).ok()?;
    let v = p.as_wide().to_vec();
    windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
    Some(v)
}

/// (status, folder, items, focused index)
unsafe fn read_selection(dialog: HWND) -> (u32, Vec<u16>, Vec<Vec<u16>>, u32) {
    let raw = SendMessageW(dialog, WM_GETISHELLBROWSER, None, None).0 as *mut core::ffi::c_void;
    let Some(browser) = IShellBrowser::from_raw_borrowed(&raw) else { return (2, Vec::new(), Vec::new(), 0) };
    let Ok(view) = browser.QueryActiveShellView() else { return (3, Vec::new(), Vec::new(), 0) };
    let Ok(fv) = view.cast::<IFolderView2>() else { return (3, Vec::new(), Vec::new(), 0) };
    let folder = fv.GetFolder::<windows::Win32::UI::Shell::IShellItem>().ok().and_then(|f| path_of(&f)).unwrap_or_default();
    let mut items = Vec::new();
    if let Ok(arr) = fv.Items::<IShellItemArray>(SVGIO_SELECTION | SVGIO_FLAG_VIEWORDER) {
        for i in 0..arr.GetCount().unwrap_or(0) {
            if let Some(p) = arr.GetItemAt(i).ok().and_then(|si| path_of(&si)) {
                items.push(p);
            }
        }
    }
    let focused = fv.GetFocusedItem().ok().and_then(|i| fv.GetItem::<IShellItem>(i).ok()).and_then(|si| path_of(&si));
    let index = focused.and_then(|f| items.iter().position(|p| *p == f)).unwrap_or(0) as u32;
    (1, folder, items, index)
}

unsafe fn answer(seq: u32, dialog: HWND) {
    let (status, folder, items, focused) = read_selection(dialog);
    let Ok(map) = OpenFileMappingW(FILE_MAP_WRITE.0, false, w!(r"Local\Gregarioustypes.DialogSelection")) else { return };
    let view = MapViewOfFile(map, FILE_MAP_WRITE, 0, 0, MAP_SIZE);
    if !view.Value.is_null() {
        let base = view.Value as *mut u8;
        let data = base.add(HEADER) as *mut u16;
        let cap = (MAP_SIZE - HEADER) / 2;
        let mut n = 0usize;
        let mut count = 0u32;
        let put = |s: &[u16], n: &mut usize| -> bool {
            if *n + s.len() + 1 > cap {
                return false;
            }
            std::ptr::copy_nonoverlapping(s.as_ptr(), data.add(*n), s.len());
            *data.add(*n + s.len()) = 0;
            *n += s.len() + 1;
            true
        };
        put(&folder, &mut n);
        for it in &items {
            if !put(it, &mut n) {
                break;
            }
            count += 1;
        }
        let h = base as *mut u32;
        for (i, v) in [MAGIC, seq, status, count, focused, n as u32, 0, 0].into_iter().enumerate() {
            h.add(i).write_volatile(v);
        }
        let _ = UnmapViewOfFile(view);
    }
    let _ = CloseHandle(map);
    if let Ok(ev) = OpenEventW(EVENT_MODIFY_STATE, false, w!(r"Local\Gregarioustypes.DialogSelectionReady")) {
        let _ = SetEvent(ev);
        let _ = CloseHandle(ev);
    }
}

/// The hook procedure (`WH_GETMESSAGE`).
#[no_mangle]
pub unsafe extern "system" fn GetMsgProc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam.0 == PM_REMOVE.0 as usize && lparam.0 != 0 {
        let msg = &mut *(lparam.0 as *mut MSG);
        if msg.message != 0 && msg.message == query_message() {
            answer(msg.wParam.0 as u32, msg.hwnd);
            msg.message = WM_NULL;
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

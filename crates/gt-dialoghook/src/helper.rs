//! `gt_dialoghook32.exe <thread id>` (32-bit build): hooks a 32-bit app's file
//! dialog thread with `gt_dialoghook32.dll` for the agent, until its stdin
//! closes. The query itself goes through the shared block, as for 64-bit apps.

#![windows_subsystem = "windows"]

use std::io::Read;
use windows::core::{s, PCWSTR};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::WindowsAndMessaging::{SetWindowsHookExW, UnhookWindowsHookEx, HOOKPROC, WH_GETMESSAGE};

fn main() {
    let Some(thread) = std::env::args().nth(1).and_then(|a| a.parse::<u32>().ok()) else { return };
    let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_default();
    let dll: Vec<u16> = dir.join("gt_dialoghook32.dll").display().to_string().encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let Ok(module) = LoadLibraryW(PCWSTR(dll.as_ptr())) else { return };
        let proc: HOOKPROC = std::mem::transmute(GetProcAddress(module, s!("GetMsgProc")));
        let Ok(hook) = SetWindowsHookExW(WH_GETMESSAGE, proc, Some(module.into()), thread) else { return };
        // The agent waits for this before its first query.
        {
            use std::io::Write;
            let mut out = std::io::stdout();
            let _ = out.write_all(b"ok\n");
            let _ = out.flush();
        }
        // Until the agent lets go (or exits).
        let mut buf = [0u8; 16];
        while let Ok(n) = std::io::stdin().read(&mut buf) {
            if n == 0 {
                break;
            }
        }
        let _ = UnhookWindowsHookEx(hook);
    }
}

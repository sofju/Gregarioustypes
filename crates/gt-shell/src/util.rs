use windows::core::PWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GetClassNameW, GetParent, GA_ROOT};

pub fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn parent_class(hwnd: HWND) -> String {
    match unsafe { GetParent(hwnd) } {
        Ok(p) if !p.is_invalid() => class_name(p),
        _ => String::new(),
    }
}

pub fn root(hwnd: HWND) -> HWND {
    unsafe { GetAncestor(hwnd, GA_ROOT) }
}

/// Take ownership of a CoTaskMem-allocated wide string.
pub fn take_pwstr(p: PWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let s = unsafe { p.to_string().ok() };
    unsafe { CoTaskMemFree(Some(p.0 as _)) };
    s
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

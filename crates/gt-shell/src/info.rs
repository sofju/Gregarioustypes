//! Shell helpers used by the viewer: icons/thumbnails, type names, default apps,
//! launching, and the clipboard.

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HANDLE, HWND, SIZE};
use windows::Win32::Graphics::Gdi::{DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ};
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Shell::{
    AssocQueryStringW, IShellItemImageFactory, SHCreateItemFromParsingName, SHGetFileInfoW, SHOpenWithDialog, ShellExecuteW, ASSOCF_INIT_IGNOREUNKNOWN,
    ASSOCF_NOTRUNCATE, ASSOCSTR_FRIENDLYAPPNAME, OAIF_ALLOW_REGISTRATION, OAIF_EXEC, OPENASINFO, SHFILEINFOW, SHGFI_TYPENAME, SIIGBF, SIIGBF_BIGGERSIZEOK,
    SIIGBF_ICONONLY, SIIGBF_INCACHEONLY, SIIGBF_THUMBNAILONLY,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// Premultiplied BGRA pixels.
pub struct Bgra {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Shell icon (or, when `thumbnail`, a cached thumbnail) at roughly `px` pixels.
pub fn image(path: &str, px: i32, thumbnail: bool) -> Option<Bgra> {
    let flags = if thumbnail { SIIGBF(SIIGBF_THUMBNAILONLY.0 | SIIGBF_INCACHEONLY.0 | SIIGBF_BIGGERSIZEOK.0) } else { SIIGBF(SIIGBF_ICONONLY.0 | SIIGBF_BIGGERSIZEOK.0) };
    image_with(path, px, flags)
}

/// The item's thumbnail, extracted if Explorer has none yet, else its icon
/// (index sheet). Slow for some formats: call off the UI thread.
pub fn thumbnail(path: &str, px: i32) -> Option<Bgra> {
    image_with(path, px, SIIGBF(SIIGBF_BIGGERSIZEOK.0))
}

fn image_with(path: &str, px: i32, flags: SIIGBF) -> Option<Bgra> {
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(path), None).ok()?;
        let hbmp = factory.GetImage(SIZE { cx: px, cy: px }, flags).ok()?;
        let mut bm = BITMAP::default();
        GetObjectW(HGDIOBJ(hbmp.0), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as _));
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut data = vec![0u8; (w * h * 4) as usize];
        let hdc = GetDC(None);
        let lines = GetDIBits(hdc, hbmp, 0, h as u32, Some(data.as_mut_ptr() as _), &mut bi, DIB_RGB_COLORS);
        ReleaseDC(None, hdc);
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        if lines == 0 || w == 0 || h == 0 {
            return None;
        }
        // No alpha at all: opaque. Straight alpha (colour > alpha somewhere): premultiply.
        let any_alpha = data.chunks_exact(4).any(|p| p[3] != 0);
        if !any_alpha {
            data.chunks_exact_mut(4).for_each(|p| p[3] = 255);
        } else if data.chunks_exact(4).any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]) {
            for p in data.chunks_exact_mut(4) {
                let a = p[3] as u32;
                for c in &mut p[..3] {
                    *c = ((*c as u32 * a + 127) / 255) as u8;
                }
            }
        }
        Some(Bgra { width: w as u32, height: h as u32, data })
    }
}

/// "JPEG File", "Text Document", ...
pub fn type_name(path: &str) -> String {
    unsafe {
        let mut info = SHFILEINFOW::default();
        let r = SHGetFileInfoW(&HSTRING::from(path), FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut info), std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_TYPENAME);
        if r == 0 {
            return String::new();
        }
        let n = info.szTypeName.iter().position(|&c| c == 0).unwrap_or(info.szTypeName.len());
        String::from_utf16_lossy(&info.szTypeName[..n])
    }
}

/// Friendly name of the default app for this file's extension ("Photos", "Notepad").
pub fn default_app(path: &str) -> Option<String> {
    let ext = std::path::Path::new(path).extension()?.to_string_lossy().to_string();
    let assoc = HSTRING::from(format!(".{ext}"));
    unsafe {
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let hr = AssocQueryStringW(ASSOCF_INIT_IGNOREUNKNOWN | ASSOCF_NOTRUNCATE, ASSOCSTR_FRIENDLYAPPNAME, &assoc, PCWSTR::null(), Some(PWSTR(buf.as_mut_ptr())), &mut len);
        if hr.is_err() || len <= 1 {
            return None;
        }
        let s = String::from_utf16_lossy(&buf[..(len - 1) as usize]);
        (!s.is_empty()).then_some(s)
    }
}

pub fn open_default(path: &str) {
    unsafe {
        ShellExecuteW(None, windows::core::w!("open"), &HSTRING::from(path), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

pub fn open_with(owner: HWND, path: &str) {
    let file = HSTRING::from(path);
    let info = OPENASINFO { pcszFile: PCWSTR(file.as_ptr()), pcszClass: PCWSTR::null(), oaifInFlags: OAIF_ALLOW_REGISTRATION | OAIF_EXEC };
    unsafe {
        let _ = SHOpenWithDialog(Some(owner), &info);
    }
}

pub fn set_clipboard_text(owner: HWND, text: &str) -> bool {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if OpenClipboard(Some(owner)).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        let ok = (|| {
            let h = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).ok()?;
            let p = GlobalLock(h) as *mut u16;
            if p.is_null() {
                return None;
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            let _ = GlobalUnlock(h);
            SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(h.0))).ok()
        })()
        .is_some();
        let _ = CloseClipboard();
        ok
    }
}

mod drag {
    use windows::core::{implement, BOOL, HRESULT, HSTRING};
    use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, S_OK};
    use windows::Win32::System::Com::IDataObject;
    use windows::Win32::System::Ole::{DoDragDrop, IDropSource, IDropSource_Impl, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE};
    use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
    use windows::Win32::UI::Shell::{IShellItem, SHCreateItemFromParsingName, BHID_DataObject};

    #[implement(IDropSource)]
    struct Source;

    impl IDropSource_Impl for Source_Impl {
        fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
            if escape.as_bool() {
                DRAGDROP_S_CANCEL
            } else if keys.0 & MK_LBUTTON.0 == 0 {
                DRAGDROP_S_DROP
            } else {
                S_OK
            }
        }
        fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }

    /// Start an OLE drag of the file (title-bar proxy icon). Blocks until the drop.
    pub fn drag_file(path: &str) -> bool {
        unsafe {
            let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(path), None) else { return false };
            let Ok(data) = item.BindToHandler::<_, IDataObject>(None, &BHID_DataObject) else { return false };
            let src: IDropSource = Source.into();
            let mut effect = DROPEFFECT(0);
            DoDragDrop(&data, &src, DROPEFFECT_COPY | DROPEFFECT_LINK | DROPEFFECT_MOVE, &mut effect) == DRAGDROP_S_DROP
        }
    }
}

pub use drag::drag_file;

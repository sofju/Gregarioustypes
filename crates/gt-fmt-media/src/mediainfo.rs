//! MediaInfo.dll (MediaArea, C API with wide strings), loaded at run time from
//! `deps/mediainfo`. Used for the quick probe that sizes the window and fills the
//! Info panel, and for the full text report.

use std::ffi::c_void;
use std::path::Path;
use std::sync::OnceLock;
use windows::core::{s, HSTRING, PCWSTR};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH};

type Handle = *mut c_void;

struct Api {
    new: unsafe extern "C" fn() -> Handle,
    delete: unsafe extern "C" fn(Handle),
    open: unsafe extern "C" fn(Handle, *const u16) -> usize,
    close: unsafe extern "C" fn(Handle),
    inform: unsafe extern "C" fn(Handle, usize) -> *const u16,
    get: unsafe extern "C" fn(Handle, i32, usize, *const u16, i32, i32) -> *const u16,
    option: unsafe extern "C" fn(Handle, *const u16, *const u16) -> *const u16,
    count_get: unsafe extern "C" fn(Handle, i32, usize) -> usize,
}

unsafe impl Send for Api {}
unsafe impl Sync for Api {}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

fn load_api() -> Result<Api, String> {
    let path = gt_core::paths::deps_dir().join("mediainfo").join("MediaInfo.dll");
    let m = unsafe { LoadLibraryExW(&HSTRING::from(path.as_os_str()), None, LOAD_WITH_ALTERED_SEARCH_PATH) }.map_err(|e| format!("cannot load {}: {e}", path.display()))?;
    macro_rules! sym {
        ($n:literal) => {{
            let p = unsafe { GetProcAddress(m, s!($n)) }.ok_or(concat!("MediaInfo.dll lacks ", $n))?;
            #[allow(clippy::missing_transmute_annotations)]
            unsafe {
                std::mem::transmute(p)
            }
        }};
    }
    Ok(Api {
        new: sym!("MediaInfo_New"),
        delete: sym!("MediaInfo_Delete"),
        open: sym!("MediaInfo_Open"),
        close: sym!("MediaInfo_Close"),
        inform: sym!("MediaInfo_Inform"),
        get: sym!("MediaInfo_Get"),
        option: sym!("MediaInfo_Option"),
        count_get: sym!("MediaInfo_Count_Get"),
    })
}

fn api() -> Result<&'static Api, String> {
    API.get_or_init(load_api).as_ref().map_err(|e| e.clone())
}

/// Load the DLL ahead of the first preview (call from a background thread).
pub fn preload() {
    let t = std::time::Instant::now();
    match MediaInfo::new() {
        // The first instance builds MediaInfo's internal tables: pay for it now.
        Ok(mut mi) => {
            mi.option("ParseSpeed", "0");
            tracing::info!("MediaInfo ready in {:.1} ms ({})", t.elapsed().as_secs_f64() * 1000.0, version());
        }
        Err(e) => tracing::warn!("MediaInfo unavailable: {e}"),
    }
}

pub fn available() -> bool {
    api().is_ok()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn from_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { PCWSTR(p).to_string() }.unwrap_or_default()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    General = 0,
    Video = 1,
    Audio = 2,
    Text = 3,
    Other = 4,
    Image = 5,
    Menu = 6,
}

pub struct MediaInfo {
    h: Handle,
    api: &'static Api,
    open: bool,
}

impl MediaInfo {
    pub fn new() -> Result<MediaInfo, String> {
        let api = api()?;
        let h = unsafe { (api.new)() };
        if h.is_null() {
            return Err("MediaInfo_New failed".into());
        }
        Ok(MediaInfo { h, api, open: false })
    }

    pub fn option(&mut self, key: &str, value: &str) -> String {
        let (k, v) = (wide(key), wide(value));
        unsafe { from_wide((self.api.option)(self.h, k.as_ptr(), v.as_ptr())) }
    }

    pub fn open(&mut self, path: &Path) -> bool {
        let p: Vec<u16> = path.as_os_str().encode_wide_z();
        self.open = unsafe { (self.api.open)(self.h, p.as_ptr()) } != 0;
        self.open
    }

    pub fn inform(&mut self) -> String {
        unsafe { from_wide((self.api.inform)(self.h, 0)) }
    }

    pub fn get(&self, kind: Stream, n: usize, param: &str) -> String {
        let p = wide(param);
        // Info_Text = 1, search by Info_Name = 0.
        unsafe { from_wide((self.api.get)(self.h, kind as i32, n, p.as_ptr(), 1, 0)) }
    }

    pub fn count(&self, kind: Stream) -> usize {
        unsafe { (self.api.count_get)(self.h, kind as i32, usize::MAX) }
    }
}

impl Drop for MediaInfo {
    fn drop(&mut self) {
        unsafe {
            if self.open {
                (self.api.close)(self.h);
            }
            (self.api.delete)(self.h);
        }
    }
}

trait WideZ {
    fn encode_wide_z(&self) -> Vec<u16>;
}

impl WideZ for std::ffi::OsStr {
    fn encode_wide_z(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(std::iter::once(0)).collect()
    }
}

/// MediaInfo library version ("MediaInfoLib - v25.xx").
pub fn version() -> String {
    match api() {
        Ok(api) => {
            let (k, v) = (wide("Info_Version"), wide(""));
            unsafe { from_wide((api.option)(std::ptr::null_mut(), k.as_ptr(), v.as_ptr())) }
        }
        Err(_) => String::new(),
    }
}

/// The text report MediaInfo's GUI shows ("Text" view); `full` = "Complete" mode.
pub fn report(path: &Path, full: bool) -> Result<String, String> {
    let mut mi = MediaInfo::new()?;
    mi.option("Complete", if full { "1" } else { "" });
    if !mi.open(path) {
        return Err("MediaInfo could not open the file".into());
    }
    let text = mi.inform().replace("\r\n", "\n").replace('\r', "\n");
    Ok(text)
}

/// Extensions MediaInfo has something useful to say about (the MediaInfo button).
pub fn supports(ext: &str) -> bool {
    const EXTRA: &[&str] = &[
        "jpg", "jpeg", "png", "gif", "bmp", "tif", "tiff", "webp", "avif", "heic", "heif", "hif", "jxl", "jxr", "exr", "hdr", "psd", "dng", "j2k", "jp2", "jpx", "j2c",
        "jpf", "tga", "ico", "dpx", "iso", "ifo", "bup", "mpls", "clpi", "swf",
    ];
    crate::is_media_ext(ext) || EXTRA.contains(&ext)
}

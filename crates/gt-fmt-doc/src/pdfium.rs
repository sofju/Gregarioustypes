//! Minimal PDFium binding, loaded at run time from `deps/pdfium/bin/pdfium.dll`.
//!
//! PDFium is not thread-safe: every call goes through one process-wide lock.
//! Documents are read lazily through a file-access callback, so opening a
//! 500 MB PDF only touches the bytes PDFium actually needs.

use parking_lot::{Mutex, MutexGuard};
use std::ffi::{c_int, c_ulong, c_void};
use std::fs::File;
use std::os::windows::fs::FileExt;
use std::path::Path;
use std::sync::OnceLock;
use windows::core::{s, HSTRING};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH};

type H = *mut c_void;

#[repr(C)]
struct FileAccess {
    len: c_ulong,
    get_block: unsafe extern "C" fn(*mut c_void, c_ulong, *mut u8, c_ulong) -> c_int,
    param: *mut c_void,
}

#[repr(C)]
struct SizeF {
    w: f32,
    h: f32,
}

#[repr(C)]
struct Matrix {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

#[repr(C)]
struct RectF {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

/// `FPDF_FORMFILLINFO` version 1 with every callback left null (PDFium checks
/// each one before calling it).
#[repr(C)]
struct FormInfo {
    version: c_int,
    rest: [usize; 64],
}

struct Api {
    init: unsafe extern "C" fn(),
    load_custom: unsafe extern "C" fn(*mut FileAccess, *const u8) -> H,
    last_error: unsafe extern "C" fn() -> c_ulong,
    close_doc: unsafe extern "C" fn(H),
    page_count: unsafe extern "C" fn(H) -> c_int,
    page_size: unsafe extern "C" fn(H, c_int, *mut SizeF) -> c_int,
    load_page: unsafe extern "C" fn(H, c_int) -> H,
    close_page: unsafe extern "C" fn(H),
    bmp_create: unsafe extern "C" fn(c_int, c_int, c_int, *mut c_void, c_int) -> H,
    bmp_fill: unsafe extern "C" fn(H, c_int, c_int, c_int, c_int, u32) -> c_int,
    bmp_destroy: unsafe extern "C" fn(H),
    render: unsafe extern "C" fn(H, H, *const Matrix, *const RectF, c_int),
    file_version: unsafe extern "C" fn(H, *mut c_int) -> c_int,
    security_rev: unsafe extern "C" fn(H) -> c_int,
    meta_text: unsafe extern "C" fn(H, *const u8, *mut c_void, c_ulong) -> c_ulong,
    page_label: unsafe extern "C" fn(H, c_int, *mut c_void, c_ulong) -> c_ulong,
    text_load: unsafe extern "C" fn(H) -> H,
    text_close: unsafe extern "C" fn(H),
    text_count: unsafe extern "C" fn(H) -> c_int,
    text_get: unsafe extern "C" fn(H, c_int, c_int, *mut u16) -> c_int,
    bm_first: unsafe extern "C" fn(H, H) -> H,
    bm_next: unsafe extern "C" fn(H, H) -> H,
    bm_title: unsafe extern "C" fn(H, *mut c_void, c_ulong) -> c_ulong,
    bm_dest: unsafe extern "C" fn(H, H) -> H,
    bm_action: unsafe extern "C" fn(H) -> H,
    action_dest: unsafe extern "C" fn(H, H) -> H,
    dest_page: unsafe extern "C" fn(H, H) -> c_int,
    form_init: unsafe extern "C" fn(H, *mut FormInfo) -> H,
    form_exit: unsafe extern "C" fn(H),
    form_after_load: unsafe extern "C" fn(H, H),
    form_before_close: unsafe extern "C" fn(H, H),
    form_draw: unsafe extern "C" fn(H, H, H, c_int, c_int, c_int, c_int, c_int, c_int),
}

unsafe impl Send for Api {}
unsafe impl Sync for Api {}

static API: OnceLock<Result<Api, String>> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());

fn load_api() -> Result<Api, String> {
    let path = gt_core::paths::deps_dir().join("pdfium").join("bin").join("pdfium.dll");
    let module: HMODULE = unsafe { LoadLibraryExW(&HSTRING::from(path.as_os_str()), None, LOAD_WITH_ALTERED_SEARCH_PATH) }
        .map_err(|e| format!("cannot load {}: {e}", path.display()))?;
    macro_rules! sym {
        ($name:literal) => {{
            let p = unsafe { GetProcAddress(module, s!($name)) }.ok_or(concat!("pdfium lacks ", $name))?;
            #[allow(clippy::missing_transmute_annotations)]
            unsafe {
                std::mem::transmute(p)
            }
        }};
    }
    let api = Api {
        init: sym!("FPDF_InitLibrary"),
        load_custom: sym!("FPDF_LoadCustomDocument"),
        last_error: sym!("FPDF_GetLastError"),
        close_doc: sym!("FPDF_CloseDocument"),
        page_count: sym!("FPDF_GetPageCount"),
        page_size: sym!("FPDF_GetPageSizeByIndexF"),
        load_page: sym!("FPDF_LoadPage"),
        close_page: sym!("FPDF_ClosePage"),
        bmp_create: sym!("FPDFBitmap_CreateEx"),
        bmp_fill: sym!("FPDFBitmap_FillRect"),
        bmp_destroy: sym!("FPDFBitmap_Destroy"),
        render: sym!("FPDF_RenderPageBitmapWithMatrix"),
        file_version: sym!("FPDF_GetFileVersion"),
        security_rev: sym!("FPDF_GetSecurityHandlerRevision"),
        meta_text: sym!("FPDF_GetMetaText"),
        page_label: sym!("FPDF_GetPageLabel"),
        text_load: sym!("FPDFText_LoadPage"),
        text_close: sym!("FPDFText_ClosePage"),
        text_count: sym!("FPDFText_CountChars"),
        text_get: sym!("FPDFText_GetText"),
        bm_first: sym!("FPDFBookmark_GetFirstChild"),
        bm_next: sym!("FPDFBookmark_GetNextSibling"),
        bm_title: sym!("FPDFBookmark_GetTitle"),
        bm_dest: sym!("FPDFBookmark_GetDest"),
        bm_action: sym!("FPDFBookmark_GetAction"),
        action_dest: sym!("FPDFAction_GetDest"),
        dest_page: sym!("FPDFDest_GetDestPageIndex"),
        form_init: sym!("FPDFDOC_InitFormFillEnvironment"),
        form_exit: sym!("FPDFDOC_ExitFormFillEnvironment"),
        form_after_load: sym!("FORM_OnAfterLoadPage"),
        form_before_close: sym!("FORM_OnBeforeClosePage"),
        form_draw: sym!("FPDF_FFLDraw"),
    };
    let _g = LOCK.lock();
    unsafe { (api.init)() };
    Ok(api)
}

fn api() -> Result<&'static Api, String> {
    API.get_or_init(load_api).as_ref().map_err(|e| e.clone())
}

/// Load the DLL and initialise the library ahead of the first document.
pub fn preload() {
    if let Err(e) = api() {
        tracing::warn!("pdfium: {e}");
    }
}

pub fn available() -> bool {
    api().is_ok()
}

struct Reader {
    file: File,
}

unsafe extern "C" fn get_block(param: *mut c_void, pos: c_ulong, buf: *mut u8, size: c_ulong) -> c_int {
    let r = &*(param as *const Reader);
    let out = std::slice::from_raw_parts_mut(buf, size as usize);
    let mut done = 0usize;
    while done < out.len() {
        match r.file.seek_read(&mut out[done..], pos as u64 + done as u64) {
            Ok(0) => return 0,
            Ok(n) => done += n,
            Err(_) => return 0,
        }
    }
    1
}

#[derive(Debug)]
pub enum OpenError {
    Password,
    Other(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Password => write!(f, "This PDF is password protected."),
            OpenError::Other(s) => write!(f, "{s}"),
        }
    }
}

/// An open document. All methods take the global PDFium lock.
pub struct Document {
    doc: H,
    form: H,
    _form_info: Box<FormInfo>,
    _access: Box<FileAccess>,
    _reader: Box<Reader>,
    sizes: Vec<(f32, f32)>,
}

unsafe impl Send for Document {}
unsafe impl Sync for Document {}

pub struct Bookmark {
    pub title: String,
    pub page: Option<usize>,
    pub depth: usize,
}

fn utf16_buf(mut f: impl FnMut(*mut c_void, c_ulong) -> c_ulong) -> String {
    let n = f(std::ptr::null_mut(), 0) as usize;
    if n <= 2 {
        return String::new();
    }
    let mut buf = vec![0u16; n / 2];
    f(buf.as_mut_ptr() as *mut c_void, n as c_ulong);
    while buf.last() == Some(&0) {
        buf.pop();
    }
    String::from_utf16_lossy(&buf)
}

impl Document {
    pub fn open(path: &Path) -> Result<Document, OpenError> {
        let api = api().map_err(OpenError::Other)?;
        let file = File::open(path).map_err(|e| OpenError::Other(e.to_string()))?;
        let len = file.metadata().map_err(|e| OpenError::Other(e.to_string()))?.len();
        if len > u32::MAX as u64 {
            return Err(OpenError::Other("PDF files larger than 4 GB are not supported.".into()));
        }
        let mut reader = Box::new(Reader { file });
        let mut access = Box::new(FileAccess { len: len as c_ulong, get_block, param: &mut *reader as *mut Reader as *mut c_void });
        let _g = LOCK.lock();
        let doc = unsafe { (api.load_custom)(&mut *access, std::ptr::null()) };
        if doc.is_null() {
            let e = unsafe { (api.last_error)() };
            return Err(match e {
                4 => OpenError::Password,
                3 => OpenError::Other("Not a valid PDF file (or the file is damaged).".into()),
                5 => OpenError::Other("This PDF uses an unsupported security handler.".into()),
                _ => OpenError::Other(format!("PDFium could not open the file (error {e}).")),
            });
        }
        let n = unsafe { (api.page_count)(doc) }.max(0) as usize;
        let mut sizes = Vec::with_capacity(n);
        for i in 0..n {
            let mut s = SizeF { w: 612.0, h: 792.0 };
            if unsafe { (api.page_size)(doc, i as c_int, &mut s) } == 0 || s.w <= 0.0 || s.h <= 0.0 {
                s = SizeF { w: 612.0, h: 792.0 };
            }
            sizes.push((s.w, s.h));
        }
        let mut form_info = Box::new(FormInfo { version: 1, rest: [0; 64] });
        let form = unsafe { (api.form_init)(doc, &mut *form_info) };
        Ok(Document { doc, form, _form_info: form_info, _access: access, _reader: reader, sizes })
    }

    fn guard(&self) -> (MutexGuard<'static, ()>, &'static Api) {
        (LOCK.lock(), api().expect("pdfium loaded"))
    }

    pub fn page_count(&self) -> usize {
        self.sizes.len()
    }

    /// Page size in points, rotation applied.
    pub fn page_size(&self, i: usize) -> (f32, f32) {
        self.sizes.get(i).copied().unwrap_or((612.0, 792.0))
    }

    /// Render the pixel region (x, y, w, h) of page `i` drawn at `scale` pixels
    /// per point, as premultiplied BGRA on white.
    pub fn render(&self, i: usize, scale: f32, x: i32, y: i32, w: u32, h: u32) -> Option<Vec<u8>> {
        if i >= self.sizes.len() || w == 0 || h == 0 {
            return None;
        }
        let (_g, api) = self.guard();
        let stride = w as usize * 4;
        let mut buf = vec![0u8; stride * h as usize];
        unsafe {
            let page = (api.load_page)(self.doc, i as c_int);
            if page.is_null() {
                return None;
            }
            if !self.form.is_null() {
                (api.form_after_load)(page, self.form);
            }
            let bmp = (api.bmp_create)(w as c_int, h as c_int, 4, buf.as_mut_ptr() as *mut c_void, stride as c_int);
            if bmp.is_null() {
                (api.close_page)(page);
                return None;
            }
            (api.bmp_fill)(bmp, 0, 0, w as c_int, h as c_int, 0xFFFF_FFFF);
            let m = Matrix { a: scale, b: 0.0, c: 0.0, d: scale, e: -x as f32, f: -y as f32 };
            let clip = RectF { left: 0.0, top: 0.0, right: w as f32, bottom: h as f32 };
            // FPDF_ANNOT: annotations with appearance streams (stamps, filled fields).
            (api.render)(bmp, page, &m, &clip, 0x01);
            if !self.form.is_null() {
                // Interactive form fields drawn the way a viewer shows them.
                let (pw, ph) = self.sizes[i];
                (api.form_draw)(self.form, bmp, page, -x, -y, (pw * scale).round() as c_int, (ph * scale).round() as c_int, 0, 0x01);
                (api.form_before_close)(page, self.form);
            }
            (api.bmp_destroy)(bmp);
            (api.close_page)(page);
        }
        Some(buf)
    }

    pub fn page_text(&self, i: usize) -> Option<String> {
        let (_g, api) = self.guard();
        unsafe {
            let page = (api.load_page)(self.doc, i as c_int);
            if page.is_null() {
                return None;
            }
            let tp = (api.text_load)(page);
            let mut out = None;
            if !tp.is_null() {
                let n = (api.text_count)(tp);
                if n > 0 {
                    let mut buf = vec![0u16; n as usize + 1];
                    let got = (api.text_get)(tp, 0, n, buf.as_mut_ptr());
                    buf.truncate((got.max(1) - 1) as usize);
                    out = Some(String::from_utf16_lossy(&buf).replace("\r\n", "\n"));
                }
                (api.text_close)(tp);
            }
            (api.close_page)(page);
            out
        }
    }

    /// Document information dictionary entry ("Title", "Author", "CreationDate"...).
    pub fn meta(&self, tag: &str) -> String {
        let (_g, api) = self.guard();
        let mut t = tag.as_bytes().to_vec();
        t.push(0);
        utf16_buf(|p, n| unsafe { (api.meta_text)(self.doc, t.as_ptr(), p, n) }).trim().to_string()
    }

    pub fn page_label(&self, i: usize) -> String {
        let (_g, api) = self.guard();
        utf16_buf(|p, n| unsafe { (api.page_label)(self.doc, i as c_int, p, n) })
    }

    /// "1.7" style version.
    pub fn version(&self) -> Option<String> {
        let (_g, api) = self.guard();
        let mut v = 0;
        (unsafe { (api.file_version)(self.doc, &mut v) } != 0).then(|| format!("{}.{}", v / 10, v % 10))
    }

    /// Standard security handler revision, None when unencrypted.
    pub fn security_revision(&self) -> Option<i32> {
        let (_g, api) = self.guard();
        let r = unsafe { (api.security_rev)(self.doc) };
        (r >= 0).then_some(r)
    }

    /// The outline (bookmarks), depth-first, capped at `max` entries.
    pub fn outline(&self, max: usize) -> Vec<Bookmark> {
        let (_g, api) = self.guard();
        let mut out = Vec::new();
        let mut stack: Vec<(H, usize)> = Vec::new();
        let first = unsafe { (api.bm_first)(self.doc, std::ptr::null_mut()) };
        if !first.is_null() {
            stack.push((first, 0));
        }
        let mut seen = std::collections::HashSet::new();
        while let Some((bm, depth)) = stack.pop() {
            if out.len() >= max || !seen.insert(bm as usize) {
                break;
            }
            let title = utf16_buf(|p, n| unsafe { (api.bm_title)(bm, p, n) });
            let mut dest = unsafe { (api.bm_dest)(self.doc, bm) };
            if dest.is_null() {
                let a = unsafe { (api.bm_action)(bm) };
                if !a.is_null() {
                    dest = unsafe { (api.action_dest)(self.doc, a) };
                }
            }
            let page = if dest.is_null() { None } else { usize::try_from(unsafe { (api.dest_page)(self.doc, dest) }).ok() };
            out.push(Bookmark { title, page, depth });
            let next = unsafe { (api.bm_next)(self.doc, bm) };
            if !next.is_null() {
                stack.push((next, depth));
            }
            let child = unsafe { (api.bm_first)(self.doc, bm) };
            if !child.is_null() && depth < 16 {
                stack.push((child, depth + 1));
            }
        }
        out
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        let (_g, api) = self.guard();
        unsafe {
            if !self.form.is_null() {
                (api.form_exit)(self.form);
            }
            (api.close_doc)(self.doc);
        }
    }
}

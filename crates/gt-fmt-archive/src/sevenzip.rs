//! Bridge to 7-Zip's `7z.dll` (loaded at run time from `deps/7zip`): COM-style
//! interfaces declared in Rust, our own stream objects, format discovery,
//! opening, listing and extraction.

#![allow(non_snake_case, clippy::missing_safety_doc)]

use gt_core::CancelToken;
use parking_lot::Mutex;
use std::ffi::c_void;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use windows::core::{implement, interface, s, Interface, BSTR, GUID, HRESULT, HSTRING, IUnknown, IUnknown_Vtbl, PCWSTR};
use windows::Win32::Foundation::{E_ABORT, E_FAIL, E_INVALIDARG, E_NOTIMPL, S_FALSE, S_OK};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH};

// ------------------------------------------------------------------ PROPVARIANT

#[repr(C)]
#[derive(Default)]
pub struct PropVar {
    vt: u16,
    _r: [u16; 3],
    v: u64,
    v2: u64,
}

const VT_EMPTY: u16 = 0;
const VT_I4: u16 = 3;
const VT_BSTR: u16 = 8;
const VT_BOOL: u16 = 11;
const VT_I8: u16 = 20;
const VT_UI1: u16 = 17;
const VT_UI2: u16 = 18;
const VT_UI4: u16 = 19;
const VT_UI8: u16 = 21;
const VT_FILETIME: u16 = 64;

#[derive(Clone, Debug, PartialEq)]
pub enum Prop {
    Empty,
    Str(String),
    Bytes(Vec<u8>),
    Bool(bool),
    Num(u64),
    Time(u64),
}

impl PropVar {
    /// Convert and free.
    fn take(&mut self, binary: bool) -> Prop {
        let p = match self.vt {
            VT_EMPTY => Prop::Empty,
            VT_BSTR => {
                let ptr = self.v as *const u16;
                if ptr.is_null() {
                    Prop::Empty
                } else {
                    // BSTR length prefix (bytes) sits just before the pointer.
                    let bytes = unsafe { *(ptr as *const u32).offset(-1) } as usize;
                    if binary {
                        Prop::Bytes(unsafe { std::slice::from_raw_parts(ptr as *const u8, bytes) }.to_vec())
                    } else {
                        Prop::Str(String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, bytes / 2) }))
                    }
                }
            }
            VT_BOOL => Prop::Bool(self.v as u16 != 0),
            VT_UI1 => Prop::Num(self.v as u8 as u64),
            VT_UI2 => Prop::Num(self.v as u16 as u64),
            VT_UI4 | VT_I4 => Prop::Num(self.v as u32 as u64),
            VT_UI8 | VT_I8 => Prop::Num(self.v),
            VT_FILETIME => Prop::Time(self.v),
            _ => Prop::Empty,
        };
        if self.vt == VT_BSTR && self.v != 0 {
            drop(unsafe { BSTR::from_raw(self.v as *const u16) });
        }
        self.vt = VT_EMPTY;
        self.v = 0;
        p
    }
}

impl Prop {
    pub fn str(&self) -> Option<&str> {
        match self {
            Prop::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn num(&self) -> Option<u64> {
        match self {
            Prop::Num(n) => Some(*n),
            _ => None,
        }
    }
    pub fn bool(&self) -> bool {
        matches!(self, Prop::Bool(true))
    }
}

// Item properties (kpid*).
pub const KPID_PATH: u32 = 3;
pub const KPID_IS_DIR: u32 = 6;
pub const KPID_SIZE: u32 = 7;
pub const KPID_PACK_SIZE: u32 = 8;
pub const KPID_ATTRIB: u32 = 9;
pub const KPID_CTIME: u32 = 10;
pub const KPID_MTIME: u32 = 12;
pub const KPID_SOLID: u32 = 13;
pub const KPID_ENCRYPTED: u32 = 15;
pub const KPID_METHOD: u32 = 22;
pub const KPID_HOST_OS: u32 = 23;
pub const KPID_FILE_SYSTEM: u32 = 24;
pub const KPID_COMMENT: u32 = 28;
pub const KPID_NUM_BLOCKS: u32 = 38;
pub const KPID_NUM_VOLUMES: u32 = 39;
pub const KPID_PHY_SIZE: u32 = 44;
pub const KPID_HEADERS_SIZE: u32 = 45;
pub const KPID_CHARACTS: u32 = 47;
pub const KPID_CREATOR_APP: u32 = 51;
pub const KPID_SYMLINK: u32 = 54;
pub const KPID_ERROR: u32 = 55;
pub const KPID_CLUSTER_SIZE: u32 = 58;
pub const KPID_VOLUME_NAME: u32 = 59;
pub const KPID_IS_ALT_STREAM: u32 = 63;
pub const KPID_IS_AUX: u32 = 64;
pub const KPID_WARNING: u32 = 73;
pub const KPID_TOTAL_PHY_SIZE: u32 = 79;
pub const KPID_SUB_TYPE: u32 = 81;

// Format (handler) properties.
const HP_NAME: u32 = 0;
const HP_CLASS_ID: u32 = 1;
const HP_EXTENSION: u32 = 2;
const HP_SIGNATURE: u32 = 6;
const HP_MULTI_SIGNATURE: u32 = 7;
const HP_SIGNATURE_OFFSET: u32 = 8;

// ------------------------------------------------------------------ interfaces

#[interface("23170F69-40C1-278A-0000-000300010000")]
pub unsafe trait ISequentialInStream: IUnknown {
    unsafe fn Read(&self, data: *mut c_void, size: u32, processed: *mut u32) -> HRESULT;
}

#[interface("23170F69-40C1-278A-0000-000300020000")]
pub unsafe trait ISequentialOutStream: IUnknown {
    unsafe fn Write(&self, data: *const c_void, size: u32, processed: *mut u32) -> HRESULT;
}

#[interface("23170F69-40C1-278A-0000-000300030000")]
pub unsafe trait IInStream: ISequentialInStream {
    unsafe fn Seek(&self, offset: i64, origin: u32, new_pos: *mut u64) -> HRESULT;
}

#[interface("23170F69-40C1-278A-0000-000300060000")]
pub unsafe trait IStreamGetSize: IUnknown {
    unsafe fn GetSize(&self, size: *mut u64) -> HRESULT;
}

#[interface("23170F69-40C1-278A-0000-000600100000")]
pub unsafe trait IArchiveOpenCallback: IUnknown {
    unsafe fn SetTotal(&self, files: *const u64, bytes: *const u64) -> HRESULT;
    unsafe fn SetCompleted(&self, files: *const u64, bytes: *const u64) -> HRESULT;
}

#[interface("23170F69-40C1-278A-0000-000000050000")]
pub unsafe trait IProgress: IUnknown {
    unsafe fn SetTotal(&self, total: u64) -> HRESULT;
    unsafe fn SetCompleted(&self, complete: *const u64) -> HRESULT;
}

#[interface("23170F69-40C1-278A-0000-000600200000")]
pub unsafe trait IArchiveExtractCallback: IProgress {
    unsafe fn GetStream(&self, index: u32, out: *mut *mut c_void, mode: i32) -> HRESULT;
    unsafe fn PrepareOperation(&self, mode: i32) -> HRESULT;
    unsafe fn SetOperationResult(&self, result: i32) -> HRESULT;
}

#[interface("23170F69-40C1-278A-0000-000600300000")]
pub unsafe trait IArchiveOpenVolumeCallback: IUnknown {
    unsafe fn GetProperty(&self, prop: u32, value: *mut PropVar) -> HRESULT;
    unsafe fn GetStream(&self, name: PCWSTR, stream: *mut *mut c_void) -> HRESULT;
}

#[interface("23170F69-40C1-278A-0000-000600400000")]
pub unsafe trait IInArchiveGetStream: IUnknown {
    unsafe fn GetStream(&self, index: u32, stream: *mut *mut c_void) -> HRESULT;
}

#[interface("23170F69-40C1-278A-0000-000500100000")]
pub unsafe trait ICryptoGetTextPassword: IUnknown {
    unsafe fn CryptoGetTextPassword(&self, password: *mut BSTR) -> HRESULT;
}

#[interface("23170F69-40C1-278A-0000-000600600000")]
pub unsafe trait IInArchive: IUnknown {
    unsafe fn Open(&self, stream: *mut c_void, max_check: *const u64, callback: *mut c_void) -> HRESULT;
    unsafe fn Close(&self) -> HRESULT;
    unsafe fn GetNumberOfItems(&self, n: *mut u32) -> HRESULT;
    unsafe fn GetProperty(&self, index: u32, prop: u32, value: *mut PropVar) -> HRESULT;
    unsafe fn Extract(&self, indices: *const u32, n: u32, test: i32, callback: *mut c_void) -> HRESULT;
    unsafe fn GetArchiveProperty(&self, prop: u32, value: *mut PropVar) -> HRESULT;
    unsafe fn GetNumberOfProperties(&self, n: *mut u32) -> HRESULT;
    unsafe fn GetPropertyInfo(&self, index: u32, name: *mut BSTR, prop: *mut u32, vt: *mut u16) -> HRESULT;
    unsafe fn GetNumberOfArchiveProperties(&self, n: *mut u32) -> HRESULT;
    unsafe fn GetArchivePropertyInfo(&self, index: u32, name: *mut BSTR, prop: *mut u32, vt: *mut u16) -> HRESULT;
}

// ------------------------------------------------------------------ streams

/// Anything readable and seekable with a known size (files, virtual disc images).
pub trait Source: Read + Seek + Send {
    fn len(&self) -> u64;
}

impl Source for std::fs::File {
    fn len(&self) -> u64 {
        self.metadata().map(|m| m.len()).unwrap_or(0)
    }
}

/// Wrap an `std::io::BufReader<File>`-like reader with a fixed size.
pub struct SizedReader<R: Read + Seek + Send> {
    pub inner: R,
    pub size: u64,
}

impl<R: Read + Seek + Send> Read for SizedReader<R> {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(b)
    }
}
impl<R: Read + Seek + Send> Seek for SizedReader<R> {
    fn seek(&mut self, p: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(p)
    }
}
impl<R: Read + Seek + Send> Source for SizedReader<R> {
    fn len(&self) -> u64 {
        self.size
    }
}

#[implement(IInStream, IStreamGetSize)]
pub struct InStream {
    src: Mutex<Box<dyn Source>>,
    cancel: CancelToken,
}

impl InStream {
    pub fn new(src: Box<dyn Source>, cancel: CancelToken) -> IInStream {
        InStream { src: Mutex::new(src), cancel }.into()
    }
}

impl ISequentialInStream_Impl for InStream_Impl {
    unsafe fn Read(&self, data: *mut c_void, size: u32, processed: *mut u32) -> HRESULT {
        if self.cancel.is_cancelled() {
            return E_ABORT;
        }
        let buf = unsafe { std::slice::from_raw_parts_mut(data as *mut u8, size as usize) };
        let mut src = self.src.lock();
        let mut done = 0usize;
        // 7-Zip expects short reads only at the end of the stream.
        while done < buf.len() {
            match src.read(&mut buf[done..]) {
                Ok(0) => break,
                Ok(n) => done += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => {
                    if !processed.is_null() {
                        unsafe { *processed = done as u32 };
                    }
                    return E_FAIL;
                }
            }
        }
        if !processed.is_null() {
            unsafe { *processed = done as u32 };
        }
        S_OK
    }
}

impl IInStream_Impl for InStream_Impl {
    unsafe fn Seek(&self, offset: i64, origin: u32, new_pos: *mut u64) -> HRESULT {
        let pos = match origin {
            0 => SeekFrom::Start(offset.max(0) as u64),
            1 => SeekFrom::Current(offset),
            2 => SeekFrom::End(offset),
            _ => return E_INVALIDARG,
        };
        match self.src.lock().seek(pos) {
            Ok(p) => {
                if !new_pos.is_null() {
                    unsafe { *new_pos = p };
                }
                S_OK
            }
            Err(_) => HRESULT(0x80070083u32 as i32), // ERROR_NEGATIVE_SEEK
        }
    }
}

impl IStreamGetSize_Impl for InStream_Impl {
    unsafe fn GetSize(&self, size: *mut u64) -> HRESULT {
        unsafe { *size = self.src.lock().len() };
        S_OK
    }
}

#[implement(ISequentialOutStream)]
struct OutStream {
    sink: Mutex<Box<dyn Write + Send>>,
    cancel: CancelToken,
    written: Arc<AtomicU64>,
}

impl ISequentialOutStream_Impl for OutStream_Impl {
    unsafe fn Write(&self, data: *const c_void, size: u32, processed: *mut u32) -> HRESULT {
        if self.cancel.is_cancelled() {
            return E_ABORT;
        }
        let buf = unsafe { std::slice::from_raw_parts(data as *const u8, size as usize) };
        match self.sink.lock().write_all(buf) {
            Ok(()) => {
                self.written.fetch_add(size as u64, Ordering::Relaxed);
                if !processed.is_null() {
                    unsafe { *processed = size };
                }
                S_OK
            }
            Err(_) => E_ABORT,
        }
    }
}

/// Progress shared with the UI while opening or extracting.
#[derive(Default)]
pub struct Progress {
    pub total_bytes: AtomicU64,
    pub done_bytes: AtomicU64,
    pub files: AtomicU64,
    pub needs_password: AtomicBool,
}

#[implement(IArchiveOpenCallback, IArchiveOpenVolumeCallback, ICryptoGetTextPassword)]
struct OpenCallback {
    progress: Arc<Progress>,
    cancel: CancelToken,
    /// For multi-volume archives: the first volume's path.
    path: Option<PathBuf>,
}

impl IArchiveOpenCallback_Impl for OpenCallback_Impl {
    unsafe fn SetTotal(&self, _files: *const u64, bytes: *const u64) -> HRESULT {
        if !bytes.is_null() {
            self.progress.total_bytes.store(unsafe { *bytes }, Ordering::Relaxed);
        }
        S_OK
    }
    unsafe fn SetCompleted(&self, files: *const u64, bytes: *const u64) -> HRESULT {
        if self.cancel.is_cancelled() {
            return E_ABORT;
        }
        if !files.is_null() {
            self.progress.files.store(unsafe { *files }, Ordering::Relaxed);
        }
        if !bytes.is_null() {
            self.progress.done_bytes.store(unsafe { *bytes }, Ordering::Relaxed);
        }
        S_OK
    }
}

impl IArchiveOpenVolumeCallback_Impl for OpenCallback_Impl {
    unsafe fn GetProperty(&self, prop: u32, value: *mut PropVar) -> HRESULT {
        // kpidName (4): the first volume's file name.
        if prop == 4 {
            if let Some(name) = self.path.as_ref().and_then(|p| p.file_name()) {
                let b = BSTR::from(name.to_string_lossy().as_ref());
                unsafe {
                    (*value).vt = VT_BSTR;
                    (*value).v = b.into_raw() as u64;
                }
            }
        }
        S_OK
    }
    unsafe fn GetStream(&self, name: PCWSTR, stream: *mut *mut c_void) -> HRESULT {
        let Some(dir) = self.path.as_ref().and_then(|p| p.parent()) else { return S_FALSE };
        let name = unsafe { name.to_string() }.unwrap_or_default();
        match std::fs::File::open(dir.join(&name)) {
            Ok(f) => {
                let s = InStream::new(Box::new(f), self.cancel.clone());
                unsafe { *stream = s.into_raw() };
                S_OK
            }
            Err(_) => S_FALSE,
        }
    }
}

impl ICryptoGetTextPassword_Impl for OpenCallback_Impl {
    unsafe fn CryptoGetTextPassword(&self, _password: *mut BSTR) -> HRESULT {
        self.progress.needs_password.store(true, Ordering::Relaxed);
        E_ABORT
    }
}

#[implement(IArchiveExtractCallback, ICryptoGetTextPassword)]
struct ExtractCallback {
    index: u32,
    out: Mutex<Option<ISequentialOutStream>>,
    result: Arc<std::sync::atomic::AtomicI32>,
    progress: Arc<Progress>,
    cancel: CancelToken,
}

impl IProgress_Impl for ExtractCallback_Impl {
    unsafe fn SetTotal(&self, total: u64) -> HRESULT {
        self.progress.total_bytes.store(total, Ordering::Relaxed);
        S_OK
    }
    unsafe fn SetCompleted(&self, complete: *const u64) -> HRESULT {
        if self.cancel.is_cancelled() {
            return E_ABORT;
        }
        if !complete.is_null() {
            self.progress.done_bytes.store(unsafe { *complete }, Ordering::Relaxed);
        }
        S_OK
    }
}

impl IArchiveExtractCallback_Impl for ExtractCallback_Impl {
    unsafe fn GetStream(&self, index: u32, out: *mut *mut c_void, mode: i32) -> HRESULT {
        unsafe { *out = std::ptr::null_mut() };
        if mode != 0 || index != self.index {
            return S_OK;
        }
        if let Some(s) = self.out.lock().take() {
            unsafe { *out = s.into_raw() };
        }
        S_OK
    }
    unsafe fn PrepareOperation(&self, _mode: i32) -> HRESULT {
        S_OK
    }
    unsafe fn SetOperationResult(&self, result: i32) -> HRESULT {
        self.result.store(result, Ordering::Relaxed);
        S_OK
    }
}

impl ICryptoGetTextPassword_Impl for ExtractCallback_Impl {
    unsafe fn CryptoGetTextPassword(&self, _password: *mut BSTR) -> HRESULT {
        self.progress.needs_password.store(true, Ordering::Relaxed);
        E_ABORT
    }
}

// ------------------------------------------------------------------ library

pub struct Format {
    pub name: String,
    clsid: GUID,
    pub exts: Vec<String>,
    /// (signature, offset)
    pub sigs: Vec<(Vec<u8>, u32)>,
}

type CreateObjectFn = unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut c_void) -> HRESULT;
type GetNumberOfFormatsFn = unsafe extern "system" fn(*mut u32) -> HRESULT;
type GetHandlerProperty2Fn = unsafe extern "system" fn(u32, u32, *mut PropVar) -> HRESULT;

pub struct Lib {
    create: CreateObjectFn,
    pub formats: Vec<Format>,
}

unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();

fn load() -> Result<Lib, String> {
    // Built from source without the RAR decoders (pure LGPL); see `rar`.
    let path = gt_core::paths::deps_dir().join("7zip-lgpl").join("7z.dll");
    let m = unsafe { LoadLibraryExW(&HSTRING::from(path.as_os_str()), None, LOAD_WITH_ALTERED_SEARCH_PATH) }.map_err(|e| format!("cannot load {}: {e}", path.display()))?;
    unsafe {
        let create: CreateObjectFn = std::mem::transmute(GetProcAddress(m, s!("CreateObject")).ok_or("7z.dll lacks CreateObject")?);
        let count: GetNumberOfFormatsFn = std::mem::transmute(GetProcAddress(m, s!("GetNumberOfFormats")).ok_or("7z.dll lacks GetNumberOfFormats")?);
        let prop: GetHandlerProperty2Fn = std::mem::transmute(GetProcAddress(m, s!("GetHandlerProperty2")).ok_or("7z.dll lacks GetHandlerProperty2")?);
        let mut n = 0u32;
        let _ = count(&mut n);
        let mut formats = Vec::new();
        for i in 0..n {
            let get = |pid: u32, binary: bool| {
                let mut v = PropVar::default();
                let _ = prop(i, pid, &mut v);
                v.take(binary)
            };
            let name = get(HP_NAME, false).str().unwrap_or("").to_string();
            let Prop::Bytes(id) = get(HP_CLASS_ID, true) else { continue };
            if id.len() < 16 {
                continue;
            }
            let clsid = GUID::from_u128(u128::from_be_bytes({
                // GUID in memory: Data1 LE, Data2 LE, Data3 LE, Data4 bytes.
                let d1 = u32::from_le_bytes([id[0], id[1], id[2], id[3]]);
                let d2 = u16::from_le_bytes([id[4], id[5]]);
                let d3 = u16::from_le_bytes([id[6], id[7]]);
                let mut b = [0u8; 16];
                b[0..4].copy_from_slice(&d1.to_be_bytes());
                b[4..6].copy_from_slice(&d2.to_be_bytes());
                b[6..8].copy_from_slice(&d3.to_be_bytes());
                b[8..16].copy_from_slice(&id[8..16]);
                b
            }));
            let exts = get(HP_EXTENSION, false).str().unwrap_or("").split_whitespace().map(|e| e.trim_start_matches('*').trim_start_matches('.').to_ascii_lowercase()).collect();
            let off = get(HP_SIGNATURE_OFFSET, false).num().unwrap_or(0) as u32;
            let mut sigs = Vec::new();
            if let Prop::Bytes(sig) = get(HP_SIGNATURE, true) {
                if !sig.is_empty() {
                    sigs.push((sig, off));
                }
            }
            if let Prop::Bytes(multi) = get(HP_MULTI_SIGNATURE, true) {
                // Length-prefixed list.
                let mut j = 0;
                while j < multi.len() {
                    let l = multi[j] as usize;
                    if j + 1 + l > multi.len() || l == 0 {
                        break;
                    }
                    sigs.push((multi[j + 1..j + 1 + l].to_vec(), off));
                    j += 1 + l;
                }
            }
            formats.push(Format { name, clsid, exts, sigs });
        }
        Ok(Lib { create, formats })
    }
}

pub fn lib() -> Result<&'static Lib, String> {
    LIB.get_or_init(load).as_ref().map_err(|e| e.clone())
}

pub fn preload() {
    if let Err(e) = lib() {
        tracing::warn!("7-Zip unavailable: {e}");
    }
}

impl Lib {
    fn create(&self, f: &Format) -> Option<IInArchive> {
        let mut p: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { (self.create)(&f.clsid, &IInArchive::IID, &mut p) };
        if hr.is_err() || p.is_null() {
            return None;
        }
        Some(unsafe { IInArchive::from_raw(p) })
    }

    pub fn format(&self, name: &str) -> Option<&Format> {
        self.formats.iter().find(|f| f.name.eq_ignore_ascii_case(name))
    }

    /// Candidate formats: preferred ones, then signature + extension matches,
    /// extension matches, and signature matches.
    pub fn candidates<'a>(&'a self, ext: &str, head: &[u8], preferred: &[&str]) -> Vec<&'a Format> {
        let mut out: Vec<&'a Format> = Vec::new();
        fn push<'a>(out: &mut Vec<&'a Format>, f: &'a Format) {
            if !out.iter().any(|g| std::ptr::eq(*g, f)) {
                out.push(f);
            }
        }
        let sig_hit = |f: &Format| {
            f.sigs.iter().any(|(s, off)| {
                let o = *off as usize;
                head.len() >= o + s.len() && &head[o..o + s.len()] == s.as_slice()
            })
        };
        for p in preferred {
            if let Some(f) = self.format(p) {
                push(&mut out, f);
            }
        }
        for f in &self.formats {
            if sig_hit(f) && f.exts.iter().any(|e| e == ext) {
                push(&mut out, f);
            }
        }
        for f in &self.formats {
            if f.exts.iter().any(|e| e == ext) {
                push(&mut out, f);
            }
        }
        for f in &self.formats {
            if sig_hit(f) {
                push(&mut out, f);
            }
        }
        out
    }
}

// ------------------------------------------------------------------ archives

#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub index: u32,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub packed: Option<u64>,
    pub mtime: Option<u64>,
    pub encrypted: bool,
    pub method: String,
    pub symlink: String,
}

pub struct Archive {
    arc: IInArchive,
    pub format: String,
    _stream: IInStream,
}

unsafe impl Send for Archive {}

#[derive(Debug)]
pub enum OpenError {
    NotArchive,
    Password,
    Cancelled,
    Unavailable(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::NotArchive => write!(f, "not a supported archive"),
            OpenError::Password => write!(f, "the archive is encrypted: its file names are protected by a password"),
            OpenError::Cancelled => write!(f, "cancelled"),
            OpenError::Unavailable(e) => write!(f, "{e}"),
        }
    }
}

impl Archive {
    /// Open `src`, trying `candidates` in order.
    pub fn open(src: Box<dyn Source>, path: Option<&Path>, candidates: &[&Format], progress: Arc<Progress>, cancel: &CancelToken) -> Result<Archive, OpenError> {
        let lib = lib().map_err(OpenError::Unavailable)?;
        let stream = InStream::new(src, cancel.clone());
        let cb: IArchiveOpenCallback = OpenCallback { progress: progress.clone(), cancel: cancel.clone(), path: path.map(|p| p.to_path_buf()) }.into();
        let max_check: u64 = 1 << 22;
        for f in candidates {
            if cancel.is_cancelled() {
                return Err(OpenError::Cancelled);
            }
            let Some(arc) = lib.create(f) else { continue };
            unsafe {
                let _ = stream.Seek(0, 0, std::ptr::null_mut());
                let hr = arc.Open(stream.as_raw(), &max_check, cb.as_raw());
                if hr == S_OK {
                    return Ok(Archive { arc, format: f.name.clone(), _stream: stream.clone() });
                }
                let _ = arc.Close();
            }
            if progress.needs_password.load(Ordering::Relaxed) {
                return Err(OpenError::Password);
            }
            if cancel.is_cancelled() {
                return Err(OpenError::Cancelled);
            }
        }
        Err(OpenError::NotArchive)
    }

    pub fn count(&self) -> u32 {
        let mut n = 0;
        unsafe {
            let _ = self.arc.GetNumberOfItems(&mut n);
        }
        n
    }

    pub fn prop(&self, index: u32, pid: u32) -> Prop {
        let mut v = PropVar::default();
        unsafe {
            let _ = self.arc.GetProperty(index, pid, &mut v);
        }
        v.take(false)
    }

    pub fn arc_prop(&self, pid: u32) -> Prop {
        let mut v = PropVar::default();
        unsafe {
            let _ = self.arc.GetArchiveProperty(pid, &mut v);
        }
        v.take(false)
    }

    pub fn entries(&self, cancel: &CancelToken) -> Vec<Entry> {
        let n = self.count();
        let mut v = Vec::with_capacity(n as usize);
        for i in 0..n {
            if i % 4096 == 0 && cancel.is_cancelled() {
                break;
            }
            if self.prop(i, KPID_IS_ALT_STREAM).bool() || self.prop(i, KPID_IS_AUX).bool() {
                continue;
            }
            v.push(Entry {
                index: i,
                path: self.prop(i, KPID_PATH).str().unwrap_or("").to_string(),
                is_dir: self.prop(i, KPID_IS_DIR).bool(),
                size: self.prop(i, KPID_SIZE).num().unwrap_or(0),
                packed: self.prop(i, KPID_PACK_SIZE).num(),
                mtime: match self.prop(i, KPID_MTIME) {
                    Prop::Time(t) if t > 0 => Some(t),
                    _ => None,
                },
                encrypted: self.prop(i, KPID_ENCRYPTED).bool(),
                method: self.prop(i, KPID_METHOD).str().unwrap_or("").to_string(),
                symlink: self.prop(i, KPID_SYMLINK).str().unwrap_or("").to_string(),
            });
        }
        v
    }

    /// Extract one item into `sink`. Returns the 7-Zip operation result.
    pub fn extract(&self, index: u32, sink: Box<dyn Write + Send>, progress: Arc<Progress>, cancel: &CancelToken) -> Result<u64, String> {
        let written = Arc::new(AtomicU64::new(0));
        let out: ISequentialOutStream = OutStream { sink: Mutex::new(sink), cancel: cancel.clone(), written: written.clone() }.into();
        let result = Arc::new(std::sync::atomic::AtomicI32::new(-1));
        let cb: IArchiveExtractCallback = ExtractCallback { index, out: Mutex::new(Some(out)), result: result.clone(), progress: progress.clone(), cancel: cancel.clone() }.into();
        let hr = unsafe { self.arc.Extract(&index, 1, 0, cb.as_raw()) };
        if cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        let r = result.load(Ordering::Relaxed);
        if progress.needs_password.load(Ordering::Relaxed) || r == 9 {
            return Err("this file is encrypted (password required)".into());
        }
        if hr.is_err() && hr != E_NOTIMPL {
            return Err(format!("extraction failed ({hr:?})"));
        }
        match r {
            0 | -1 => Ok(written.load(Ordering::Relaxed)),
            1 => Err("unsupported compression method".into()),
            2 => Err("data error".into()),
            3 => Err("CRC error".into()),
            4 => Err("data unavailable".into()),
            5 => Err("unexpected end of data".into()),
            n => Err(format!("extraction error {n}")),
        }
    }

    /// Open item `index` as an archive of its own (disk images inside DMG, ISO
    /// inside ZIP...), using random access when the handler offers it.
    pub fn nested(&self, index: u32, name: &str, cancel: &CancelToken) -> Option<Archive> {
        let gs: IInArchiveGetStream = self.arc.cast().ok()?;
        let mut p: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { gs.GetStream(index, &mut p) };
        if hr.is_err() || p.is_null() {
            return None;
        }
        let seq = unsafe { ISequentialInStream::from_raw(p) };
        let stream: IInStream = seq.cast().ok()?;
        let lib = lib().ok()?;
        let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        let mut head = vec![0u8; 64 * 1024];
        let mut got = 0u32;
        unsafe {
            let _ = stream.Seek(0, 0, std::ptr::null_mut());
            let _ = stream.Read(head.as_mut_ptr() as _, head.len() as u32, &mut got);
            let _ = stream.Seek(0, 0, std::ptr::null_mut());
        }
        head.truncate(got as usize);
        let progress = Arc::new(Progress::default());
        let cb: IArchiveOpenCallback = OpenCallback { progress, cancel: cancel.clone(), path: None }.into();
        let max_check: u64 = 1 << 22;
        for f in lib.candidates(&ext, &head, &[]) {
            let Some(arc) = lib.create(f) else { continue };
            unsafe {
                let _ = stream.Seek(0, 0, std::ptr::null_mut());
                if arc.Open(stream.as_raw(), &max_check, cb.as_raw()) == S_OK {
                    return Some(Archive { arc, format: f.name.clone(), _stream: stream });
                }
                let _ = arc.Close();
            }
        }
        None
    }

    pub fn raw(&self) -> &IInArchive {
        &self.arc
    }
}

impl Drop for Archive {
    fn drop(&mut self) {
        unsafe {
            let _ = self.arc.Close();
        }
    }
}

/// A pipe: `extract` writes into the sender, the listing thread reads.
pub struct PipeWriter(pub std::sync::mpsc::SyncSender<Vec<u8>>);

impl Write for PipeWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.send(b.to_vec()).map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "reader gone"))?;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub struct PipeReader {
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
    pub read: u64,
}

impl PipeReader {
    pub fn new(rx: std::sync::mpsc::Receiver<Vec<u8>>) -> Self {
        PipeReader { rx, buf: Vec::new(), pos: 0, read: 0 }
    }
}

impl Read for PipeReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.buf.len() {
            match self.rx.recv() {
                Ok(b) => {
                    self.buf = b;
                    self.pos = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = (self.buf.len() - self.pos).min(out.len());
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        self.read += n as u64;
        Ok(n)
    }
}

pub fn filetime_to_unix(ft: u64) -> i64 {
    (ft / 10_000_000) as i64 - 11_644_473_600
}

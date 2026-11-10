//! Minimal libmpv binding, loaded at run time from `deps/mpv/libmpv-2.dll`.
//!
//! The UI thread only ever uses the asynchronous calls (`command_async`,
//! `set_*_async`) and `wait_event(0)`: a synchronous call could deadlock while
//! mpv's window thread sends a message to our window.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::OnceLock;
use windows::core::{s, HSTRING};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH};

#[repr(C)]
pub struct RawHandle {
    _p: [u8; 0],
}

#[repr(C)]
pub struct RawEvent {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

#[repr(C)]
struct RawEventProperty {
    name: *const c_char,
    format: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct RawLogMessage {
    prefix: *const c_char,
    level: *const c_char,
    text: *const c_char,
    log_level: c_int,
}

#[repr(C)]
struct RawEndFile {
    reason: c_int,
    error: c_int,
    playlist_entry_id: i64,
    playlist_insert_id: i64,
    playlist_insert_num_entries: c_int,
}

#[repr(C)]
struct RawStartFile {
    playlist_entry_id: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
union RawNodeU {
    string: *mut c_char,
    flag: c_int,
    int64: i64,
    double: f64,
    list: *mut RawNodeList,
    ba: *mut c_void,
}

#[repr(C)]
struct RawNode {
    u: RawNodeU,
    format: c_int,
}

#[repr(C)]
struct RawNodeList {
    num: c_int,
    values: *mut RawNode,
    keys: *mut *mut c_char,
}

/// Custom stream protocol callbacks (`mpv_stream_cb_info`).
#[repr(C)]
pub struct StreamCbInfo {
    pub cookie: *mut c_void,
    pub read_fn: Option<unsafe extern "C" fn(*mut c_void, *mut c_char, u64) -> i64>,
    pub seek_fn: Option<unsafe extern "C" fn(*mut c_void, i64) -> i64>,
    pub size_fn: Option<unsafe extern "C" fn(*mut c_void) -> i64>,
    pub close_fn: Option<unsafe extern "C" fn(*mut c_void)>,
    pub cancel_fn: Option<unsafe extern "C" fn(*mut c_void)>,
}

pub type StreamOpenFn = unsafe extern "C" fn(*mut c_void, *mut c_char, *mut StreamCbInfo) -> c_int;

pub const FORMAT_NONE: c_int = 0;
pub const FORMAT_STRING: c_int = 1;
pub const FORMAT_FLAG: c_int = 3;
pub const FORMAT_INT64: c_int = 4;
pub const FORMAT_DOUBLE: c_int = 5;
pub const FORMAT_NODE: c_int = 6;
const FORMAT_NODE_ARRAY: c_int = 7;
const FORMAT_NODE_MAP: c_int = 8;

const EVENT_SHUTDOWN: c_int = 1;
const EVENT_LOG_MESSAGE: c_int = 2;
const EVENT_SET_PROPERTY_REPLY: c_int = 4;
const EVENT_COMMAND_REPLY: c_int = 5;
const EVENT_START_FILE: c_int = 6;
const EVENT_END_FILE: c_int = 7;
const EVENT_FILE_LOADED: c_int = 8;
const EVENT_VIDEO_RECONFIG: c_int = 17;
const EVENT_AUDIO_RECONFIG: c_int = 18;
const EVENT_SEEK: c_int = 20;
const EVENT_PLAYBACK_RESTART: c_int = 21;
const EVENT_PROPERTY_CHANGE: c_int = 22;

pub const END_EOF: i32 = 0;
pub const END_STOP: i32 = 2;
pub const END_QUIT: i32 = 3;
pub const END_ERROR: i32 = 4;
pub const END_REDIRECT: i32 = 5;

#[allow(non_snake_case)]
struct Api {
    create: unsafe extern "C" fn() -> *mut RawHandle,
    initialize: unsafe extern "C" fn(*mut RawHandle) -> c_int,
    terminate_destroy: unsafe extern "C" fn(*mut RawHandle),
    set_option_string: unsafe extern "C" fn(*mut RawHandle, *const c_char, *const c_char) -> c_int,
    set_property_string: unsafe extern "C" fn(*mut RawHandle, *const c_char, *const c_char) -> c_int,
    get_property_string: unsafe extern "C" fn(*mut RawHandle, *const c_char) -> *mut c_char,
    command_async: unsafe extern "C" fn(*mut RawHandle, u64, *mut *const c_char) -> c_int,
    set_property_async: unsafe extern "C" fn(*mut RawHandle, u64, *const c_char, c_int, *mut c_void) -> c_int,
    observe_property: unsafe extern "C" fn(*mut RawHandle, u64, *const c_char, c_int) -> c_int,
    wait_event: unsafe extern "C" fn(*mut RawHandle, f64) -> *mut RawEvent,
    set_wakeup_callback: unsafe extern "C" fn(*mut RawHandle, Option<unsafe extern "C" fn(*mut c_void)>, *mut c_void),
    request_log_messages: unsafe extern "C" fn(*mut RawHandle, *const c_char) -> c_int,
    free: unsafe extern "C" fn(*mut c_void),
    error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    client_api_version: unsafe extern "C" fn() -> u32,
    stream_cb_add_ro: unsafe extern "C" fn(*mut RawHandle, *const c_char, *mut c_void, StreamOpenFn) -> c_int,
}

unsafe impl Send for Api {}
unsafe impl Sync for Api {}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

fn load_api() -> Result<Api, String> {
    let path = gt_core::paths::deps_dir().join("mpv").join("libmpv-2.dll");
    let module: HMODULE = unsafe { LoadLibraryExW(&HSTRING::from(path.as_os_str()), None, LOAD_WITH_ALTERED_SEARCH_PATH) }
        .map_err(|e| format!("cannot load {}: {e}", path.display()))?;
    macro_rules! sym {
        ($name:literal) => {{
            let p = unsafe { GetProcAddress(module, s!($name)) }.ok_or(concat!("libmpv lacks ", $name))?;
            #[allow(clippy::missing_transmute_annotations)]
            unsafe {
                std::mem::transmute(p)
            }
        }};
    }
    Ok(Api {
        create: sym!("mpv_create"),
        initialize: sym!("mpv_initialize"),
        terminate_destroy: sym!("mpv_terminate_destroy"),
        set_option_string: sym!("mpv_set_option_string"),
        set_property_string: sym!("mpv_set_property_string"),
        get_property_string: sym!("mpv_get_property_string"),
        command_async: sym!("mpv_command_async"),
        set_property_async: sym!("mpv_set_property_async"),
        observe_property: sym!("mpv_observe_property"),
        wait_event: sym!("mpv_wait_event"),
        set_wakeup_callback: sym!("mpv_set_wakeup_callback"),
        request_log_messages: sym!("mpv_request_log_messages"),
        free: sym!("mpv_free"),
        error_string: sym!("mpv_error_string"),
        client_api_version: sym!("mpv_client_api_version"),
        stream_cb_add_ro: sym!("mpv_stream_cb_add_ro"),
    })
}

fn api() -> Result<&'static Api, String> {
    API.get_or_init(load_api).as_ref().map_err(|e| e.clone())
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

unsafe fn from_c(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// A decoded `mpv_node`.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Node {
    #[default]
    None,
    Str(String),
    Flag(bool),
    Int(i64),
    Double(f64),
    List(Vec<Node>),
    Map(Vec<(String, Node)>),
}

impl Node {
    pub fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Map(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Node::Str(s)) => Some(s),
            _ => None,
        }
    }
    pub fn int(&self, key: &str) -> Option<i64> {
        match self.get(key) {
            Some(Node::Int(i)) => Some(*i),
            Some(Node::Double(d)) => Some(*d as i64),
            _ => None,
        }
    }
    pub fn num(&self, key: &str) -> Option<f64> {
        match self.get(key) {
            Some(Node::Double(d)) => Some(*d),
            Some(Node::Int(i)) => Some(*i as f64),
            _ => None,
        }
    }
    pub fn flag(&self, key: &str) -> bool {
        matches!(self.get(key), Some(Node::Flag(true)))
    }
    pub fn list(&self) -> &[Node] {
        match self {
            Node::List(v) => v,
            _ => &[],
        }
    }
}

unsafe fn read_node(n: &RawNode) -> Node {
    unsafe {
        match n.format {
            FORMAT_STRING => Node::Str(from_c(n.u.string)),
            FORMAT_FLAG => Node::Flag(n.u.flag != 0),
            FORMAT_INT64 => Node::Int(n.u.int64),
            FORMAT_DOUBLE => Node::Double(n.u.double),
            FORMAT_NODE_ARRAY | FORMAT_NODE_MAP => {
                let l = n.u.list;
                if l.is_null() {
                    return Node::None;
                }
                let l = &*l;
                let vals: Vec<Node> = (0..l.num.max(0) as usize).map(|i| read_node(&*l.values.add(i))).collect();
                if n.format == FORMAT_NODE_MAP && !l.keys.is_null() {
                    let keys = (0..vals.len()).map(|i| from_c(*l.keys.add(i)));
                    Node::Map(keys.zip(vals).collect())
                } else {
                    Node::List(vals)
                }
            }
            _ => Node::None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    Shutdown,
    Log { prefix: String, level: String, text: String },
    StartFile { entry: i64 },
    EndFile { reason: i32, error: i32, entry: i64 },
    FileLoaded,
    VideoReconfig,
    AudioReconfig,
    Seek,
    PlaybackRestart,
    Property { id: u64, name: String, value: Node },
    Reply { id: u64, error: i32, result: Node },
    Other(i32),
}

/// An mpv core. Every method is thread-safe (libmpv locks internally).
pub struct Mpv {
    h: *mut RawHandle,
    api: &'static Api,
}

unsafe impl Send for Mpv {}
unsafe impl Sync for Mpv {}

impl Mpv {
    pub fn create() -> Result<Mpv, String> {
        let api = api()?;
        let v = unsafe { (api.client_api_version)() };
        if v >> 16 != 2 {
            return Err(format!("unsupported libmpv client API {}.{}", v >> 16, v & 0xFFFF));
        }
        let h = unsafe { (api.create)() };
        if h.is_null() {
            return Err("mpv_create failed".into());
        }
        Ok(Mpv { h, api })
    }

    pub fn error_string(&self, e: c_int) -> String {
        unsafe { from_c((self.api.error_string)(e)) }
    }

    fn check(&self, r: c_int, what: &str) -> Result<(), String> {
        if r < 0 {
            Err(format!("{what}: {}", self.error_string(r)))
        } else {
            Ok(())
        }
    }

    pub fn set_option(&self, name: &str, value: &str) -> Result<(), String> {
        let (n, v) = (cstr(name), cstr(value));
        self.check(unsafe { (self.api.set_option_string)(self.h, n.as_ptr(), v.as_ptr()) }, name)
    }

    pub fn initialize(&self) -> Result<(), String> {
        self.check(unsafe { (self.api.initialize)(self.h) }, "mpv_initialize")
    }

    /// Synchronous; never call from the UI thread once a video window exists.
    pub fn set_property_sync(&self, name: &str, value: &str) -> Result<(), String> {
        let (n, v) = (cstr(name), cstr(value));
        self.check(unsafe { (self.api.set_property_string)(self.h, n.as_ptr(), v.as_ptr()) }, name)
    }

    /// Synchronous; never call from the UI thread once a video window exists.
    pub fn get_property_sync(&self, name: &str) -> Option<String> {
        let n = cstr(name);
        unsafe {
            let p = (self.api.get_property_string)(self.h, n.as_ptr());
            if p.is_null() {
                return None;
            }
            let s = from_c(p);
            (self.api.free)(p as _);
            Some(s)
        }
    }

    pub fn command_async(&self, id: u64, args: &[&str]) -> Result<(), String> {
        let owned: Vec<CString> = args.iter().map(|a| cstr(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        self.check(unsafe { (self.api.command_async)(self.h, id, ptrs.as_mut_ptr()) }, args.first().copied().unwrap_or("command"))
    }

    pub fn set_string_async(&self, name: &str, value: &str) {
        let (n, v) = (cstr(name), cstr(value));
        let mut p = v.as_ptr();
        let r = unsafe { (self.api.set_property_async)(self.h, 0, n.as_ptr(), FORMAT_STRING, &mut p as *mut _ as _) };
        if r < 0 {
            tracing::warn!("mpv set {name}={value}: {}", self.error_string(r));
        }
    }

    pub fn set_flag_async(&self, name: &str, value: bool) {
        let n = cstr(name);
        let mut v: c_int = value as c_int;
        unsafe { (self.api.set_property_async)(self.h, 0, n.as_ptr(), FORMAT_FLAG, &mut v as *mut _ as _) };
    }

    pub fn set_double_async(&self, name: &str, value: f64) {
        let n = cstr(name);
        let mut v = value;
        unsafe { (self.api.set_property_async)(self.h, 0, n.as_ptr(), FORMAT_DOUBLE, &mut v as *mut _ as _) };
    }

    pub fn observe(&self, id: u64, name: &str, format: c_int) {
        let n = cstr(name);
        unsafe { (self.api.observe_property)(self.h, id, n.as_ptr(), format) };
    }

    pub fn request_log(&self, level: &str) {
        let l = cstr(level);
        unsafe { (self.api.request_log_messages)(self.h, l.as_ptr()) };
    }

    /// `cb(ctx)` is called from arbitrary mpv threads whenever events are queued.
    pub fn set_wakeup(&self, cb: unsafe extern "C" fn(*mut c_void), ctx: *mut c_void) {
        unsafe { (self.api.set_wakeup_callback)(self.h, Some(cb), ctx) };
    }

    pub fn add_stream_protocol(&self, protocol: &str, user: *mut c_void, open: StreamOpenFn) -> Result<(), String> {
        let p = cstr(protocol);
        self.check(unsafe { (self.api.stream_cb_add_ro)(self.h, p.as_ptr(), user, open) }, "stream_cb_add_ro")
    }

    /// Next queued event, without waiting when `timeout` is 0.
    pub fn wait_event(&self, timeout: f64) -> Option<Event> {
        unsafe {
            let e = (self.api.wait_event)(self.h, timeout);
            if e.is_null() {
                return None;
            }
            let e = &*e;
            Some(match e.event_id {
                0 => return None,
                EVENT_SHUTDOWN => Event::Shutdown,
                EVENT_LOG_MESSAGE => {
                    let m = &*(e.data as *const RawLogMessage);
                    Event::Log { prefix: from_c(m.prefix), level: from_c(m.level), text: from_c(m.text).trim_end().to_string() }
                }
                EVENT_START_FILE => Event::StartFile { entry: (*(e.data as *const RawStartFile)).playlist_entry_id },
                EVENT_END_FILE => {
                    let m = &*(e.data as *const RawEndFile);
                    Event::EndFile { reason: m.reason, error: m.error, entry: m.playlist_entry_id }
                }
                EVENT_FILE_LOADED => Event::FileLoaded,
                EVENT_VIDEO_RECONFIG => Event::VideoReconfig,
                EVENT_AUDIO_RECONFIG => Event::AudioReconfig,
                EVENT_SEEK => Event::Seek,
                EVENT_PLAYBACK_RESTART => Event::PlaybackRestart,
                EVENT_PROPERTY_CHANGE => {
                    let p = &*(e.data as *const RawEventProperty);
                    let value = if p.data.is_null() {
                        Node::None
                    } else {
                        match p.format {
                            FORMAT_FLAG => Node::Flag(*(p.data as *const c_int) != 0),
                            FORMAT_INT64 => Node::Int(*(p.data as *const i64)),
                            FORMAT_DOUBLE => Node::Double(*(p.data as *const f64)),
                            FORMAT_STRING => Node::Str(from_c(*(p.data as *const *const c_char))),
                            FORMAT_NODE => read_node(&*(p.data as *const RawNode)),
                            _ => Node::None,
                        }
                    };
                    Event::Property { id: e.reply_userdata, name: from_c(p.name), value }
                }
                EVENT_COMMAND_REPLY => {
                    let result = if e.data.is_null() { Node::None } else { read_node(&*(e.data as *const RawNode)) };
                    Event::Reply { id: e.reply_userdata, error: e.error, result }
                }
                EVENT_SET_PROPERTY_REPLY => Event::Reply { id: e.reply_userdata, error: e.error, result: Node::None },
                other => Event::Other(other),
            })
        }
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        unsafe { (self.api.terminate_destroy)(self.h) };
    }
}

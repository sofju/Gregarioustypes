//! Agent <-> viewer protocol over two one-way named pipes (one per direction, so a
//! blocking read never serializes behind a write on the same synchronous handle).
//! Frames are a little-endian u32 length followed by JSON.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_SHARE_NONE, OPEN_EXISTING,
    PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND,
};
use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RectI {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl RectI {
    pub fn w(&self) -> i32 {
        self.r - self.l
    }
    pub fn h(&self) -> i32 {
        self.b - self.t
    }
}

/// One selected shell item.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// File-system path, or the shell parsing name for virtual items.
    pub path: String,
    /// False when the item has no file-system path (inside a zip folder, a phone, ...).
    pub filesystem: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ToViewer {
    /// Open the preview. `anchor` is the item's icon rectangle on screen, if known.
    Show { items: Vec<Item>, index: usize, fullscreen: bool, anchor: Option<RectI>, monitor: Option<RectI>, crashed: Option<String> },
    /// The Explorer selection changed while the preview is visible.
    Update { items: Vec<Item>, index: usize, anchor: Option<RectI> },
    /// Step within a multi-selection (from the hook while Explorer has focus).
    Step(i32),
    /// Items next to the selection in view order, to decode ahead of time.
    Prefetch(Vec<Item>),
    Hide,
    ToggleFullscreen,
    /// Copy / Ctrl+C style command routed from the hook (reserved).
    Command(String),
    Quit,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ToAgent {
    Ready { pid: u32 },
    /// Visibility and whether the window currently shows a multi-selection.
    Visible { visible: bool, multi: bool, pinned: bool, hwnd: isize },
    /// Ask the agent to move the Explorer selection by `delta` items (viewer had focus).
    Navigate(i32),
    /// The file currently being loaded, so a crash can be attributed to it.
    Loading { path: String },
}

pub fn pipe_names(tag: &str) -> (String, String) {
    (format!(r"\\.\pipe\Gregarioustypes.{tag}.a2v"), format!(r"\\.\pipe\Gregarioustypes.{tag}.v2a"))
}

struct Handle(HANDLE);
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn last_err() -> io::Error {
    io::Error::from_raw_os_error(unsafe { GetLastError().0 } as i32)
}

pub struct Sender {
    h: Handle,
}

pub struct Receiver {
    h: Handle,
}

fn write_all(h: HANDLE, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        let mut n = 0u32;
        unsafe { WriteFile(h, Some(buf), Some(&mut n), None) }.map_err(|e| io::Error::other(e.message()))?;
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        buf = &buf[n as usize..];
    }
    Ok(())
}

fn read_exact(h: HANDLE, mut buf: &mut [u8]) -> io::Result<()> {
    while !buf.is_empty() {
        let mut n = 0u32;
        unsafe { ReadFile(h, Some(buf), Some(&mut n), None) }.map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf = &mut buf[n as usize..];
    }
    Ok(())
}

impl Sender {
    pub fn send<T: Serialize>(&self, msg: &T) -> io::Result<()> {
        let body = serde_json::to_vec(msg).map_err(io::Error::other)?;
        let mut frame = Vec::with_capacity(body.len() + 4);
        frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
        frame.extend_from_slice(&body);
        write_all(self.h.0, &frame)
    }
}

impl Receiver {
    pub fn recv<T: DeserializeOwned>(&self) -> io::Result<T> {
        let mut len = [0u8; 4];
        read_exact(self.h.0, &mut len)?;
        let len = u32::from_le_bytes(len) as usize;
        if len > 64 * 1024 * 1024 {
            return Err(io::Error::other("frame too large"));
        }
        let mut body = vec![0u8; len];
        read_exact(self.h.0, &mut body)?;
        serde_json::from_slice(&body).map_err(io::Error::other)
    }
}

fn create_server(name: &str, access: FILE_FLAGS_AND_ATTRIBUTES) -> io::Result<Handle> {
    let h = unsafe {
        CreateNamedPipeW(
            &HSTRING::from(name),
            access | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            64 * 1024,
            64 * 1024,
            0,
            None,
        )
    };
    if h == INVALID_HANDLE_VALUE || h.is_invalid() {
        return Err(last_err());
    }
    Ok(Handle(h))
}

/// Server side (agent): pipes created before the viewer is spawned.
pub struct ServerPipes {
    a2v: Handle,
    v2a: Handle,
}

impl ServerPipes {
    pub fn create(tag: &str) -> io::Result<Self> {
        let (a2v, v2a) = pipe_names(tag);
        Ok(Self { a2v: create_server(&a2v, PIPE_ACCESS_OUTBOUND)?, v2a: create_server(&v2a, PIPE_ACCESS_INBOUND)? })
    }

    /// Blocks until the viewer has opened both ends.
    pub fn accept(self) -> io::Result<(Sender, Receiver)> {
        for h in [&self.a2v, &self.v2a] {
            if let Err(e) = unsafe { ConnectNamedPipe(h.0, None) } {
                if e.code() != ERROR_PIPE_CONNECTED.to_hresult() {
                    return Err(io::Error::other(e.message()));
                }
            }
        }
        Ok((Sender { h: self.a2v }, Receiver { h: self.v2a }))
    }
}

/// Client side (viewer).
pub fn connect(tag: &str) -> io::Result<(Receiver, Sender)> {
    let (a2v, v2a) = pipe_names(tag);
    let open = |name: &str, access: u32| -> io::Result<Handle> {
        let h = unsafe {
            CreateFileW(PCWSTR(HSTRING::from(name).as_ptr()), access, FILE_SHARE_NONE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None)
        }
        .map_err(|e| io::Error::other(e.message()))?;
        Ok(Handle(h))
    };
    let rx = open(&a2v, GENERIC_READ.0)?;
    let tx = open(&v2a, GENERIC_WRITE.0)?;
    Ok((Receiver { h: rx }, Sender { h: tx }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let tag = format!("test.{}", std::process::id());
        let server = ServerPipes::create(&tag).expect("create");
        let t = std::thread::spawn(move || server.accept().expect("accept"));
        let (rx, tx) = connect(&tag).expect("connect");
        let (stx, srx) = t.join().unwrap();
        stx.send(&ToViewer::Step(3)).unwrap();
        match rx.recv::<ToViewer>().unwrap() {
            ToViewer::Step(3) => {}
            m => panic!("unexpected {m:?}"),
        }
        tx.send(&ToAgent::Navigate(-1)).unwrap();
        assert!(matches!(srx.recv::<ToAgent>().unwrap(), ToAgent::Navigate(-1)));
    }
}

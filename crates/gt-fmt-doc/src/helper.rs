//! Helper processes (Ghostscript, LibreOffice): no console window, members of a
//! kill-on-close job so they never outlive the viewer, cancellable, with a timeout.

use gt_core::CancelToken;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

struct Job(HANDLE);
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

fn job() -> Option<HANDLE> {
    static JOB: OnceLock<Option<Job>> = OnceLock::new();
    JOB.get_or_init(|| unsafe {
        let h = CreateJobObjectW(None, None).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(h, JobObjectExtendedLimitInformation, &info as *const _ as *const _, std::mem::size_of_val(&info) as u32).ok()?;
        Some(Job(h))
    })
    .as_ref()
    .map(|j| j.0)
}

/// Spawn `cmd` hidden, inside the viewer's job.
pub fn spawn(cmd: &mut Command, low_priority: bool) -> std::io::Result<Child> {
    let flags = CREATE_NO_WINDOW | if low_priority { BELOW_NORMAL_PRIORITY_CLASS } else { 0 };
    let child = cmd.creation_flags(flags).spawn()?;
    if let Some(j) = job() {
        unsafe {
            let _ = AssignProcessToJobObject(j, HANDLE(child.as_raw_handle()));
        }
    }
    Ok(child)
}

/// Run to completion; returns stderr+stdout text on failure.
pub fn run(cmd: &mut Command, cancel: &CancelToken, timeout: Duration) -> Result<(), String> {
    let log = std::env::temp_dir().join(format!("gt-helper-{}-{}.log", std::process::id(), rand_id()));
    let out = std::fs::File::create(&log).map_err(|e| e.to_string())?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    cmd.stdin(Stdio::null()).stdout(out).stderr(err);
    let mut child = spawn(cmd, false).map_err(|e| e.to_string())?;
    let t = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if cancel.is_cancelled() || t.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&log);
            return Err(if cancel.is_cancelled() { "cancelled".into() } else { "timed out".into() });
        }
        std::thread::sleep(Duration::from_millis(15));
    };
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&log);
    if status.success() {
        Ok(())
    } else {
        let msg: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).take(6).collect();
        Err(if msg.is_empty() { format!("exit code {}", status.code().unwrap_or(-1)) } else { msg.join("\n") })
    }
}

fn rand_id() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(Instant::now().elapsed().as_nanos() as u64);
    h.finish()
}

/// Cache file for a conversion of `src`, keyed by path, size, mtime and `salt`.
pub fn cache_file(dir: &str, src: &Path, salt: &str, ext: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    if let Ok(m) = std::fs::metadata(src) {
        m.len().hash(&mut h);
        m.modified().ok().hash(&mut h);
    }
    salt.hash(&mut h);
    let d = gt_core::paths::cache_dir().join(dir);
    let _ = std::fs::create_dir_all(&d);
    d.join(format!("{:016x}.{ext}", h.finish()))
}

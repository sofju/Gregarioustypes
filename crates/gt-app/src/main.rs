#![windows_subsystem = "windows"]
//! Gregarioustypes: Space-bar file previews for Windows, in the spirit of macOS Quick Look.
//!
//! * no arguments          → agent (tray, Space hook, keeps a warm viewer process)
//! * `--viewer --tag <t>`  → viewer process (started by the agent)
//! * `--open <file>...`    → standalone preview window, no agent needed
//! * `--hdr-test`          → standalone HDR test pattern
//! * `--settings`          → the settings window
//! * `--quit`              → close a running agent (and its viewer) and settings window

mod agent;
mod settings_ui;
mod viewer;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(i) = args.iter().position(|a| a == "--viewer") {
        let tag = args.iter().skip(i).position(|a| a == "--tag").and_then(|j| args.get(i + j + 1)).cloned().unwrap_or_default();
        viewer::run(viewer::Mode::Agent(tag));
    } else if args.iter().any(|a| a == "--quit") {
        quit_running();
    } else if let Some(i) = args.iter().position(|a| a == "--settings") {
        settings_ui::run(args.get(i + 1).map(|s| s.as_str()).unwrap_or(""));
    } else if args.iter().any(|a| a == "--hdr-test") {
        viewer::run(viewer::Mode::HdrTest);
    } else if let Some(i) = args.iter().position(|a| a == "--open") {
        let files: Vec<String> = args[i + 1..]
            .iter()
            .map(|p| std::fs::canonicalize(p).map(|c| c.display().to_string().trim_start_matches(r"\\?\").to_string()).unwrap_or_else(|_| p.clone()))
            .collect();
        if !files.is_empty() {
            viewer::run(viewer::Mode::Standalone(files));
        }
    } else {
        agent::run();
    }
}

/// Close the running agent (it stops its viewer) and the settings window, and
/// wait for them to go (installer, uninstaller, updates).
fn quit_running() {
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE};
    let classes = [w!("Gregarioustypes.Agent"), settings_ui::CLASS];
    for c in classes {
        if let Ok(h) = unsafe { FindWindowW(c, PCWSTR::null()) } {
            unsafe {
                let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }
    let t = std::time::Instant::now();
    while t.elapsed() < std::time::Duration::from_secs(5) {
        if classes.iter().all(|c| unsafe { FindWindowW(*c, PCWSTR::null()) }.is_err()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // The viewer exits when the agent's pipe closes; give it a moment.
    std::thread::sleep(std::time::Duration::from_millis(500));
}

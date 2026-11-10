//! Compile the HLSL shaders with fxc from the Windows SDK.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fxc() -> PathBuf {
    if let Ok(p) = std::env::var("FXC") {
        return PathBuf::from(p);
    }
    let base = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(base).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.join("x64").join("fxc.exe").is_file()).collect()).unwrap_or_default();
    versions.sort();
    versions.pop().map(|v| v.join("x64").join("fxc.exe")).expect("fxc.exe not found (install the Windows SDK or set FXC)")
}

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let src = Path::new("shaders").join("image.hlsl");
    let fxc = fxc();
    for (profile, entry, file) in [("vs_5_0", "vs_main", "image_vs.cso"), ("ps_5_0", "ps_main", "image_ps.cso")] {
        let status = Command::new(&fxc)
            .args(["/nologo", "/O3", "/T", profile, "/E", entry, "/Fo"])
            .arg(out.join(file))
            .arg(&src)
            .status()
            .expect("run fxc");
        assert!(status.success(), "fxc failed for {entry}");
    }
    println!("cargo:rerun-if-changed=shaders/image.hlsl");
}

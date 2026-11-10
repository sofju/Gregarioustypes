//! Probe every file under the given folders with MediaInfo and time it.
//! Usage: cargo run --release -p gt-fmt-media --example miprobe -- samples/media

use std::path::Path;
use std::time::Instant;

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn main() {
    let t = Instant::now();
    gt_fmt_media::mediainfo::preload();
    println!("preload {:.1} ms, {}", t.elapsed().as_secs_f64() * 1000.0, gt_fmt_media::mediainfo::version());
    let mut files = Vec::new();
    for a in std::env::args().skip(1) {
        walk(Path::new(&a), &mut files);
    }
    files.sort();
    for f in files {
        let t = Instant::now();
        match gt_fmt_media::probe::probe(&f) {
            Ok(s) => {
                let v = s.video.iter().map(|v| v.describe()).collect::<Vec<_>>().join(" | ");
                let a = s.audio.iter().map(|a| a.describe()).collect::<Vec<_>>().join(" | ");
                println!(
                    "{:6.1} ms  {:28} {:12} {:7.2}s  V[{}] A[{}] cover={} hdr={}",
                    t.elapsed().as_secs_f64() * 1000.0,
                    f.file_name().unwrap().to_string_lossy(),
                    s.container,
                    s.duration,
                    v,
                    a,
                    s.cover.as_ref().map(|c| c.len()).unwrap_or(0),
                    s.video.first().map(|v| v.hdr_kind()).unwrap_or("")
                );
            }
            Err(e) => println!("{:6.1} ms  {:28} ERROR {e}", t.elapsed().as_secs_f64() * 1000.0, f.file_name().unwrap().to_string_lossy()),
        }
    }
    if let Ok(p) = std::env::var("MI_REPORT") {
        let t = Instant::now();
        let r = gt_fmt_media::mediainfo::report(Path::new(&p), std::env::var("MI_FULL").is_ok()).unwrap();
        println!("--- report in {:.1} ms ---\n{r}", t.elapsed().as_secs_f64() * 1000.0);
    }
}

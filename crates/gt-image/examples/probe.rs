//! Decode every image under a folder and print what the decoder found.
//! cargo run -p gt-image --release --example probe -- samples/images [--full]

use gt_image::{codecs, util, DecodeOptions, Pixels};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut v: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    v.sort();
    for p in v {
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = PathBuf::from(args.first().cloned().unwrap_or_else(|| "samples/images".into()));
    let full = args.iter().any(|a| a == "--full");
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
    }
    let mut files = Vec::new();
    walk(&root, &mut files);
    let opts = DecodeOptions { full, ..Default::default() };
    let (mut ok, mut failed, mut skipped) = (0, 0, 0);
    for f in files {
        let head = gt_image_head(&f);
        let ext = f.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let Some((kind, _)) = codecs::detect(&head, &ext) else {
            skipped += 1;
            continue;
        };
        let t = Instant::now();
        let name = f.strip_prefix(&root).unwrap_or(&f).display().to_string();
        match codecs::decode(&f, kind, &opts) {
            Ok(img) => {
                ok += 1;
                let fmt = match img.pixels {
                    Pixels::Rgba8(_) => "rgba8",
                    Pixels::Rgba16(_) => "rgba16",
                    Pixels::RgbaF16(_) => "f16",
                };
                let gm = img.gain_map.as_ref().map(|g| format!(" gainmap[{} {}x{} max×{:.2}]", g.label, g.width, g.height, g.max_boost())).unwrap_or_default();
                let reduced = img.full_size.map(|(w, h)| format!(" (full {w}x{h})")).unwrap_or_default();
                println!(
                    "OK   {name:<55} {:?} {}x{}{reduced} {fmt} | {} | {} | peak {:.0} nits | o={} frames={}/{}{gm} | {:.1} ms",
                    kind,
                    img.width,
                    img.height,
                    img.color.name,
                    img.hdr_label().unwrap_or_else(|| "SDR".into()),
                    util::content_peak_nits(&img),
                    img.orientation,
                    img.frames.len() + 1,
                    img.frame_count,
                    t.elapsed().as_secs_f64() * 1000.0
                );
            }
            Err(e) => {
                failed += 1;
                println!("FAIL {name:<55} {kind:?}: {e}");
            }
        }
    }
    println!("\n{ok} decoded, {failed} failed, {skipped} not images");
    if failed > 0 {
        std::process::exit(1);
    }
}

fn gt_image_head(p: &Path) -> Vec<u8> {
    use std::io::Read;
    let mut b = vec![0u8; 4096];
    let n = std::fs::File::open(p).and_then(|mut f| f.read(&mut b)).unwrap_or(0);
    b.truncate(n);
    b
}

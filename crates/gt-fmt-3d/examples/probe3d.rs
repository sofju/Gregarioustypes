//! Headless check of the 3D converters: converts each file to GLB, prints the
//! facts and timings, and writes `<stem>.glb` into `--out` (for gltf-validator).
//! Usage: probe3d [--out DIR] files...

use gt_core::CancelToken;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut out: Option<PathBuf> = None;
    if let Some(i) = args.iter().position(|a| a == "--out") {
        out = Some(PathBuf::from(args.remove(i + 1)));
        args.remove(i);
    }
    let cancel = CancelToken::new();
    for a in args {
        let p = PathBuf::from(&a);
        let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        println!("== {a}");
        if ext == "blend" {
            let head = std::fs::read(&p).map(|b| b[..b.len().min(65536)].to_vec()).unwrap_or_default();
            if let Some((d, comp)) = gt_fmt_3d::blend::uncompressed_head(&p, &head, 1 << 20) {
                let h = gt_fmt_3d::blend::header(&d).unwrap();
                let t = gt_fmt_3d::blend::thumbnail(&d, &h);
                println!("   Blender {} ({comp:?}), preview {:?}", h.version, t.as_ref().map(|t| (t.w, t.h)));
                if let (Some(t), Some(o)) = (t, &out) {
                    // BGRA premultiplied → PPM on black.
                    let mut ppm = format!("P6 {} {} 255\n", t.w, t.h).into_bytes();
                    for px in t.bgra.chunks_exact(4) {
                        ppm.extend([px[2], px[1], px[0]]);
                    }
                    let stem = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
                    let _ = std::fs::write(o.join(format!("{stem}-thumb.ppm")), ppm);
                }
            }
        }
        let t = Instant::now();
        let r = match ext.as_str() {
            "stl" => std::fs::read(&p).map_err(|e| e.to_string()).and_then(|d| gt_fmt_3d::stl::convert(&d, &cancel)),
            "fbx" | "obj" => gt_fmt_3d::fbx::convert(&p, &cancel),
            "abc" => gt_fmt_3d::abc::convert(&p, &cancel),
            "blend" => match gt_fmt_3d::blend::to_glb(&p, &cancel, &|s| println!("   … {s}")) {
                Ok((glb, facts)) => std::fs::read(&glb).map(|b| gt_fmt_3d::Model { glb: b, facts, unit_meters: Some(1.0) }).map_err(|e| e.to_string()),
                Err(e) => Err(e),
            },
            _ => {
                println!("   (loaded by the page)");
                continue;
            }
        };
        match r {
            Ok(m) => {
                println!("   {} in {:.0} ms", gt_core::fmt::size(m.glb.len() as u64), t.elapsed().as_secs_f64() * 1000.0);
                for (k, v) in &m.facts {
                    println!("   {k}: {v}");
                }
                if let Some(o) = &out {
                    let stem = p.file_stem().unwrap_or_default().to_string_lossy().replace(' ', "_");
                    let f = o.join(format!("{stem}.glb"));
                    let _ = std::fs::write(&f, &m.glb);
                    println!("   → {}", f.display());
                }
            }
            Err(e) => println!("   ERROR {e}"),
        }
    }
}

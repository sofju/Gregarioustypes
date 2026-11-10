//! Headless check of design files: facts, previews and rendered pages
//! (written as PPM next to `--out`, for eyeballing).
//! Usage: designprobe [--out DIR] [--pages N] files...

use gt_core::CancelToken;
use gt_fmt_doc::pages::{PageSource, Raster};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn save(r: &Raster, path: &Path) {
    let mut out = format!("P6 {} {} 255\n", r.w, r.h).into_bytes();
    for px in r.data.chunks_exact(4) {
        // Premultiplied BGRA on white.
        let a = px[3] as u32;
        out.extend([(px[2] as u32 + 255 - a).min(255) as u8, (px[1] as u32 + 255 - a).min(255) as u8, (px[0] as u32 + 255 - a).min(255) as u8]);
    }
    let _ = std::fs::write(path, out);
}

fn render(src: &dyn PageSource, page: usize, height: f32) -> Option<(Raster, f64)> {
    let (w, h) = src.size(page);
    let k = height / h;
    let t = Instant::now();
    let r = src.render(page, k, 0, 0, (w * k).round() as u32, (h * k).round() as u32)?;
    Some((r, t.elapsed().as_secs_f64() * 1000.0))
}

fn main() {
    let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = std::env::temp_dir();
    let mut pages = 2usize;
    if let Some(i) = args.iter().position(|a| a == "--out") {
        out = PathBuf::from(args.remove(i + 1));
        args.remove(i);
    }
    if let Some(i) = args.iter().position(|a| a == "--pages") {
        pages = args.remove(i + 1).parse().unwrap_or(2);
        args.remove(i);
    }
    let cancel = CancelToken::new();
    for a in args {
        let p = PathBuf::from(&a);
        let stem = p.file_stem().unwrap_or_default().to_string_lossy().replace(' ', "_");
        let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let data = std::fs::read(&p).unwrap_or_default();
        println!("== {a}");
        let t = Instant::now();
        let src: Option<Box<dyn PageSource>> = match ext.as_str() {
            "idml" => match gt_fmt_design::idml::load(&p, &cancel) {
                Ok((s, facts)) => {
                    println!("   parsed in {:.0} ms: {:?}", t.elapsed().as_secs_f64() * 1000.0, facts);
                    Some(Box::new(ArcSrc(s)))
                }
                Err(e) => {
                    println!("   ERROR {e}");
                    None
                }
            },
            "indd" => {
                let d = gt_fmt_design::indd::parse(&data);
                match d {
                    Some(d) => {
                        println!("   {} pages, {:?} pt, {} previews in {:.0} ms", d.pages, d.page_size, d.thumbs.len(), t.elapsed().as_secs_f64() * 1000.0);
                        for (k, v) in &d.facts {
                            println!("   {k}: {}", v.chars().take(140).collect::<String>());
                        }
                    }
                    None => println!("   no InDesign XMP"),
                }
                None
            }
            "xar" | "web" | "wix" | "art" => {
                match gt_fmt_design::xara::parse(&data) {
                    Some(x) => println!("   strings {:?}, preview {} bytes", x.strings, x.preview.as_ref().map(|p| p.len()).unwrap_or(0)),
                    None => println!("   not Xara"),
                }
                None
            }
            _ => {
                let t = gt_fmt_design::affinity::thumbnail(&data);
                println!("   affinity thumbnail {:?} bytes", t.map(|t| t.len()));
                None
            }
        };
        if let Some(s) = src {
            for i in 0..s.count().min(pages) {
                match render(&*s, i, 1100.0) {
                    Some((r, ms)) => {
                        let f = out.join(format!("{stem}-p{}.ppm", i + 1));
                        save(&r, &f);
                        println!("   page {} {}×{} in {ms:.0} ms → {}", i + 1, r.w, r.h, f.display());
                    }
                    None => println!("   page {} FAILED", i + 1),
                }
            }
        }
    }
}

struct ArcSrc(std::sync::Arc<dyn PageSource>);

impl PageSource for ArcSrc {
    fn count(&self) -> usize {
        self.0.count()
    }
    fn size(&self, p: usize) -> (f32, f32) {
        self.0.size(p)
    }
    fn render(&self, page: usize, scale: f32, x: i32, y: i32, w: u32, h: u32) -> Option<Raster> {
        self.0.render(page, scale, x, y, w, h)
    }
}

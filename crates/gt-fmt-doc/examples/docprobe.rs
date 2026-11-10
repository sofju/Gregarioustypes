//! Headless check of every document sample: opens it with the engine the
//! providers would use, renders the first page, and prints timings.
//! Usage: cargo run --release -p gt-fmt-doc --example docprobe [dir-or-file...]

use gt_core::CancelToken;
use gt_fmt_doc::pages::PageSource;
use gt_fmt_doc::{ghostscript, office, pdf, sheet, svg};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

fn files(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut v: Vec<_> = std::fs::read_dir(p).unwrap().flatten().map(|e| e.path()).collect();
        v.sort();
        for e in v {
            files(&e, out);
        }
    } else {
        out.push(p.to_path_buf());
    }
}

fn ms(t: Instant) -> String {
    format!("{:.0} ms", t.elapsed().as_secs_f64() * 1000.0)
}

fn first_page(src: &dyn PageSource) -> String {
    let (w, h) = src.size(0);
    let s = 1200.0 / w.max(h);
    let t = Instant::now();
    let r = src.render(0, s, 0, 0, (w * s).round() as u32, (h * s).round() as u32);
    match r {
        Some(r) => {
            // Is anything drawn at all (not a blank page)?
            let ink = r.data.chunks_exact(4).filter(|p| p[0] < 200 || p[1] < 200 || p[2] < 200).count();
            format!("page1 {}×{} in {} ({:.1}% ink)", r.w, r.h, ms(t), ink as f64 * 100.0 / (r.w * r.h) as f64)
        }
        None => "page1 FAILED".into(),
    }
}

fn main() {
    let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let roots: Vec<PathBuf> = if args.is_empty() { vec![PathBuf::from("samples/docs")] } else { args.iter().map(PathBuf::from).collect() };
    let mut all = Vec::new();
    for r in &roots {
        files(r, &mut all);
    }
    let cancel = CancelToken::new();
    for f in all {
        let ext = f.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let head = {
            use std::io::Read;
            let mut b = vec![0u8; 65536];
            let n = std::fs::File::open(&f).and_then(|mut x| x.read(&mut b)).unwrap_or(0);
            b.truncate(n);
            b
        };
        let name = f.display().to_string();
        let t = Instant::now();
        let line = match ext.as_str() {
            "pdf" | "ai" if head.windows(5).take(1024).any(|w| w == b"%PDF-") => match pdf::open(&f) {
                Ok(src) => {
                    let open = ms(t);
                    let placeholder = if ext == "ai" && pdf::is_ai_placeholder(&src) { " [AI without PDF content]" } else { "" };
                    let facts = pdf::facts(&src);
                    let title = facts.iter().find(|(k, _)| k == "Title").map(|(_, v)| v.clone()).unwrap_or_default();
                    format!("pdf {} pages, open {open}, {}{placeholder} {title}", src.count(), first_page(&*src))
                }
                Err(e) => format!("pdf ERROR {e}"),
            },
            "eps" | "ps" | "ai" => {
                let kind = if ext == "ps" { ghostscript::Kind::Ps } else { ghostscript::Kind::Eps };
                let prev = ghostscript::dos_eps(&head).map(|d| if ghostscript::dos_preview(&f, &d).is_some() { " +tiff preview" } else { " (preview unreadable)" }).unwrap_or("");
                match ghostscript::to_pdf(&f, kind, &cancel).and_then(|p| pdf::open(&p).map_err(|e| e.to_string())) {
                    Ok(src) => format!("gs→pdf {} pages in {}{prev}, {}", src.count(), ms(t), first_page(&*src)),
                    Err(e) => format!("gs ERROR {e}"),
                }
            }
            _ if head.starts_with(b"{\\rtf") => match gt_fmt_doc::rtf::RtfSource::new(&std::fs::read(&f).unwrap(), &cancel) {
                Ok(src) => format!("rtf {} pages laid out in {}, {}", src.count(), ms(t), first_page(&src)),
                Err(e) => format!("rtf ERROR {e}"),
            },
            "svg" | "svgz" => match svg::open(&f) {
                Ok((src, facts)) => format!("svg {:?} parsed in {}, {}", facts.first().map(|x| &x.1), ms(t), first_page(&*src)),
                Err(e) => format!("svg ERROR {e}"),
            },
            e if sheet::EXTS.contains(&e) => match sheet::load_workbook(&f, &cancel) {
                Ok((sheets, facts)) => {
                    let s0 = &sheets[0];
                    let sample: Vec<String> = (0..s0.range.width().min(4)).map(|c| s0.range.get((1, c)).map(sheet::cell_text).unwrap_or_default()).collect();
                    format!("sheet {} sheets in {}, {:?}, row2 {:?}", sheets.len(), ms(t), facts, sample)
                }
                Err(e) => format!("sheet ERROR {e}"),
            },
            _ if gt_fmt_doc::provider::OFFICE_EXTS.contains(&ext.as_str()) => {
                let pr = gt_fmt_doc::props::read(&f, &head);
                let props = format!("{:?} thumb={} enc={} {:?}", pr.kind, pr.thumbnail.is_some(), pr.encrypted, pr.facts.iter().take(4).collect::<Vec<_>>());
                if pr.encrypted {
                    println!("{name}
      office {props} → password card");
                    continue;
                }
                let t2 = Instant::now();
                match office::to_pdf(&f, &cancel, &|_| {}).and_then(|p| pdf::open(&p).map_err(|e| e.to_string())) {
                    Ok(src) => format!("office {props}\n      → {} pages in {}, {}", src.count(), ms(t2), first_page(&*src)),
                    Err(e) => format!("office {props}\n      → ERROR {e} ({})", ms(t2)),
                }
            }
            _ => continue,
        };
        println!("{name}\n      {line}");
        let _ = Arc::new(());
    }
}

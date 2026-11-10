//! List every archive/disc sample under the given folders (headless check).
use gt_core::CancelToken;
use std::path::Path;
use std::sync::Arc;

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.join("BDMV").is_dir() {
                out.push(p.clone());
            } else if !p.file_name().unwrap().to_string_lossy().starts_with('_') {
                walk(&p, out);
            }
        } else {
            out.push(p);
        }
    }
}

fn main() {
    let mut files = Vec::new();
    for a in std::env::args().skip(1) {
        walk(Path::new(&a), &mut files);
    }
    files.sort();
    let c = CancelToken::new();
    for f in files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let t = std::time::Instant::now();
        let r = if f.is_dir() {
            gt_fmt_archive::listing::open_bd_folder(&f, &c).ok_or_else(|| "not a BD folder".to_string())
        } else {
            let head = gt_core::sniff::read_head(&f, 64 * 1024).unwrap_or_default();
            gt_fmt_archive::listing::open(&f, &head, Arc::new(Default::default()), &c)
        };
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        match r {
            Ok(l) => {
                let files = l.entries.iter().filter(|e| !e.is_dir).count();
                let enc = l.entries.iter().filter(|e| e.encrypted).count();
                let first = l.entries.iter().find(|e| !e.is_dir).map(|e| e.path.clone()).unwrap_or_default();
                let bd = l.bd.as_ref().and_then(|d| d.main().map(|m| format!(" BD main={} {:.1}s {} streams {} ch", m.name, m.seconds, m.streams.len(), m.chapters))).unwrap_or_default();
                let pkg = l.package.iter().take(2).map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(",");
                println!("{ms:7.1} ms  {name:38} {:28} {:5} files {}{} first={first} {pkg}{}", l.chain.join(">"), files, if enc > 0 { format!("({enc} enc) ") } else { String::new() }, l.facts.iter().find(|(k, _)| k == "Image").map(|(_, v)| format!("[{v}] ")).unwrap_or_default(), bd);
                // Extract the first file to check data access.
                if let (Some(a), Some(e)) = (l.access.as_ref(), l.entries.iter().find(|e| !e.is_dir && !e.encrypted)) {
                    let mut v = Vec::new();
                    match gt_fmt_archive::listing::extract(a, e.id, &mut v, Arc::new(Default::default()), &c) {
                        Ok(()) if v.len() as u64 == e.size => {}
                        Ok(()) => println!("           extract size mismatch {} vs {}", v.len(), e.size),
                        Err(err) => println!("           extract FAILED: {err}"),
                    }
                }
            }
            Err(e) => println!("{ms:7.1} ms  {name:38} ERROR {e}"),
        }
    }
}

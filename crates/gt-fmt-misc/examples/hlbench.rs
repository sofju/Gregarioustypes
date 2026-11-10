//! Time syntax colouring of files: hlbench files...
fn main() {
    for a in std::env::args().skip(1) {
        let p = std::path::PathBuf::from(&a);
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let t0 = std::time::Instant::now();
        let Some(syn) = gt_fmt_misc::highlight::syntax_for(&p, &ext, text.lines().next().unwrap_or("")) else {
            println!("{a}: no syntax");
            continue;
        };
        let load = t0.elapsed();
        let h = gt_fmt_misc::highlight::start(syn, &text);
        let mut first = None;
        while !h.is_done() {
            if first.is_none() && h.ready() >= 200 {
                first = Some(t0.elapsed());
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let n = text.lines().count();
        println!("{a}: {} · {n} lines, {:.1} MB · syntax set {:.0} ms · first 200 lines {:?} · all {:.0} ms", h.language, text.len() as f64 / 1e6, load.as_secs_f64() * 1000.0, first.map(|d| d.as_millis()), t0.elapsed().as_secs_f64() * 1000.0);
    }
}

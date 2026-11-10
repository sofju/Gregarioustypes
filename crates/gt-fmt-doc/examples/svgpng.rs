use gt_fmt_doc::pages::PageSource;
fn main() {
    for a in std::env::args().skip(1) {
        let p = std::path::Path::new(&a);
        let (src, _) = gt_fmt_doc::svg::open(p).unwrap();
        let (w, h) = src.size(0);
        let r = src.render(0, 1.0, 0, 0, w as u32, h as u32).unwrap();
        // BGRA → PPM for a quick look.
        let mut out = format!("P6 {} {} 255\n", r.w, r.h).into_bytes();
        for px in r.data.chunks_exact(4) { out.extend([px[2], px[1], px[0]]); }
        std::fs::write(p.with_extension("ppm"), out).unwrap();
    }
}

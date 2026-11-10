//! SVG / SVGZ with resvg: parsed once, re-rasterized at the on-screen scale so
//! zooming stays sharp. Text uses the system fonts (loaded once, shared).

use crate::pages::{PageSource, Raster};
use resvg::{tiny_skia, usvg};
use std::path::Path;
use std::sync::{Arc, OnceLock};

fn fonts() -> Arc<usvg::fontdb::Database> {
    static DB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let t = std::time::Instant::now();
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        db.set_serif_family("Times New Roman");
        db.set_sans_serif_family("Arial");
        db.set_monospace_family("Consolas");
        db.set_cursive_family("Comic Sans MS");
        db.set_fantasy_family("Impact");
        tracing::info!("svg fonts: {} faces in {:.0} ms", db.len(), t.elapsed().as_secs_f64() * 1000.0);
        Arc::new(db)
    })
    .clone()
}

/// Index the system fonts in the background before the first SVG with text.
pub fn prewarm() {
    let _ = std::thread::Builder::new().name("gt-svg-fonts".into()).spawn(|| {
        fonts();
    });
}

/// Fallback for characters the requested font lacks. resvg re-shapes the whole
/// run with the fallback font when that font covers every character, so a
/// fallback with Latin letters would restyle the entire line (a serif heading
/// turning monospace for one "✓"). Symbol-only fonts come first, then the
/// Windows UI and script fonts, then any proportional font.
fn has_char(db: &usvg::fontdb::Database, id: usvg::fontdb::ID, c: char) -> bool {
    db.with_face_data(id, |data, index| {
        let font = skrifa::FontRef::from_index(data, index).ok()?;
        skrifa::MetadataProvider::charmap(&font).map(c).map(|_| ())
    })
    .flatten()
    .is_some()
}

fn fallback() -> usvg::FallbackSelectionFn<'static> {
    const PREFERRED: &[&str] = &[
        "Segoe UI Symbol",
        "Segoe UI",
        "Segoe UI Historic",
        "Microsoft YaHei UI",
        "Yu Gothic UI",
        "Malgun Gothic",
        "Microsoft JhengHei UI",
        "Nirmala UI",
        "Leelawadee UI",
        "Ebrima",
        "Gadugi",
        "Javanese Text",
        "Myanmar Text",
        "Mongolian Baiti",
        "Microsoft Yi Baiti",
        "Segoe UI Emoji",
    ];
    Box::new(|c, exclude, db| {
        let mut best: Option<(u8, usize, usvg::fontdb::ID)> = None;
        for face in db.faces() {
            if exclude.contains(&face.id) || face.style != usvg::fontdb::Style::Normal || !has_char(db, face.id, c) {
                continue;
            }
            let family = face.families.first().map(|f| f.0.as_str()).unwrap_or("");
            let pref = PREFERRED.iter().position(|p| *p == family);
            let rank = if !has_char(db, face.id, 'a') {
                0
            } else if pref.is_some() {
                1
            } else if !face.monospaced {
                2
            } else {
                3
            };
            let key = (rank, pref.unwrap_or(usize::MAX));
            if best.is_none_or(|(r, p, _)| key < (r, p)) {
                best = Some((key.0, key.1, face.id));
            }
        }
        best.map(|b| b.2)
    })
}

pub struct SvgSource {
    tree: usvg::Tree,
}

impl PageSource for SvgSource {
    fn count(&self) -> usize {
        1
    }
    fn size(&self, _page: usize) -> (f32, f32) {
        let s = self.tree.size();
        (s.width(), s.height())
    }
    fn dip_per_unit(&self) -> f32 {
        1.0
    }
    fn render(&self, _page: usize, scale: f32, x: i32, y: i32, w: u32, h: u32) -> Option<Raster> {
        let mut pm = tiny_skia::Pixmap::new(w, h)?;
        let t = tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, -x as f32, -y as f32);
        resvg::render(&self.tree, t, &mut pm.as_mut());
        let mut data = pm.take();
        for px in data.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        Some(Raster { w, h, data })
    }
    fn paper(&self) -> bool {
        false
    }
}

pub fn open(path: &Path) -> Result<(Arc<SvgSource>, Vec<(String, String)>), String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut opt = usvg::Options { resources_dir: path.parent().map(|p| p.to_path_buf()), ..Default::default() };
    opt.font_resolver = usvg::FontResolver { select_font: usvg::FontResolver::default_font_selector(), select_fallback: fallback() };
    opt.fontdb = fonts();
    let tree = usvg::Tree::from_data(&data, &opt).map_err(|e| format!("Not a valid SVG file: {e}"))?;
    let s = tree.size();
    let mut facts = vec![("Dimensions".to_string(), format!("{} × {} px", fmt_num(s.width()), fmt_num(s.height())))];
    // <title> of the root, for the Info panel.
    let text = String::from_utf8_lossy(&data[..data.len().min(64 * 1024)]);
    if let Some(i) = text.find("<title") {
        if let Some(j) = text[i..].find('>') {
            if let Some(k) = text[i + j..].find("</title>") {
                let t = text[i + j + 1..i + j + k].trim();
                if !t.is_empty() && t.len() < 200 {
                    facts.insert(0, ("Title".into(), t.to_string()));
                }
            }
        }
    }
    if data.starts_with(&[0x1F, 0x8B]) {
        facts.push(("Compression".into(), "gzip (SVGZ)".into()));
    }
    Ok((Arc::new(SvgSource { tree }), facts))
}

fn fmt_num(v: f32) -> String {
    if (v - v.round()).abs() < 0.01 {
        format!("{:.0}", v)
    } else {
        format!("{:.1}", v)
    }
}

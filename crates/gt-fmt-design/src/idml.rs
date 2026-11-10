//! IDML (InDesign Markup Language) "lite" layout: spreads, master pages, layers,
//! frames with Bézier paths, solid and gradient swatches, opacity, placed
//! images (with their crop), and story text flowed through threaded frames and
//! columns, laid out with DirectWrite. Not handled: tables, anchored objects,
//! text wrap, effects, and InDesign's own line breaking (DirectWrite's is used).

use crate::dom::{self, El, Node};
use gt_core::CancelToken;
use gt_fmt_doc::pages::{PageSource, Raster};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 2×3 affine matrix "a b c d tx ty": x' = a·x + c·y + tx, y' = b·x + d·y + ty.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat(pub [f32; 6]);

impl Mat {
    pub const ID: Mat = Mat([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub fn parse(s: Option<&str>) -> Mat {
        let v: Vec<f32> = s.unwrap_or("").split_whitespace().filter_map(|x| x.parse().ok()).collect();
        if v.len() == 6 {
            Mat([v[0], v[1], v[2], v[3], v[4], v[5]])
        } else {
            Mat::ID
        }
    }

    /// Apply `self` first, then `p`.
    pub fn then(&self, p: &Mat) -> Mat {
        let [a, b, c, d, e, f] = self.0;
        let [a2, b2, c2, d2, e2, f2] = p.0;
        Mat([a * a2 + b * c2, a * b2 + b * d2, c * a2 + d * c2, c * b2 + d * d2, e * a2 + f * c2 + e2, e * b2 + f * d2 + f2])
    }

    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }

    pub fn translate(x: f32, y: f32) -> Mat {
        Mat([1.0, 0.0, 0.0, 1.0, x, y])
    }

    pub fn scale(s: f32) -> Mat {
        Mat([s, 0.0, 0.0, s, 0.0, 0.0])
    }
}

/// Premultiplied-free RGBA, 0..1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub [f32; 4]);

#[derive(Clone, Debug)]
pub enum Paint {
    Solid(Rgba),
    /// Gradient stops (position 0..1, colour), linear or radial.
    Gradient { stops: Vec<(f32, Rgba)>, radial: bool },
}

#[derive(Clone, Debug, Default)]
pub struct SubPath {
    /// (anchor, left direction, right direction)
    pub pts: Vec<[(f32, f32); 3]>,
    pub open: bool,
}

#[derive(Clone, Debug)]
pub enum Source {
    File(PathBuf),
    Embedded(Arc<Vec<u8>>),
    Missing(String),
}

#[derive(Clone, Debug)]
pub struct Placed {
    /// Graphic → frame coordinates.
    pub t: Mat,
    /// GraphicBounds (left, top, right, bottom) in graphic coordinates.
    pub bounds: (f32, f32, f32, f32),
    pub source: Source,
}

#[derive(Clone, Debug)]
pub struct TextFrameInfo {
    pub story: String,
    pub prev: Option<String>,
    pub next: Option<String>,
    pub inset: [f32; 4],
    pub columns: usize,
    pub gutter: f32,
    pub vjust: VJust,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VJust {
    Top,
    Center,
    Bottom,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub kind: String,
    /// Item → parent coordinates.
    pub t: Mat,
    pub paths: Vec<SubPath>,
    pub fill: Option<Paint>,
    pub stroke: Option<Paint>,
    pub stroke_w: f32,
    pub opacity: f32,
    pub corner: f32,
    pub layer: String,
    pub visible: bool,
    pub placed: Vec<Placed>,
    pub children: Vec<Item>,
    pub text: Option<TextFrameInfo>,
}

impl Item {
    /// Bounding box of the paths, item coordinates.
    pub fn bbox(&self) -> Option<(f32, f32, f32, f32)> {
        let mut b: Option<(f32, f32, f32, f32)> = None;
        for p in &self.paths {
            for pt in &p.pts {
                for &(x, y) in pt {
                    b = Some(match b {
                        None => (x, y, x, y),
                        Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                    });
                }
            }
        }
        b
    }
}

#[derive(Clone, Debug)]
pub struct Page {
    pub id: String,
    pub name: String,
    /// Page rectangle in spread coordinates (x0, y0, x1, y1).
    pub rect: (f32, f32, f32, f32),
    pub master: Option<String>,
    pub overrides: HashSet<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Spread {
    pub pages: Vec<Page>,
    pub items: Vec<Item>,
}

// ---------------------------------------------------------------- text

#[derive(Clone, Debug, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Clone, Debug)]
pub struct Props {
    pub font: String,
    pub style: String,
    pub size: f32,
    pub color: Option<Paint>,
    pub tint: f32,
    /// None = auto (120 % of the size).
    pub leading: Option<f32>,
    pub caps: bool,
    pub underline: bool,
    pub strike: bool,
    pub align: Align,
    pub space_before: f32,
    pub space_after: f32,
    pub left_indent: f32,
    pub right_indent: f32,
    pub first_indent: f32,
    pub bullet: Option<char>,
    /// Letter spacing in thousandths of an em.
    pub tracking: f32,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            font: "Minion Pro".into(),
            style: "Regular".into(),
            size: 12.0,
            color: Some(Paint::Solid(Rgba([0.0, 0.0, 0.0, 1.0]))),
            tint: 1.0,
            leading: None,
            caps: false,
            underline: false,
            strike: false,
            align: Align::Left,
            space_before: 0.0,
            space_after: 0.0,
            left_indent: 0.0,
            right_indent: 0.0,
            first_indent: 0.0,
            bullet: None,
            tracking: 0.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Run {
    pub text: String,
    pub props: Props,
}

#[derive(Clone, Debug, Default)]
pub struct Para {
    pub runs: Vec<Run>,
}

impl Para {
    pub fn props(&self) -> Props {
        self.runs.first().map(|r| r.props.clone()).unwrap_or_default()
    }
}

/// A style element: attributes, `<Properties>` children, BasedOn.
#[derive(Clone, Debug, Default)]
struct StyleDef {
    attrs: Vec<(String, String)>,
    props: Vec<(String, String)>,
    based_on: Option<String>,
}

fn style_def(e: &El) -> StyleDef {
    let mut d = StyleDef { attrs: e.attrs.clone(), ..Default::default() };
    if let Some(p) = e.child("Properties") {
        for c in p.els() {
            if c.name == "BasedOn" {
                d.based_on = Some(c.text());
            } else if c.name == "BulletChar" {
                if let Some(v) = c.attr("BulletCharacterValue") {
                    d.props.push(("BulletChar".into(), v.to_string()));
                }
            } else {
                d.props.push((c.name.clone(), c.text()));
            }
        }
    }
    d
}

pub struct Idml {
    pub spreads: Vec<Spread>,
    pub masters: HashMap<String, Spread>,
    /// Bottom-to-top layer order and visibility.
    pub layers: Vec<(String, bool)>,
    pub stories: HashMap<String, Vec<Para>>,
    /// (spread index, page index) per document page.
    pub pages: Vec<(usize, usize)>,
    pub dir: PathBuf,
    pub images: Mutex<HashMap<String, Option<Arc<Raster>>>>,
    /// Paragraph ranges per text frame (column-expanded), computed once.
    pub flow: Mutex<Option<Arc<HashMap<String, Vec<(usize, usize)>>>>>,
    pub frames: HashMap<String, Item>,
}

struct Ctx<'a> {
    colors: &'a HashMap<String, Paint>,
    /// Object styles: frame defaults (stroke weight, colours) items inherit.
    objects: &'a HashMap<String, StyleDef>,
}

fn cmyk(c: f32, m: f32, y: f32, k: f32) -> Rgba {
    let (c, m, y, k) = (c / 100.0, m / 100.0, y / 100.0, k / 100.0);
    Rgba([(1.0 - c) * (1.0 - k), (1.0 - m) * (1.0 - k), (1.0 - y) * (1.0 - k), 1.0])
}

fn lab(l: f32, a: f32, b: f32) -> Rgba {
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let f = |t: f32| if t > 6.0 / 29.0 { t * t * t } else { 3.0 * (6.0f32 / 29.0).powi(2) * (t - 4.0 / 29.0) };
    let (x, y, z) = (0.9642 * f(fx), f(fy), 0.8249 * f(fz));
    let r = 3.1338561 * x - 1.6168667 * y - 0.4906146 * z;
    let g = -0.9787684 * x + 1.9161415 * y + 0.0334540 * z;
    let bl = 0.0719453 * x - 0.2289914 * y + 1.4052427 * z;
    let enc = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.0031308 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    };
    Rgba([enc(r), enc(g), enc(bl), 1.0])
}

fn colors(graphic: &El) -> HashMap<String, Paint> {
    let mut map = HashMap::new();
    let mut list = Vec::new();
    graphic.find_all("Color", &mut list);
    for c in list {
        let Some(id) = c.attr("Self") else { continue };
        let v: Vec<f32> = c.attr("ColorValue").unwrap_or("").split_whitespace().filter_map(|x| x.parse().ok()).collect();
        let col = match (c.attr("Space").unwrap_or("CMYK"), v.as_slice()) {
            ("CMYK", [c, m, y, k]) => cmyk(*c, *m, *y, *k),
            ("RGB", [r, g, b]) => Rgba([r / 255.0, g / 255.0, b / 255.0, 1.0]),
            ("LAB", [l, a, b]) => lab(*l, *a, *b),
            _ => Rgba([0.0, 0.0, 0.0, 1.0]),
        };
        map.insert(id.to_string(), Paint::Solid(col));
    }
    map.insert("Color/Paper".into(), Paint::Solid(Rgba([1.0, 1.0, 1.0, 1.0])));
    map.entry("Color/Black".into()).or_insert(Paint::Solid(Rgba([0.0, 0.0, 0.0, 1.0])));
    map.insert("Color/Registration".into(), Paint::Solid(Rgba([0.0, 0.0, 0.0, 1.0])));
    // Tints of a base colour.
    let mut tints = Vec::new();
    graphic.find_all("Tint", &mut tints);
    for t in tints {
        let (Some(id), Some(base)) = (t.attr("Self"), t.attr("BaseColor")) else { continue };
        let pct = t.f("TintValue").unwrap_or(100.0) / 100.0;
        if let Some(Paint::Solid(Rgba(c))) = map.get(base).cloned() {
            map.insert(id.to_string(), Paint::Solid(Rgba([1.0 - (1.0 - c[0]) * pct, 1.0 - (1.0 - c[1]) * pct, 1.0 - (1.0 - c[2]) * pct, 1.0])));
        }
    }
    let mut grads = Vec::new();
    graphic.find_all("Gradient", &mut grads);
    for g in grads {
        let Some(id) = g.attr("Self") else { continue };
        let mut stops = Vec::new();
        for s in g.els().filter(|e| e.name == "GradientStop") {
            let loc = s.f("Location").unwrap_or(0.0) / 100.0;
            if let Some(Paint::Solid(c)) = s.attr("StopColor").and_then(|k| map.get(k)) {
                stops.push((loc, *c));
            }
        }
        if !stops.is_empty() {
            map.insert(id.to_string(), Paint::Gradient { stops, radial: g.attr("Type") == Some("Radial") });
        }
    }
    map
}

fn paint(ctx: &Ctx, key: Option<&str>, tint: Option<f32>) -> Option<Paint> {
    let key = key?;
    if key == "Swatch/None" || key == "n" {
        return None;
    }
    let p = ctx.colors.get(key).cloned()?;
    let t = tint.filter(|t| *t >= 0.0 && *t < 100.0).map(|t| t / 100.0);
    Some(match (p, t) {
        (Paint::Solid(Rgba(c)), Some(t)) => Paint::Solid(Rgba([1.0 - (1.0 - c[0]) * t, 1.0 - (1.0 - c[1]) * t, 1.0 - (1.0 - c[2]) * t, 1.0])),
        (p, _) => p,
    })
}

fn paths(e: &El) -> Vec<SubPath> {
    let mut out = Vec::new();
    let Some(geo) = e.child("Properties").and_then(|p| p.child("PathGeometry")) else { return out };
    for g in geo.els().filter(|x| x.name == "GeometryPathType") {
        let mut sp = SubPath { open: g.attr("PathOpen") == Some("true"), pts: Vec::new() };
        if let Some(arr) = g.child("PathPointArray") {
            for p in arr.els() {
                let pt = |k: &str| {
                    let v: Vec<f32> = p.attr(k).unwrap_or("").split_whitespace().filter_map(|x| x.parse().ok()).collect();
                    if v.len() == 2 {
                        (v[0], v[1])
                    } else {
                        (0.0, 0.0)
                    }
                };
                let a = pt("Anchor");
                let l = if p.attr("LeftDirection").is_some() { pt("LeftDirection") } else { a };
                let r = if p.attr("RightDirection").is_some() { pt("RightDirection") } else { a };
                sp.pts.push([a, l, r]);
            }
        }
        if !sp.pts.is_empty() {
            out.push(sp);
        }
    }
    out
}

/// URI of a link → a path on this machine, or where the file would be next to the IDML.
fn resolve_link(uri: &str, dir: &Path) -> Source {
    let mut s = uri.trim().to_string();
    for p in ["file:///", "file://", "file:"] {
        if let Some(r) = s.strip_prefix(p) {
            s = r.to_string();
            break;
        }
    }
    // Percent-decoding.
    let bytes = s.as_bytes();
    let mut dec = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                dec.push(v);
                i += 3;
                continue;
            }
        }
        dec.push(bytes[i]);
        i += 1;
    }
    let s = String::from_utf8_lossy(&dec).to_string();
    let candidate = PathBuf::from(s.trim_start_matches('/').replace('/', "\\"));
    if candidate.is_file() {
        return Source::File(candidate);
    }
    // Mac ("Volume:Users:x:file.jpg") or moved documents: look next to the IDML.
    let name = s.rsplit(['/', '\\', ':']).next().unwrap_or(&s).to_string();
    for d in [dir.join("Links"), dir.to_path_buf(), dir.join("..").join("Links")] {
        let p = d.join(&name);
        if p.is_file() {
            return Source::File(p);
        }
    }
    Source::Missing(name)
}

fn item(ctx: &Ctx, e: &El, dir: &Path) -> Option<Item> {
    let kind = e.name.as_str();
    if !matches!(kind, "Rectangle" | "Oval" | "Polygon" | "GraphicLine" | "TextFrame" | "Group" | "Button" | "MultiStateObject") {
        return None;
    }
    // Attributes the item doesn't set come from its object style chain.
    let mut inherited: HashMap<&str, String> = HashMap::new();
    if let Some(os) = e.attr("AppliedObjectStyle") {
        for d in chain(ctx.objects, os) {
            for (k, v) in &d.attrs {
                if matches!(k.as_str(), "FillColor" | "StrokeColor" | "StrokeWeight" | "FillTint" | "StrokeTint") {
                    inherited.insert(match k.as_str() {
                        "FillColor" => "FillColor",
                        "StrokeColor" => "StrokeColor",
                        "StrokeWeight" => "StrokeWeight",
                        "FillTint" => "FillTint",
                        _ => "StrokeTint",
                    }, v.clone());
                }
            }
        }
    }
    let get = |k: &str| e.attr(k).map(|v| v.to_string()).or_else(|| inherited.get(k).cloned());
    let getf = |k: &str| get(k).and_then(|v| v.trim().parse::<f32>().ok());
    let mut opacity = 1.0;
    if let Some(ts) = e.child("TransparencySetting").and_then(|t| t.child("BlendingSetting")) {
        opacity = ts.f("Opacity").unwrap_or(100.0) / 100.0;
    }
    let mut it = Item {
        id: e.attr("Self").unwrap_or("").to_string(),
        kind: kind.to_string(),
        t: Mat::parse(e.attr("ItemTransform")),
        paths: paths(e),
        fill: paint(ctx, get("FillColor").as_deref(), getf("FillTint")),
        stroke: paint(ctx, get("StrokeColor").as_deref(), getf("StrokeTint")),
        // InDesign's default stroke is 1 pt.
        stroke_w: getf("StrokeWeight").unwrap_or(1.0),
        opacity,
        corner: if e.attr("CornerOption").is_some_and(|c| c.contains("Rounded")) || e.attr("TopLeftCornerOption").is_some_and(|c| c.contains("Rounded")) {
            e.f("CornerRadius").or_else(|| e.f("TopLeftCornerRadius")).unwrap_or(0.0)
        } else {
            0.0
        },
        layer: e.attr("ItemLayer").unwrap_or("").to_string(),
        visible: e.attr("Visible") != Some("false"),
        placed: Vec::new(),
        children: Vec::new(),
        text: None,
    };
    if kind == "TextFrame" {
        let pref = e.child("TextFramePreference");
        let mut inset = [0.0f32; 4];
        if let Some(p) = pref {
            if let Some(v) = p.attr("InsetSpacing") {
                let v: Vec<f32> = v.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                if v.len() == 4 {
                    inset = [v[0], v[1], v[2], v[3]];
                } else if v.len() == 1 {
                    inset = [v[0]; 4];
                }
            } else if let Some(list) = p.child("Properties").and_then(|q| q.child("InsetSpacing")) {
                let v: Vec<f32> = list.els().filter_map(|x| x.text().trim().parse().ok()).collect();
                if v.len() == 4 {
                    inset = [v[0], v[1], v[2], v[3]];
                }
            }
        }
        let opt = |k: &str| e.attr(k).filter(|v| *v != "n").map(|v| v.to_string());
        it.text = Some(TextFrameInfo {
            story: e.attr("ParentStory").unwrap_or("").to_string(),
            prev: opt("PreviousTextFrame"),
            next: opt("NextTextFrame"),
            inset,
            columns: pref.and_then(|p| p.f("TextColumnCount")).unwrap_or(1.0).max(1.0) as usize,
            gutter: pref.and_then(|p| p.f("TextColumnGutter")).unwrap_or(12.0),
            vjust: match pref.and_then(|p| p.attr("VerticalJustification")) {
                Some("CenterAlign") => VJust::Center,
                Some("BottomAlign") => VJust::Bottom,
                _ => VJust::Top,
            },
        });
    }
    for c in e.els() {
        match c.name.as_str() {
            "Image" | "PDF" | "EPS" | "SVG" | "ImportedPage" | "WMF" | "PICT" => {
                let gb = c.child("Properties").and_then(|p| p.child("GraphicBounds"));
                let bounds = gb.map(|g| (g.f("Left").unwrap_or(0.0), g.f("Top").unwrap_or(0.0), g.f("Right").unwrap_or(0.0), g.f("Bottom").unwrap_or(0.0))).unwrap_or((0.0, 0.0, 0.0, 0.0));
                let source = if let Some(data) = c.prop("Contents").filter(|s| s.len() > 16) {
                    let clean: String = data.chars().filter(|ch| !ch.is_whitespace()).collect();
                    match gt_fmt_media::probe::base64_decode(&clean) {
                        Some(b) => Source::Embedded(Arc::new(b)),
                        None => Source::Missing("embedded image".into()),
                    }
                } else if let Some(uri) = c.child("Link").and_then(|l| l.attr("LinkResourceURI")) {
                    resolve_link(uri, dir)
                } else {
                    Source::Missing(String::new())
                };
                if c.attr("Visible") != Some("false") {
                    it.placed.push(Placed { t: Mat::parse(c.attr("ItemTransform")), bounds, source });
                }
            }
            _ => {
                if let Some(ch) = item(ctx, c, dir) {
                    it.children.push(ch);
                }
            }
        }
    }
    Some(it)
}

fn spread(ctx: &Ctx, root: &El, dir: &Path) -> Spread {
    // The file's root is the <idPkg:Spread> wrapper; the spread itself is inside.
    let s = root.els().find(|e| e.name == "Spread" || e.name == "MasterSpread").unwrap_or(root);
    let st = Mat::parse(s.attr("ItemTransform"));
    let _ = st;
    let mut sp = Spread::default();
    for e in s.els() {
        if e.name == "Page" {
            let t = Mat::parse(e.attr("ItemTransform"));
            let b: Vec<f32> = e.attr("GeometricBounds").unwrap_or("").split_whitespace().filter_map(|x| x.parse().ok()).collect();
            let (top, left, bottom, right) = if b.len() == 4 { (b[0], b[1], b[2], b[3]) } else { (0.0, 0.0, 792.0, 612.0) };
            let (x0, y0) = t.apply(left, top);
            let (x1, y1) = t.apply(right, bottom);
            let overrides = e.attr("OverrideList").unwrap_or("").split_whitespace().filter(|v| *v != "n").map(|v| v.to_string()).collect();
            sp.pages.push(Page {
                id: e.attr("Self").unwrap_or("").into(),
                name: e.attr("Name").unwrap_or("").into(),
                rect: (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)),
                master: e.attr("AppliedMaster").filter(|m| *m != "n").map(|m| m.to_string()),
                overrides,
            });
        } else if let Some(it) = item(ctx, e, dir) {
            sp.items.push(it);
        }
    }
    sp
}

// ---------------------------------------------------------------- styles and stories

struct Styles {
    para: HashMap<String, StyleDef>,
    chr: HashMap<String, StyleDef>,
    obj: HashMap<String, StyleDef>,
}

fn chain<'a>(map: &'a HashMap<String, StyleDef>, id: &str) -> Vec<&'a StyleDef> {
    let mut v = Vec::new();
    let mut cur = Some(id.to_string());
    let mut seen = HashSet::new();
    while let Some(c) = cur {
        if !seen.insert(c.clone()) {
            break;
        }
        let Some(d) = map.get(&c) else { break };
        v.push(d);
        cur = d.based_on.clone();
    }
    v.reverse();
    v
}

fn apply(p: &mut Props, ctx: &Ctx, attrs: &[(String, String)], props: &[(String, String)]) {
    let num = |v: &str| v.trim().parse::<f32>().ok();
    for (k, v) in attrs {
        match k.as_str() {
            "PointSize" => p.size = num(v).unwrap_or(p.size),
            "FontStyle" => p.style = v.clone(),
            "FillColor" => p.color = paint(ctx, Some(v), None),
            "FillTint" => p.tint = num(v).filter(|t| *t >= 0.0).map(|t| t / 100.0).unwrap_or(1.0),
            "Capitalization" => p.caps = v == "AllCaps" || v == "SmallCaps",
            "Underline" => p.underline = v == "true",
            "StrikeThru" => p.strike = v == "true",
            "Justification" => {
                p.align = match v.as_str() {
                    "CenterAlign" | "CenterJustified" => Align::Center,
                    "RightAlign" | "RightJustified" | "AwayFromBindingSide" => Align::Right,
                    "LeftJustified" | "FullyJustified" => Align::Justify,
                    _ => Align::Left,
                }
            }
            "Tracking" => p.tracking = num(v).unwrap_or(0.0),
            "SpaceBefore" => p.space_before = num(v).unwrap_or(0.0),
            "SpaceAfter" => p.space_after = num(v).unwrap_or(0.0),
            "LeftIndent" => p.left_indent = num(v).unwrap_or(0.0),
            "RightIndent" => p.right_indent = num(v).unwrap_or(0.0),
            "FirstLineIndent" => p.first_indent = num(v).unwrap_or(0.0),
            "BulletsAndNumberingListType" => {
                if v == "BulletList" {
                    p.bullet.get_or_insert('\u{2022}');
                } else {
                    p.bullet = None;
                }
            }
            _ => {}
        }
    }
    for (k, v) in props {
        match k.as_str() {
            "AppliedFont" if !v.trim().is_empty() => p.font = v.trim().to_string(),
            "Leading" => p.leading = num(v),
            "BulletChar" => {
                if p.bullet.is_some() {
                    p.bullet = v.parse::<u32>().ok().and_then(char::from_u32);
                }
            }
            _ => {}
        }
    }
}

fn styles(root: &El) -> Styles {
    let mut para = HashMap::new();
    let mut chr = HashMap::new();
    let mut list = Vec::new();
    root.find_all("ParagraphStyle", &mut list);
    for e in list {
        if let Some(id) = e.attr("Self") {
            para.insert(id.to_string(), style_def(e));
        }
    }
    let mut list = Vec::new();
    root.find_all("CharacterStyle", &mut list);
    for e in list {
        if let Some(id) = e.attr("Self") {
            chr.insert(id.to_string(), style_def(e));
        }
    }
    let mut obj = HashMap::new();
    let mut list = Vec::new();
    root.find_all("ObjectStyle", &mut list);
    for e in list {
        if let Some(id) = e.attr("Self") {
            obj.insert(id.to_string(), style_def(e));
        }
    }
    Styles { para, chr, obj }
}

fn story(ctx: &Ctx, st: &Styles, root: &El) -> Vec<Para> {
    let s = root.find("Story").unwrap_or(root);
    let mut paras = Vec::new();
    let mut cur = Para::default();
    fn walk(ctx: &Ctx, st: &Styles, e: &El, para_base: &Props, paras: &mut Vec<Para>, cur: &mut Para) {
        for c in e.els() {
            match c.name.as_str() {
                "ParagraphStyleRange" => {
                    let mut p = Props::default();
                    if let Some(id) = c.attr("AppliedParagraphStyle") {
                        for d in chain(&st.para, id) {
                            apply(&mut p, ctx, &d.attrs, &d.props);
                        }
                    }
                    let local = style_def(c);
                    apply(&mut p, ctx, &local.attrs, &local.props);
                    walk(ctx, st, c, &p, paras, cur);
                }
                "CharacterStyleRange" => {
                    let mut p = para_base.clone();
                    if let Some(id) = c.attr("AppliedCharacterStyle") {
                        for d in chain(&st.chr, id) {
                            // Character styles carry no paragraph attributes.
                            apply(&mut p, ctx, &d.attrs, &d.props);
                        }
                    }
                    let local = style_def(c);
                    apply(&mut p, ctx, &local.attrs, &local.props);
                    for k in &c.kids {
                        match k {
                            Node::El(x) if x.name == "Content" => {
                                let mut t = String::new();
                                for n in &x.kids {
                                    match n {
                                        Node::Text(s) => t.push_str(s),
                                        Node::Pi(pi) if pi.starts_with("ACE 18") => t.push('\u{E018}'),
                                        _ => {}
                                    }
                                }
                                if !t.is_empty() {
                                    cur.runs.push(Run { text: t, props: p.clone() });
                                }
                            }
                            Node::El(x) if x.name == "Br" => {
                                if cur.runs.is_empty() {
                                    cur.runs.push(Run { text: String::new(), props: p.clone() });
                                }
                                paras.push(std::mem::take(cur));
                            }
                            Node::El(x) if x.name == "XMLElement" || x.name == "Change" || x.name == "HyperlinkTextSource" => {
                                walk(ctx, st, x, &p, paras, cur);
                            }
                            Node::Pi(pi) if pi.starts_with("ACE 18") => cur.runs.push(Run { text: "\u{E018}".into(), props: p.clone() }),
                            _ => {}
                        }
                    }
                }
                "XMLElement" | "Change" | "HyperlinkTextSource" => walk(ctx, st, c, para_base, paras, cur),
                _ => {}
            }
        }
    }
    walk(ctx, st, s, &Props::default(), &mut paras, &mut cur);
    if !cur.runs.is_empty() {
        paras.push(cur);
    }
    paras
}

fn read_zip(z: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Option<String> {
    let mut f = z.by_name(name).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

fn collect_frames(items: &[Item], out: &mut HashMap<String, Item>) {
    for it in items {
        if it.text.is_some() {
            out.insert(it.id.clone(), it.clone());
        }
        collect_frames(&it.children, out);
    }
}

pub fn parse(path: &Path, cancel: &CancelToken) -> Result<Idml, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut z = zip::ZipArchive::new(file).map_err(|e| format!("Not an IDML package: {e}"))?;
    let design = dom::parse(&read_zip(&mut z, "designmap.xml").ok_or("designmap.xml missing")?)?;
    let graphic = read_zip(&mut z, "Resources/Graphic.xml").map(|s| dom::parse(&s)).transpose()?.unwrap_or_default();
    let colors = colors(&graphic);
    let st = read_zip(&mut z, "Resources/Styles.xml").map(|s| dom::parse(&s)).transpose()?.map(|e| styles(&e)).unwrap_or(Styles { para: HashMap::new(), chr: HashMap::new(), obj: HashMap::new() });
    let ctx = Ctx { colors: &colors, objects: &st.obj };
    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let mut layers = Vec::new();
    for l in design.els().filter(|e| e.name == "Layer") {
        layers.push((l.attr("Self").unwrap_or("").to_string(), l.attr("Visible") != Some("false")));
    }
    let mut spreads = Vec::new();
    let mut masters = HashMap::new();
    let mut stories = HashMap::new();
    for e in design.els() {
        if cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        let Some(src) = e.attr("src") else { continue };
        match e.name.as_str() {
            "Spread" | "MasterSpread" => {
                let Some(x) = read_zip(&mut z, src) else { continue };
                let root = dom::parse(&x)?;
                let sp = spread(&ctx, &root, &dir);
                if e.name == "Spread" {
                    spreads.push(sp);
                } else {
                    let id = root.find("MasterSpread").and_then(|m| m.attr("Self")).unwrap_or("").to_string();
                    masters.insert(id, sp);
                }
            }
            "Story" => {
                let Some(x) = read_zip(&mut z, src) else { continue };
                let root = dom::parse(&x)?;
                let id = root.find("Story").and_then(|s| s.attr("Self")).unwrap_or("").to_string();
                stories.insert(id, story(&ctx, &st, &root));
            }
            _ => {}
        }
    }
    let mut pages = Vec::new();
    for (i, s) in spreads.iter().enumerate() {
        for j in 0..s.pages.len() {
            pages.push((i, j));
        }
    }
    if pages.is_empty() {
        return Err("This IDML has no pages.".into());
    }
    let mut frames = HashMap::new();
    for s in spreads.iter().chain(masters.values()) {
        collect_frames(&s.items, &mut frames);
    }
    Ok(Idml { spreads, masters, layers, stories, pages, dir, images: Mutex::new(HashMap::new()), flow: Mutex::new(None), frames })
}

pub struct IdmlSource(pub Idml);

impl PageSource for IdmlSource {
    fn count(&self) -> usize {
        self.0.pages.len()
    }
    fn size(&self, page: usize) -> (f32, f32) {
        let (s, p) = self.0.pages[page];
        let r = self.0.spreads[s].pages[p].rect;
        (r.2 - r.0, r.3 - r.1)
    }
    fn render(&self, page: usize, scale: f32, x: i32, y: i32, w: u32, h: u32) -> Option<Raster> {
        crate::idml_render::render(&self.0, page, scale, x, y, w, h)
    }
    fn text(&self, page: usize) -> Option<String> {
        crate::idml_render::page_text(&self.0, page)
    }
    fn label(&self, page: usize) -> Option<String> {
        let (s, p) = self.0.pages[page];
        Some(self.0.spreads[s].pages[p].name.clone())
    }
}

/// Open an IDML for the page viewer; facts for the Info panel.
pub fn load(path: &Path, cancel: &CancelToken) -> Result<(Arc<dyn PageSource>, Vec<(String, String)>), String> {
    let t = std::time::Instant::now();
    let doc = parse(path, cancel)?;
    let mut facts = vec![("Format".to_string(), "InDesign Markup (IDML)".to_string())];
    facts.push(("Pages".into(), doc.pages.len().to_string()));
    let (s, p) = doc.pages[0];
    let r = doc.spreads[s].pages[p].rect;
    facts.push(("Page size".into(), gt_fmt_doc::pdf::paper_size(r.2 - r.0, r.3 - r.1)));
    facts.push(("Stories".into(), doc.stories.len().to_string()));
    let mut links = 0;
    let mut missing = 0;
    fn count(items: &[Item], links: &mut usize, missing: &mut usize) {
        for it in items {
            for pl in &it.placed {
                *links += 1;
                if matches!(pl.source, Source::Missing(_)) {
                    *missing += 1;
                }
            }
            count(&it.children, links, missing);
        }
    }
    for s in &doc.spreads {
        count(&s.items, &mut links, &mut missing);
    }
    if links > 0 {
        facts.push(("Placed graphics".into(), if missing > 0 { format!("{links} ({missing} missing)") } else { links.to_string() }));
    }
    facts.push(("Rendering".into(), "Lite layout: text reflowed with DirectWrite; tables, effects and wrap not applied".into()));
    tracing::info!("idml {} parsed in {:.0} ms: {} pages", path.display(), t.elapsed().as_secs_f64() * 1000.0, doc.pages.len());
    Ok((Arc::new(IdmlSource(doc)), facts))
}

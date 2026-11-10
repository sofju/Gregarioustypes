//! Drawing an IDML page with Direct2D (software render target on a WIC bitmap,
//! so it runs on the page-render worker threads) and DirectWrite.

use crate::idml::{Align, Idml, Item, Mat, Paint, Para, Placed, Props, Rgba, Source, VJust};
use gt_fmt_doc::pages::Raster;
use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::sync::Arc;
use windows::core::{Interface, HSTRING};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows_numerics::{Matrix3x2, Vector2};

struct Factories {
    d2d: ID2D1Factory,
    dw: IDWriteFactory,
    wic: IWICImagingFactory,
}

thread_local! {
    // Leaked on purpose: tearing COM objects down after the thread's COM
    // apartment is gone crashes at thread exit.
    static FACTORIES: RefCell<Option<&'static Factories>> = const { RefCell::new(None) };
}

fn with_factories<R>(f: impl FnOnce(&Factories) -> Option<R>) -> Option<R> {
    FACTORIES.with(|cell| {
        if cell.borrow().is_none() {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
                let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
                let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
                *cell.borrow_mut() = Some(Box::leak(Box::new(Factories { d2d, dw, wic })));
            }
        }
        let b = *cell.borrow();
        f(b?)
    })
}

fn mx(m: &Mat) -> Matrix3x2 {
    let [a, b, c, d, e, f] = m.0;
    Matrix3x2 { M11: a, M12: b, M21: c, M22: d, M31: e, M32: f }
}

fn col(c: Rgba, opacity: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.0[0], g: c.0[1], b: c.0[2], a: c.0[3] * opacity }
}

// ---------------------------------------------------------------- text layout

fn weight_style(style: &str) -> (DWRITE_FONT_WEIGHT, DWRITE_FONT_STYLE) {
    let s = style.to_ascii_lowercase().replace(' ', "");
    let w = if s.contains("black") || s.contains("heavy") {
        900
    } else if s.contains("extrabold") || s.contains("ultrabold") {
        800
    } else if s.contains("semibold") || s.contains("demibold") || s.contains("demi") {
        600
    } else if s.contains("bold") {
        700
    } else if s.contains("medium") {
        500
    } else if s.contains("extralight") || s.contains("ultralight") {
        200
    } else if s.contains("light") {
        300
    } else if s.contains("thin") || s.contains("hairline") {
        100
    } else {
        400
    };
    let italic = s.contains("italic") || s.contains("oblique") || s.contains("oblicua");
    (DWRITE_FONT_WEIGHT(w), if italic { DWRITE_FONT_STYLE_ITALIC } else { DWRITE_FONT_STYLE_NORMAL })
}

/// The paragraph's text, with UTF-16 run ranges.
fn para_text(p: &Para, page_name: &str) -> (Vec<u16>, Vec<(u32, u32, Props)>) {
    let mut text: Vec<u16> = Vec::new();
    let mut ranges = Vec::new();
    let first = p.props();
    if let Some(b) = first.bullet {
        let s: Vec<u16> = format!("{b}\u{2002}").encode_utf16().collect();
        ranges.push((0, s.len() as u32, first.clone()));
        text.extend(s);
    }
    for r in &p.runs {
        let mut s = r.text.replace('\u{E018}', page_name).replace('\u{FEFF}', "");
        if r.props.caps {
            s = s.to_uppercase();
        }
        let u: Vec<u16> = s.encode_utf16().collect();
        if u.is_empty() {
            continue;
        }
        ranges.push((text.len() as u32, u.len() as u32, r.props.clone()));
        text.extend(u);
    }
    (text, ranges)
}

/// InDesign's composer fits a little more on a line than DirectWrite: wrap
/// against a slightly wider measure so headings don't break early.
const WRAP_SLACK: f32 = 1.04;

fn leading(p: &Para) -> f32 {
    let max_size = p.runs.iter().map(|r| r.props.size).fold(0.0f32, f32::max).max(1.0);
    p.runs.iter().filter_map(|r| r.props.leading).fold(None::<f32>, |a, l| Some(a.map_or(l, |a: f32| a.max(l)))).unwrap_or(max_size * 1.2)
}

/// A DirectWrite layout of one paragraph at `width` points; returns it and its height.
fn layout(dw: &IDWriteFactory, p: &Para, width: f32, page_name: &str) -> Option<(IDWriteTextLayout, f32)> {
    let (text, ranges) = para_text(p, page_name);
    let pp = p.props();
    let lead = leading(p);
    unsafe {
        let (w, s) = weight_style(&pp.style);
        let fmt = dw.CreateTextFormat(&HSTRING::from(pp.font.as_str()), None, w, s, DWRITE_FONT_STRETCH_NORMAL, pp.size.max(0.5), &HSTRING::from("en-us")).ok()?;
        let _ = fmt.SetTextAlignment(match pp.align {
            Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
            Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
            Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            Align::Justify => DWRITE_TEXT_ALIGNMENT_JUSTIFIED,
        });
        let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
        let _ = fmt.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, lead, lead * 0.8);
        let body: Vec<u16> = if text.is_empty() { vec![0x200B] } else { text };
        let lay = dw.CreateTextLayout(&body, &fmt, width.max(1.0), 100_000.0).ok()?;
        for (start, len, rp) in &ranges {
            let range = DWRITE_TEXT_RANGE { startPosition: *start, length: *len };
            let (w, s) = weight_style(&rp.style);
            let _ = lay.SetFontFamilyName(&HSTRING::from(rp.font.as_str()), range);
            let _ = lay.SetFontSize(rp.size.max(0.5), range);
            let _ = lay.SetFontWeight(w, range);
            let _ = lay.SetFontStyle(s, range);
            if rp.underline {
                let _ = lay.SetUnderline(true, range);
            }
            if rp.strike {
                let _ = lay.SetStrikethrough(true, range);
            }
            if rp.tracking.abs() > 0.5 {
                if let Ok(l1) = lay.cast::<IDWriteTextLayout1>() {
                    let _ = l1.SetCharacterSpacing(0.0, rp.tracking / 1000.0 * rp.size, 0.0, range);
                }
            }
        }
        let mut m = DWRITE_TEXT_METRICS::default();
        lay.GetMetrics(&mut m).ok()?;
        let h = if p.runs.iter().all(|r| r.text.is_empty()) { lead } else { m.height };
        Some((lay, h + pp.space_before + pp.space_after))
    }
}

/// Text rectangles of a frame (columns), frame coordinates.
fn columns(it: &Item) -> Vec<(f32, f32, f32, f32)> {
    let Some(t) = &it.text else { return Vec::new() };
    let Some((x0, y0, x1, y1)) = it.bbox() else { return Vec::new() };
    let [top, left, bottom, right] = t.inset;
    let (ix0, iy0, ix1, iy1) = (x0 + left, y0 + top, x1 - right, y1 - bottom);
    let n = t.columns.max(1);
    let cw = ((ix1 - ix0) - t.gutter * (n as f32 - 1.0)) / n as f32;
    (0..n).map(|i| {
        let cx = ix0 + i as f32 * (cw + t.gutter);
        (cx, iy0, cx + cw.max(1.0), iy1)
    }).collect()
}

/// Which paragraphs go in which frame column: stories flow through threaded
/// frames, column after column. Computed once per document.
fn flow(doc: &Idml, dw: &IDWriteFactory) -> Arc<HashMap<String, Vec<(usize, usize)>>> {
    if let Some(f) = doc.flow.lock().as_ref() {
        return f.clone();
    }
    let mut out: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    for (sid, paras) in &doc.stories {
        let heads: Vec<&Item> = doc.frames.values().filter(|f| f.text.as_ref().is_some_and(|t| &t.story == sid && t.prev.is_none())).collect();
        for head in heads {
            let mut chain = vec![head];
            while let Some(next) = chain.last().and_then(|f| f.text.as_ref()).and_then(|t| t.next.clone()) {
                match doc.frames.get(&next) {
                    Some(f) if chain.len() < 10_000 && !chain.iter().any(|c| c.id == f.id) => chain.push(f),
                    _ => break,
                }
            }
            let slots: Vec<(String, (f32, f32, f32, f32))> = chain.iter().flat_map(|f| columns(f).into_iter().map(move |c| (f.id.clone(), c))).collect();
            let mut i = 0;
            for (k, (fid, (x0, y0, x1, y1))) in slots.iter().enumerate() {
                let last = k + 1 == slots.len();
                let cap = (y1 - y0).max(0.0) + 0.5;
                let start = i;
                let mut used = 0.0;
                while i < paras.len() {
                    let p = &paras[i];
                    let pp = p.props();
                    let width = (x1 - x0) - pp.left_indent - pp.right_indent;
                    let h = layout(dw, p, width * WRAP_SLACK, "").map(|l| l.1).unwrap_or(pp.size * 1.2);
                    if used > 0.0 && used + h > cap && !last {
                        break;
                    }
                    used += h;
                    i += 1;
                    if used >= cap && !last {
                        break;
                    }
                }
                out.entry(fid.clone()).or_default().push((start, i));
            }
        }
    }
    let arc = Arc::new(out);
    *doc.flow.lock() = Some(arc.clone());
    arc
}

// ---------------------------------------------------------------- images

fn load_raster(src: &Source) -> Option<Raster> {
    let bgra = |b: gt_shell::info::Bgra| Raster { w: b.width, h: b.height, data: b.data };
    match src {
        Source::Embedded(bytes) => gt_fmt_media::cover::decode_bytes(bytes, 2048).map(bgra),
        Source::File(p) => {
            let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            match ext.as_str() {
                "svg" | "svgz" => {
                    let (s, _) = gt_fmt_doc::svg::open(p).ok()?;
                    render_page_source(&*s, 1600.0)
                }
                "pdf" | "ai" => {
                    let s = gt_fmt_doc::pdf::open(p).ok()?;
                    render_page_source(&*s, 1600.0)
                }
                "psd" | "psb" => {
                    let kind = gt_image::codecs::detect(&std::fs::read(p).ok()?.get(..64)?.to_vec(), &ext)?.0;
                    let img = gt_image::codecs::decode(p, kind, &Default::default()).ok()?;
                    match img.pixels {
                        gt_image::Pixels::Rgba8(v) => {
                            let mut data = v;
                            for px in data.chunks_exact_mut(4) {
                                let a = px[3] as u32;
                                let (r, g, b) = (px[0] as u32, px[1] as u32, px[2] as u32);
                                px[0] = (b * a / 255) as u8;
                                px[1] = (g * a / 255) as u8;
                                px[2] = (r * a / 255) as u8;
                            }
                            Some(Raster { w: img.width, h: img.height, data })
                        }
                        _ => None,
                    }
                }
                _ => gt_fmt_media::cover::decode_file(p, 2048).map(bgra),
            }
        }
        Source::Missing(_) => None,
    }
}

fn render_page_source(s: &dyn gt_fmt_doc::pages::PageSource, max: f32) -> Option<Raster> {
    let (w, h) = s.size(0);
    let k = max / w.max(h);
    s.render(0, k, 0, 0, (w * k).round().max(1.0) as u32, (h * k).round().max(1.0) as u32)
}

fn raster_for(doc: &Idml, src: &Source) -> Option<Arc<Raster>> {
    let key = match src {
        Source::File(p) => p.display().to_string(),
        Source::Embedded(b) => format!("embedded:{:p}", Arc::as_ptr(b)),
        Source::Missing(_) => return None,
    };
    if let Some(v) = doc.images.lock().get(&key) {
        return v.clone();
    }
    let r = load_raster(src).map(Arc::new);
    doc.images.lock().insert(key, r.clone());
    r
}

// ---------------------------------------------------------------- drawing

struct Painter<'a> {
    doc: &'a Idml,
    f: &'a Factories,
    rt: ID2D1RenderTarget,
    base: Mat,
    page_name: String,
    flow: Arc<HashMap<String, Vec<(usize, usize)>>>,
    text_out: Option<&'a mut String>,
}

impl Painter<'_> {
    fn geometry(&self, it: &Item) -> Option<ID2D1Geometry> {
        unsafe {
            // Rounded rectangle frames.
            if it.corner > 0.0 && it.paths.len() == 1 && it.paths[0].pts.len() == 4 {
                if let Some((x0, y0, x1, y1)) = it.bbox() {
                    let rr = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: x0, top: y0, right: x1, bottom: y1 }, radiusX: it.corner, radiusY: it.corner };
                    return self.f.d2d.CreateRoundedRectangleGeometry(&rr).ok().map(|g| g.into());
                }
            }
            let geo = self.f.d2d.CreatePathGeometry().ok()?;
            let sink = geo.Open().ok()?;
            sink.SetFillMode(D2D1_FILL_MODE_ALTERNATE);
            for sp in &it.paths {
                let n = sp.pts.len();
                if n == 0 {
                    continue;
                }
                let v = |p: (f32, f32)| Vector2 { X: p.0, Y: p.1 };
                sink.BeginFigure(v(sp.pts[0][0]), if sp.open { D2D1_FIGURE_BEGIN_HOLLOW } else { D2D1_FIGURE_BEGIN_FILLED });
                let seg = |a: &[(f32, f32); 3], b: &[(f32, f32); 3]| D2D1_BEZIER_SEGMENT { point1: v(a[2]), point2: v(b[1]), point3: v(b[0]) };
                for i in 1..n {
                    sink.AddBezier(&seg(&sp.pts[i - 1], &sp.pts[i]));
                }
                if !sp.open && n > 1 {
                    sink.AddBezier(&seg(&sp.pts[n - 1], &sp.pts[0]));
                }
                sink.EndFigure(if sp.open { D2D1_FIGURE_END_OPEN } else { D2D1_FIGURE_END_CLOSED });
            }
            sink.Close().ok()?;
            Some(geo.into())
        }
    }

    fn brush(&self, p: &Paint, opacity: f32, bbox: Option<(f32, f32, f32, f32)>) -> Option<ID2D1Brush> {
        unsafe {
            match p {
                Paint::Solid(c) => self.rt.CreateSolidColorBrush(&col(*c, opacity), None).ok().map(|b| b.into()),
                Paint::Gradient { stops, radial } => {
                    let st: Vec<D2D1_GRADIENT_STOP> = stops.iter().map(|(pos, c)| D2D1_GRADIENT_STOP { position: *pos, color: col(*c, opacity) }).collect();
                    let coll = self.rt.CreateGradientStopCollection(&st, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).ok()?;
                    let (x0, y0, x1, y1) = bbox.unwrap_or((0.0, 0.0, 1.0, 1.0));
                    if *radial {
                        let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
                            center: Vector2 { X: (x0 + x1) / 2.0, Y: (y0 + y1) / 2.0 },
                            gradientOriginOffset: Vector2 { X: 0.0, Y: 0.0 },
                            radiusX: (x1 - x0) / 2.0,
                            radiusY: (y1 - y0) / 2.0,
                        };
                        self.rt.CreateRadialGradientBrush(&props, None, &coll).ok().map(|b| b.into())
                    } else {
                        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: x0, Y: (y0 + y1) / 2.0 }, endPoint: Vector2 { X: x1, Y: (y0 + y1) / 2.0 } };
                        self.rt.CreateLinearGradientBrush(&props, None, &coll).ok().map(|b| b.into())
                    }
                }
            }
        }
    }

    fn set(&self, m: &Mat) {
        unsafe { self.rt.SetTransform(&mx(&m.then(&self.base))) };
    }

    fn push_clip(&self, geo: &ID2D1Geometry, opacity: f32) {
        unsafe {
            let params = D2D1_LAYER_PARAMETERS {
                contentBounds: D2D_RECT_F { left: f32::MIN, top: f32::MIN, right: f32::MAX, bottom: f32::MAX },
                geometricMask: ManuallyDrop::new(Some(geo.clone())),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: Matrix3x2::identity(),
                opacity,
                opacityBrush: ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS_NONE,
            };
            self.rt.PushLayer(&params, None);
            let mut p = params;
            ManuallyDrop::drop(&mut p.geometricMask);
        }
    }

    fn draw_placed(&self, pl: &Placed, frame: &Mat) {
        let t = pl.t.then(frame);
        let (l, top, r, b) = pl.bounds;
        if r <= l || b <= top {
            return;
        }
        self.set(&t);
        unsafe {
            match raster_for(self.doc, &pl.source) {
                Some(img) => {
                    let props = D2D1_BITMAP_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, dpiX: 96.0, dpiY: 96.0 };
                    if let Ok(bmp) = self.rt.CreateBitmap(D2D_SIZE_U { width: img.w, height: img.h }, Some(img.data.as_ptr() as _), img.w * 4, &props) {
                        let dst = D2D_RECT_F { left: l, top, right: r, bottom: b };
                        self.rt.DrawBitmap(&bmp, Some(&dst), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                    }
                }
                None => {
                    // Missing link: grey box with a cross, like InDesign's placeholder.
                    let rect = D2D_RECT_F { left: l, top, right: r, bottom: b };
                    if let Ok(br) = self.rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.85, g: 0.85, b: 0.85, a: 1.0 }, None) {
                        self.rt.FillRectangle(&rect, &br);
                    }
                    if let Ok(br) = self.rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.6, g: 0.6, b: 0.6, a: 1.0 }, None) {
                        self.rt.DrawLine(Vector2 { X: l, Y: top }, Vector2 { X: r, Y: b }, &br, 1.0, None);
                        self.rt.DrawLine(Vector2 { X: r, Y: top }, Vector2 { X: l, Y: b }, &br, 1.0, None);
                    }
                }
            }
        }
    }

    fn draw_text(&mut self, it: &Item, t: &Mat) {
        let Some(info) = &it.text else { return };
        let Some(paras) = self.doc.stories.get(&info.story) else { return };
        let Some(slots) = self.flow.get(&it.id).cloned() else { return };
        let cols = columns(it);
        self.set(t);
        for ((x0, y0, x1, y1), (start, end)) in cols.into_iter().zip(slots) {
            // Lines sit baseline to baseline, one leading apart, like InDesign;
            // the frame's first baseline is at the first line's ascent.
            let mut placed = Vec::new();
            let mut last_bl: Option<f32> = None;
            let mut prev_after = 0.0;
            let mut bottom = y0;
            for p in &paras[start.min(paras.len())..end.min(paras.len())] {
                let pp = p.props();
                let width = (x1 - x0) - pp.left_indent - pp.right_indent;
                let Some((lay, _)) = layout(&self.f.dw, p, width * WRAP_SLACK, &self.page_name) else { continue };
                let lead = leading(p);
                let lines = unsafe {
                    let mut m = DWRITE_TEXT_METRICS::default();
                    let _ = lay.GetMetrics(&mut m);
                    m.lineCount.max(1)
                };
                let size = p.runs.iter().map(|r| r.props.size).fold(0.0f32, f32::max).max(1.0);
                let first_bl = match last_bl {
                    None => y0 + pp.space_before + size * 0.78,
                    Some(b) => b + prev_after + pp.space_before + lead,
                };
                let top = first_bl - lead * 0.8;
                let bl = first_bl + (lines - 1) as f32 * lead;
                bottom = bl + size * 0.25;
                placed.push((lay, top, p));
                last_bl = Some(bl);
                prev_after = pp.space_after;
                if bl > y1 + lead {
                    break;
                }
            }
            let total = bottom - y0;
            let shift = match info.vjust {
                VJust::Top => 0.0,
                VJust::Center => ((y1 - y0) - total).max(0.0) / 2.0,
                VJust::Bottom => ((y1 - y0) - total).max(0.0),
            };
            unsafe {
                self.rt.PushAxisAlignedClip(&D2D_RECT_F { left: x0 - 2.0, top: y0 - 4.0, right: x1 + (x1 - x0) * (WRAP_SLACK - 1.0) + 2.0, bottom: y1 + 4.0 }, D2D1_ANTIALIAS_MODE_ALIASED);
            }
            for (lay, top, p) in placed {
                let pp = p.props();
                let (_, ranges) = para_text(p, &self.page_name);
                unsafe {
                    for (s, n, rp) in &ranges {
                        let paint = rp.color.clone().unwrap_or(Paint::Solid(Rgba([0.0, 0.0, 0.0, 1.0])));
                        let paint = match (paint, rp.tint) {
                            (Paint::Solid(Rgba(c)), t) if t < 1.0 => Paint::Solid(Rgba([1.0 - (1.0 - c[0]) * t, 1.0 - (1.0 - c[1]) * t, 1.0 - (1.0 - c[2]) * t, 1.0])),
                            (p, _) => p,
                        };
                        if let Some(b) = self.brush(&paint, 1.0, Some((x0, y0, x1, y1))) {
                            let _ = lay.SetDrawingEffect(&b.cast::<windows::core::IUnknown>().unwrap(), DWRITE_TEXT_RANGE { startPosition: *s, length: *n });
                        }
                    }
                    if let Ok(black) = self.rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }, None) {
                        self.rt.DrawTextLayout(Vector2 { X: x0 + pp.left_indent, Y: top + shift }, &lay, &black, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    }
                }
                if let Some(out) = self.text_out.as_deref_mut() {
                    let (u, _) = para_text(p, &self.page_name);
                    out.push_str(&String::from_utf16_lossy(&u));
                    out.push('\n');
                }
            }
            unsafe { self.rt.PopAxisAlignedClip() };
        }
    }

    fn draw_item(&mut self, it: &Item, parent: &Mat) {
        if !it.visible {
            return;
        }
        let t = it.t.then(parent);
        let geo = if it.paths.is_empty() { None } else { self.geometry(it) };
        let layered = it.opacity < 0.999;
        if layered {
            if let Some(g) = &geo {
                self.set(&t);
                // Opacity layer bounded by the item (children drawn into it too).
                let big: ID2D1Geometry = unsafe { self.f.d2d.CreateRectangleGeometry(&D2D_RECT_F { left: -1e6, top: -1e6, right: 1e6, bottom: 1e6 }).ok().map(|r| r.into()) }.unwrap_or(g.clone());
                self.push_clip(&big, it.opacity);
            }
        }
        if let Some(g) = &geo {
            self.set(&t);
            unsafe {
                if let Some(fill) = &it.fill {
                    if let Some(b) = self.brush(fill, 1.0, it.bbox()) {
                        self.rt.FillGeometry(g, &b, None);
                    }
                }
            }
        }
        // Placed graphics and nested items are clipped to the frame.
        if !it.placed.is_empty() || (!it.children.is_empty() && it.kind != "Group") {
            if let Some(g) = &geo {
                self.set(&t);
                self.push_clip(g, 1.0);
                for pl in &it.placed {
                    self.draw_placed(pl, &t);
                }
                for c in &it.children {
                    self.draw_item(c, &t);
                }
                unsafe { self.rt.PopLayer() };
            }
        } else {
            for c in &it.children {
                self.draw_item(c, &t);
            }
        }
        if it.text.is_some() {
            self.draw_text(it, &t);
        }
        if let (Some(g), Some(stroke)) = (&geo, &it.stroke) {
            if it.stroke_w > 0.0 {
                self.set(&t);
                unsafe {
                    if let Some(b) = self.brush(stroke, 1.0, it.bbox()) {
                        self.rt.DrawGeometry(g, &b, it.stroke_w, None);
                    }
                }
            }
        }
        if layered && geo.is_some() {
            unsafe { self.rt.PopLayer() };
        }
    }
}

fn draw_page(p: &mut Painter, page: usize) {
    let doc = p.doc;
    let (si, pi) = doc.pages[page];
    let sp = &doc.spreads[si];
    let pg = &sp.pages[pi];
    let hidden: Vec<&str> = doc.layers.iter().filter(|l| !l.1).map(|l| l.0.as_str()).collect();
    let layer_rank = |l: &str| doc.layers.iter().position(|x| x.0 == l).unwrap_or(doc.layers.len());
    let mut ranks: Vec<usize> = (0..=doc.layers.len()).collect();
    ranks.dedup();
    // Master page items first, under the page's own items.
    let master = pg.master.as_ref().and_then(|m| doc.masters.get(m));
    let master_shift = master.and_then(|m| {
        let k = m.pages.len();
        if k == 0 {
            return None;
        }
        let idx = if k >= 2 && pg.rect.0 >= -0.5 { 1.min(k - 1) } else { 0 };
        let mp = &m.pages[idx];
        Some(Mat::translate(pg.rect.0 - mp.rect.0, pg.rect.1 - mp.rect.1))
    });
    for rank in ranks {
        if let (Some(m), Some(shift)) = (master, master_shift) {
            for it in m.items.iter().filter(|i| layer_rank(&i.layer) == rank && !hidden.contains(&i.layer.as_str()) && !pg.overrides.contains(&i.id)) {
                p.draw_item(it, &shift);
            }
        }
        for it in sp.items.iter().filter(|i| layer_rank(&i.layer) == rank && !hidden.contains(&i.layer.as_str())) {
            p.draw_item(it, &Mat::ID);
        }
    }
}

pub fn render(doc: &Idml, page: usize, scale: f32, x: i32, y: i32, w: u32, h: u32) -> Option<Raster> {
    let (si, pi) = *doc.pages.get(page)?;
    let pg = &doc.spreads[si].pages[pi];
    with_factories(|f| unsafe {
        let bmp = f.wic.CreateBitmap(w, h, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad).ok()?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        let rt = f.d2d.CreateWicBitmapRenderTarget(&bmp, &props).ok()?;
        rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        rt.BeginDraw();
        rt.Clear(Some(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }));
        let base = Mat::translate(-pg.rect.0, -pg.rect.1).then(&Mat::scale(scale)).then(&Mat::translate(-x as f32, -y as f32));
        let flow = flow(doc, &f.dw);
        let mut p = Painter { doc, f, rt: rt.clone(), base, page_name: pg.name.clone(), flow, text_out: None };
        draw_page(&mut p, page);
        let _ = rt.EndDraw(None, None);
        let mut data = vec![0u8; (w * h * 4) as usize];
        bmp.CopyPixels(std::ptr::null(), w * 4, &mut data).ok()?;
        Some(Raster { w, h, data })
    })
}

/// The text of a page's frames (for Ctrl+C).
pub fn page_text(doc: &Idml, page: usize) -> Option<String> {
    let mut out = String::new();
    let r = render_text(doc, page, &mut out);
    r.map(|_| out)
}

fn render_text(doc: &Idml, page: usize, out: &mut String) -> Option<()> {
    let (si, pi) = *doc.pages.get(page)?;
    let pg = &doc.spreads[si].pages[pi];
    with_factories(|f| unsafe {
        // A tiny target: only the text pass matters.
        let bmp = f.wic.CreateBitmap(4, 4, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad).ok()?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        let rt = f.d2d.CreateWicBitmapRenderTarget(&bmp, &props).ok()?;
        rt.BeginDraw();
        let base = Mat::translate(-pg.rect.0, -pg.rect.1);
        let flow = flow(doc, &f.dw);
        let mut p = Painter { doc, f, rt: rt.clone(), base, page_name: pg.name.clone(), flow, text_out: Some(out) };
        for it in &doc.spreads[si].items {
            collect_text(&mut p, it, &Mat::ID, pg.rect);
        }
        let _ = rt.EndDraw(None, None);
        Some(())
    })
}

fn collect_text(p: &mut Painter, it: &Item, parent: &Mat, page: (f32, f32, f32, f32)) {
    let t = it.t.then(parent);
    if it.text.is_some() {
        if let Some((x0, y0, x1, y1)) = it.bbox() {
            let (cx, cy) = t.apply((x0 + x1) / 2.0, (y0 + y1) / 2.0);
            if cx >= page.0 && cx <= page.2 && cy >= page.1 && cy <= page.3 {
                p.draw_text(it, &t);
            }
        }
    }
    for c in &it.children {
        collect_text(p, c, &t, page);
    }
}

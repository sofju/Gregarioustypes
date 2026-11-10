//! Image view: fit-to-window like Quick Look, zoom about the cursor, pan, rotate,
//! EXIF/HEIF orientation, animation, and HDR-correct rendering on the scRGB layer.

use crate::gpu::{self, Cb, DrawTarget, Texture};
use crate::provider::FullImage;
use gt_image::{GainMapKind, Image, Lum, Pixels, Transfer};
use gt_ui::cx::{Cursor, Fonts};
use gt_ui::{glyph, Action, DrawCx, Event, EventCx, GpuCx, MouseButton, Rect, View, ViewEnv};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::RECT;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_0, VK_1, VK_ADD, VK_OEM_MINUS, VK_OEM_PLUS, VK_SUBTRACT};

struct Meta {
    w: u32,
    h: u32,
    orientation: u8,
    transfer: Transfer,
    to709: [[f32; 3]; 3],
    lum: Lum,
    lut_n: f32,
    peak: f32,
    gm: Option<GainMapKind>,
    full_size: Option<(u32, u32)>,
    subtitle: String,
    facts: Vec<(String, String)>,
}

pub struct ImageView {
    m: Meta,
    tex: Option<Texture>,
    gm: Option<Texture>,
    lut: Option<Texture>,
    srgb_hw: bool,
    frames: Vec<(Pixels, u32)>,
    frame: usize,
    due: Option<Instant>,
    rotation: u8,
    zoom: Option<f32>,
    center: (f32, f32),
    drag: Option<(f32, f32, (f32, f32))>,
    /// Last layout: rect, image origin and size on screen, fit scale.
    layout: (Rect, f32, f32, f32, f32, f32),
    error: Option<String>,
}

type Affine = [[f32; 3]; 2];

fn compose(f: Affine, g: Affine) -> Affine {
    let [[a1, b1, c1], [d1, e1, f1]] = f;
    let [[a2, b2, c2], [d2, e2, f2]] = g;
    [[a1 * a2 + b1 * d2, a1 * b2 + b1 * e2, a1 * c2 + b1 * f2 + c1], [d1 * a2 + e1 * d2, d1 * b2 + e1 * e2, d1 * c2 + e1 * f2 + f1]]
}

/// Display uv → stored uv for an EXIF orientation.
fn orient_inv(o: u8) -> Affine {
    match o {
        2 => [[-1.0, 0.0, 1.0], [0.0, 1.0, 0.0]],
        3 => [[-1.0, 0.0, 1.0], [0.0, -1.0, 1.0]],
        4 => [[1.0, 0.0, 0.0], [0.0, -1.0, 1.0]],
        5 => [[0.0, 1.0, 0.0], [1.0, 0.0, 0.0]],
        6 => [[0.0, 1.0, 0.0], [-1.0, 0.0, 1.0]],
        7 => [[0.0, -1.0, 1.0], [-1.0, 0.0, 1.0]],
        8 => [[0.0, -1.0, 1.0], [1.0, 0.0, 0.0]],
        _ => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
    }
}

fn uv_matrix(orientation: u8, rotation: u8) -> Affine {
    let mut m = orient_inv(orientation);
    for _ in 0..rotation % 4 {
        m = compose(m, orient_inv(6));
    }
    m
}

fn row(r: [f32; 3]) -> [f32; 4] {
    [r[0], r[1], r[2], 0.0]
}

impl ImageView {
    pub fn new(env: &ViewEnv, mut img: Image, peak: f32) -> Self {
        let mut v = ImageView {
            m: Self::meta(&img, peak),
            tex: None,
            gm: None,
            lut: None,
            srgb_hw: false,
            frames: Vec::new(),
            frame: 0,
            due: None,
            rotation: 0,
            zoom: None,
            center: (0.5, 0.5),
            drag: None,
            layout: (Rect::default(), 0.0, 0.0, 1.0, 1.0, 1.0),
            error: None,
        };
        v.upload(env, &mut img);
        v
    }

    fn meta(img: &Image, peak: f32) -> Meta {
        let mut facts = vec![("Format".to_string(), img.format.to_string())];
        let mut dims = format!("{} × {} pixels", img.width, img.height);
        if let Some((fw, fh)) = img.full_size {
            dims = format!("{fw} × {fh} pixels (previewing {}×{})", img.width, img.height);
        }
        facts.push(("Dimensions".into(), dims));
        facts.push(("Colour".into(), img.color.name.clone()));
        facts.push(("Bit depth".into(), format!("{} bits per channel{}", img.bit_depth, if img.alpha { ", alpha" } else { "" })));
        if let Some(h) = img.hdr_label() {
            facts.push(("HDR".into(), format!("{h}, content peak ≈ {peak:.0} nits")));
        }
        if let Some(g) = &img.gain_map {
            facts.push(("Gain map".into(), format!("{}×{}, up to {:.1}× ({:.1} stops)", g.width, g.height, g.max_boost(), g.max_boost().log2())));
        }
        if img.frame_count > 1 {
            facts.push(("Frames".into(), format!("{}{}", img.frame_count, if img.frames.len() + 1 < img.frame_count as usize { " (first frames shown)" } else { "" })));
        }
        facts.extend(img.facts.iter().cloned());
        let (fw, fh) = img.full_size.unwrap_or((img.width, img.height));
        let (dw, dh) = if img.orientation >= 5 { (fh, fw) } else { (fw, fh) };
        let mut sub = vec![format!("{dw} × {dh}"), img.color.name.clone()];
        if let Some(h) = img.hdr_label() {
            sub.push(h);
        }
        if img.frame_count > 1 {
            sub.push(format!("{} frames", img.frame_count));
        }
        Meta {
            w: img.width,
            h: img.height,
            orientation: img.orientation,
            transfer: img.color.transfer,
            to709: img.color.to709,
            lum: img.color.lum,
            lut_n: img.color.lut.as_ref().map(|l| l.len() as f32).unwrap_or(0.0),
            peak,
            gm: img.gain_map.as_ref().map(|g| g.kind.clone()),
            full_size: img.full_size,
            subtitle: sub.join(" · "),
            facts,
        }
    }

    fn upload(&mut self, env: &ViewEnv, img: &mut Image) {
        let (dev, ctx) = (&env.gfx.d3d, &env.gfx.d3d_ctx);
        self.srgb_hw = matches!(img.pixels, Pixels::Rgba8(_)) && img.color.transfer == Transfer::Srgb;
        match gpu::upload(dev, Some(ctx), &img.pixels, img.width, img.height, self.srgb_hw, true) {
            Ok(t) => self.tex = Some(t),
            Err(e) => self.error = Some(format!("GPU upload failed: {}", e.message())),
        }
        self.gm = img.gain_map.as_ref().and_then(|g| gpu::upload(dev, Some(ctx), &g.pixels, g.width, g.height, false, true).ok());
        self.lut = img.color.lut.as_ref().and_then(|l| gpu::lut_texture(dev, l).ok());
        self.frames.clear();
        self.frame = 0;
        self.due = None;
        if !img.frames.is_empty() {
            let first = std::mem::replace(&mut img.pixels, Pixels::Rgba8(Vec::new()));
            self.frames.push((first, img.first_delay_ms.max(20)));
            for f in img.frames.drain(..) {
                self.frames.push((f.pixels, f.delay_ms.max(20)));
            }
            self.due = Some(Instant::now() + Duration::from_millis(self.frames[0].1 as u64));
        }
    }

    /// Size of the image as displayed (orientation + user rotation), at full resolution.
    fn display_dims(&self) -> (f32, f32) {
        let (w, h) = self.m.full_size.unwrap_or((self.m.w, self.m.h));
        let swap = (self.m.orientation >= 5) ^ (self.rotation % 2 == 1);
        if swap {
            (h as f32, w as f32)
        } else {
            (w as f32, h as f32)
        }
    }

    fn compute_layout(&mut self, rect: Rect) -> (f32, f32, f32, f32, f32) {
        let (dw, dh) = self.display_dims();
        let fit = (rect.w / dw).min(rect.h / dh).min(1.0);
        let s = self.zoom.unwrap_or(fit);
        let (iw, ih) = (dw * s, dh * s);
        if self.zoom.is_some() {
            // Keep the image covering the view on axes where it is larger.
            let clamp = |c: f32, view: f32, img: f32| if img <= view { 0.5 } else { c.clamp(view / 2.0 / img, 1.0 - view / 2.0 / img) };
            self.center = (clamp(self.center.0, rect.w, iw), clamp(self.center.1, rect.h, ih));
        }
        let (x0, y0) = if self.zoom.is_none() {
            (rect.x + (rect.w - iw) / 2.0, rect.y + (rect.h - ih) / 2.0)
        } else {
            (rect.x + rect.w / 2.0 - self.center.0 * iw, rect.y + rect.h / 2.0 - self.center.1 * ih)
        };
        let (x0, y0) = (x0.round(), y0.round());
        self.layout = (rect, x0, y0, iw, ih, fit);
        (x0, y0, iw, ih, fit)
    }

    fn set_zoom_at(&mut self, new: f32, x: f32, y: f32) {
        let (rect, x0, y0, iw, ih, fit) = self.layout;
        let (dw, dh) = self.display_dims();
        let new = new.clamp(fit.min(0.05), 32.0);
        if new <= fit + 1e-4 {
            self.zoom = None;
            self.center = (0.5, 0.5);
            return;
        }
        let p = (((x - x0) / iw).clamp(0.0, 1.0), ((y - y0) / ih).clamp(0.0, 1.0));
        let (niw, nih) = (dw * new, dh * new);
        let (cx, cy) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
        self.center = (p.0 - (x - cx) / niw, p.1 - (y - cy) / nih);
        self.zoom = Some(new);
    }

    fn current_scale(&self) -> f32 {
        self.zoom.unwrap_or(self.layout.5)
    }
}

impl View for ImageView {
    fn kind(&self) -> &'static str {
        "image"
    }

    fn preferred_size(&mut self, _fonts: &mut Fonts, _scale: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        let (dw, dh) = self.display_dims();
        let k = (max_w / dw).min(max_h / dh).min(1.0);
        (dw * k, dh * k)
    }

    fn uses_hdr_layer(&self) -> bool {
        true
    }

    fn render_gpu(&mut self, gpu: &mut GpuCx, rect: Rect) {
        if self.tex.is_none() {
            return;
        }
        let Ok(pipe) = gpu::pipeline(gpu.device) else { return };
        let (x0, y0, iw, ih, fit) = self.compute_layout(rect);
        let Some(tex) = &self.tex else { return };
        let (tw, th) = (gpu.target_w as f32, gpu.target_h as f32);
        let m = &self.m;
        let uv = uv_matrix(m.orientation, self.rotation);
        let transfer_id = if self.srgb_hw {
            0.0
        } else {
            match m.transfer {
                Transfer::Linear => 0.0,
                Transfer::Srgb => 1.0,
                Transfer::Gamma(_) => 2.0,
                Transfer::Bt709 => 3.0,
                Transfer::Pq => 4.0,
                Transfer::Hlg => 5.0,
                Transfer::Lut => {
                    if self.lut.is_some() {
                        6.0
                    } else {
                        1.0
                    }
                }
            }
        };
        let gamma = if let Transfer::Gamma(g) = m.transfer { g } else { 1.0 };
        let lum = match m.lum {
            Lum::Relative => 0.0,
            Lum::Pq => 1.0,
            Lum::Hlg => 2.0,
            Lum::ScRgb => 3.0,
        };
        // Display mapping: scRGB with SDR white at the user's SDR level when HDR is on;
        // with HDR off, BT.2408's 203-nit reference white maps to SDR white.
        let (rel, abs, peak) = if gpu.hdr { (gpu.sdr_white_nits / 80.0, 1.0 / 80.0, gpu.peak_nits / 80.0) } else { (1.0, 1.0 / 203.0, 1.0) };
        let headroom = if gpu.hdr { (gpu.peak_nits / gpu.sdr_white_nits).max(1.0) } else { 1.0 };
        let mut cb = Cb {
            dst: [x0 / tw * 2.0 - 1.0, 1.0 - y0 / th * 2.0, iw / tw * 2.0, ih / th * 2.0],
            uvx: [uv[0][0], uv[0][1], uv[0][2], 0.0],
            uvy: [uv[1][0], uv[1][1], uv[1][2], 0.0],
            m0: row(m.to709[0]),
            m1: row(m.to709[1]),
            m2: row(m.to709[2]),
            tparams: [transfer_id, gamma, lum, m.lut_n],
            misc: [1.0, 0.0, 0.0, 0.0],
            ..Default::default()
        };
        let mut content_peak = match m.lum {
            Lum::Relative => m.peak / 203.0 * rel,
            _ => m.peak * abs,
        };
        match (&m.gm, &self.gm) {
            (Some(GainMapKind::Iso { min, max, gamma, base_offset, alt_offset, base_headroom, alt_headroom }), Some(_)) => {
                let span = alt_headroom - base_headroom;
                let w = if span.abs() < 1e-6 { 0.0 } else { ((headroom.log2() - base_headroom) / span).clamp(0.0, 1.0) };
                cb.gm_min = [min[0], min[1], min[2], 0.0];
                cb.gm_max = [max[0], max[1], max[2], 0.0];
                cb.gm_gamma = [gamma[0], gamma[1], gamma[2], 1.0];
                cb.gm_off_base = [base_offset[0], base_offset[1], base_offset[2], 0.0];
                cb.gm_off_alt = [alt_offset[0], alt_offset[1], alt_offset[2], 0.0];
                cb.gm_params = [1.0, w, 0.0, 0.0];
                let top = max.iter().cloned().fold(f32::MIN, f32::max) * w;
                content_peak = content_peak.max(rel * 2f32.powf(top));
            }
            (Some(GainMapKind::Apple { headroom: h }), Some(_)) => {
                let eff = h.min(headroom).max(1.0);
                cb.gm_params = [2.0, eff, 0.0, 0.0];
                content_peak = content_peak.max(rel * eff);
            }
            _ => {}
        }
        cb.scales = [rel, abs, peak, content_peak];
        let scissor = RECT { left: rect.x as i32, top: rect.y as i32, right: rect.right() as i32, bottom: rect.bottom() as i32 };
        let target = DrawTarget { rtv: gpu.rtv, width: gpu.target_w, height: gpu.target_h, scissor };
        let scale = self.zoom.unwrap_or(fit) * self.m.full_size.map(|(fw, _)| fw as f32 / self.m.w as f32).unwrap_or(1.0);
        pipe.draw(gpu.ctx, &target, &cb, tex, self.gm.as_ref(), self.lut.as_ref(), scale >= 3.0);
    }

    fn draw(&mut self, cx: &mut DrawCx, r: Rect) {
        if let Some(e) = &self.error {
            cx.text(e, r, gt_ui::Font::Ui(13.0), cx.theme.text2, gt_ui::Align::Center);
        }
    }

    fn event(&mut self, cx: &mut EventCx, _r: Rect, ev: &Event) {
        match *ev {
            Event::Wheel { x, y, lines, ctrl: true, .. } => {
                let s = self.current_scale();
                self.set_zoom_at(s * 1.25f32.powf(lines), x, y);
                cx.toast = Some(format!("{:.0}%", self.current_scale() * 100.0 * self.m.full_size.map(|_| 1.0).unwrap_or(1.0)));
                cx.redraw = true;
            }
            Event::Wheel { lines, horizontal, .. } if self.zoom.is_some() => {
                let (rect, _, _, iw, ih, _) = self.layout;
                let step = lines * 0.12;
                if horizontal {
                    self.center.0 -= step * rect.w / iw;
                } else {
                    self.center.1 -= step * rect.h / ih;
                }
                cx.redraw = true;
            }
            Event::MouseDown { x, y, button: MouseButton::Left, double: true, .. } => {
                if self.zoom.is_some() {
                    self.zoom = None;
                    self.center = (0.5, 0.5);
                } else {
                    self.set_zoom_at(1.0, x, y);
                }
                cx.redraw = true;
            }
            Event::MouseDown { x, y, button: MouseButton::Left, .. } if self.zoom.is_some() => {
                self.drag = Some((x, y, self.center));
                cx.capture = Some(true);
            }
            Event::MouseMove { x, y } => {
                if let Some((sx, sy, c)) = self.drag {
                    let (_, _, _, iw, ih, _) = self.layout;
                    self.center = (c.0 - (x - sx) / iw, c.1 - (y - sy) / ih);
                    cx.redraw = true;
                }
                cx.cursor = if self.zoom.is_some() { Cursor::Hand } else { Cursor::Arrow };
            }
            Event::MouseUp { .. } => {
                if self.drag.take().is_some() {
                    cx.capture = Some(false);
                }
            }
            Event::Key { vk, ctrl: true, .. } => {
                let (rect, ..) = self.layout;
                let (cxp, cyp) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
                let s = self.current_scale();
                if vk == VK_0.0 {
                    self.zoom = None;
                } else if vk == VK_1.0 {
                    self.set_zoom_at(1.0, cxp, cyp);
                } else if vk == VK_OEM_PLUS.0 || vk == VK_ADD.0 {
                    self.set_zoom_at(s * 1.25, cxp, cyp);
                } else if vk == VK_OEM_MINUS.0 || vk == VK_SUBTRACT.0 {
                    self.set_zoom_at(s / 1.25, cxp, cyp);
                }
                cx.redraw = true;
            }
            _ => {}
        }
    }

    fn actions(&self) -> Vec<Action> {
        vec![
            Action::new("rotate", glyph::ROTATE, "Rotate clockwise"),
            Action::new("actual", glyph::ZOOM_IN, if self.zoom.is_some() { "Fit to window (Ctrl+0)" } else { "Actual size (Ctrl+1)" }).active(self.zoom.is_some()),
        ]
    }

    fn on_action(&mut self, id: &str, cx: &mut EventCx) {
        match id {
            "rotate" => {
                self.rotation = (self.rotation + 1) % 4;
                self.zoom = None;
                self.center = (0.5, 0.5);
                cx.relayout = true;
            }
            "actual" => {
                if self.zoom.is_some() {
                    self.zoom = None;
                } else {
                    let (rect, ..) = self.layout;
                    self.set_zoom_at(1.0, rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
                }
            }
            _ => {}
        }
        cx.redraw = true;
    }

    fn upgrade(&mut self, env: &ViewEnv, payload: Box<dyn std::any::Any + Send>) -> bool {
        let Ok(full) = payload.downcast::<FullImage>() else { return false };
        let FullImage { mut img, peak } = *full;
        let old_w = self.display_dims().0;
        self.m = Self::meta(&img, peak);
        self.upload(env, &mut img);
        let new_w = self.display_dims().0;
        if let Some(z) = self.zoom.as_mut() {
            *z *= old_w / new_w.max(1.0);
        }
        true
    }

    fn next_frame_in(&self) -> Option<Duration> {
        self.due.map(|d| d.saturating_duration_since(Instant::now()))
    }

    fn tick(&mut self, env: &ViewEnv) -> bool {
        let Some(due) = self.due else { return false };
        if Instant::now() < due || self.frames.len() < 2 {
            return false;
        }
        self.frame = (self.frame + 1) % self.frames.len();
        if let Some(t) = &self.tex {
            gpu::update(&env.gfx.d3d_ctx, t, &self.frames[self.frame].0);
        }
        self.due = Some(Instant::now() + Duration::from_millis(self.frames[self.frame].1 as u64));
        true
    }

    fn subtitle(&self) -> Option<String> {
        Some(self.m.subtitle.clone())
    }

    fn info(&self) -> Vec<(String, String)> {
        self.m.facts.clone()
    }
}

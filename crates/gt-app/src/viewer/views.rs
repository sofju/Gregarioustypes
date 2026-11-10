//! Built-in views: the loading placeholder and the HDR test pattern.

use gt_render::DisplayInfo;
use gt_ui::cx::Fonts;
use gt_ui::{rgba, Align, DrawCx, Font, Rect, View};

/// Shown while a slow preview loads (after ~80 ms), like Quick Look's icon stage.
pub struct LoadingView {
    pub name: String,
}

impl View for LoadingView {
    fn kind(&self) -> &'static str {
        "loading"
    }
    fn preferred_size(&mut self, _f: &mut Fonts, s: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        ((640.0 * s).min(max_w), (360.0 * s).min(max_h))
    }
    fn draw(&mut self, cx: &mut DrawCx, r: Rect) {
        let h = cx.px(24.0);
        cx.text("Loading preview…", Rect::new(r.x, r.y + r.h / 2.0 - h, r.w, h), Font::Ui(14.0), cx.theme.text2, Align::Center);
        cx.text(&self.name, Rect::new(r.x, r.y + r.h / 2.0, r.w, h), Font::Ui(12.0), cx.theme.text3, Align::Center);
    }
}

/// Grey bars at fixed luminance levels drawn on the FP16 scRGB layer. With Windows
/// HDR on, bars brighten up to the panel peak and then clip; with HDR off everything
/// above SDR white clips.
pub struct HdrTest {
    pub info: DisplayInfo,
    pub peak_override: Option<f32>,
}

const LEVELS: [f32; 11] = [0.0, 80.0, 203.0, 400.0, 600.0, 800.0, 1000.0, 1100.0, 1500.0, 2000.0, 4000.0];

impl HdrTest {
    fn bar(r: Rect, i: usize) -> Rect {
        let n = LEVELS.len() as f32;
        let w = r.w / n;
        Rect::new(r.x + i as f32 * w, r.y, w, r.h * 0.62).snap()
    }
}

impl View for HdrTest {
    fn kind(&self) -> &'static str {
        "hdr-test"
    }
    fn preferred_size(&mut self, _f: &mut Fonts, s: f32, max_w: f32, max_h: f32) -> (f32, f32) {
        ((1100.0 * s).min(max_w), (640.0 * s).min(max_h))
    }
    fn uses_hdr_layer(&self) -> bool {
        true
    }
    fn draw_hdr(&mut self, cx: &mut DrawCx, r: Rect) {
        cx.fill(r, rgba(0.0, 0.0, 0.0, 1.0));
        for (i, nits) in LEVELS.iter().enumerate() {
            let v = nits / 80.0;
            cx.fill(Self::bar(r, i).inset(cx.px(3.0)), rgba(v, v, v, 1.0));
        }
        // Smooth ramp 0 → 2000 nits along the bottom.
        let ramp = Rect::new(r.x, r.y + r.h * 0.70, r.w, r.h * 0.12);
        let steps = 256;
        for k in 0..steps {
            let v = (k as f32 / (steps - 1) as f32) * 2000.0 / 80.0;
            let x0 = ramp.x + ramp.w * k as f32 / steps as f32;
            let x1 = ramp.x + ramp.w * (k + 1) as f32 / steps as f32;
            cx.fill(Rect::new(x0.floor(), ramp.y, (x1 - x0).ceil() + 1.0, ramp.h), rgba(v, v, v, 1.0));
        }
        // SDR white reference patch.
        let s = cx.sdr_scale;
        cx.fill(Rect::new(r.x + r.w * 0.02, r.y + r.h * 0.86, r.w * 0.12, r.h * 0.1), rgba(s, s, s, 1.0));
    }
    fn draw(&mut self, cx: &mut DrawCx, r: Rect) {
        for (i, nits) in LEVELS.iter().enumerate() {
            let b = Self::bar(r, i);
            let label = Rect::new(b.x, b.bottom() - cx.px(28.0), b.w, cx.px(22.0));
            cx.round(label.inset_xy(cx.px(8.0), 0.0), cx.px(4.0), rgba(0.0, 0.0, 0.0, 0.65));
            cx.text(&format!("{nits:.0} nits"), label, Font::Semibold(11.5), rgba(1.0, 1.0, 1.0, 1.0), Align::Center);
        }
        let i = &self.info;
        let peak = self.peak_override.unwrap_or(i.max_nits);
        let mut text = format!(
            "{}   ·   Windows HDR: {}   ·   SDR white: {:.0} nits   ·   Peak used: {:.0} nits{}   ·   DXGI reports {:.0} / {:.0} full-frame nits, {} bpc",
            if i.device.is_empty() { "Display" } else { &i.device },
            if i.hdr { "ON" } else { "off" },
            i.sdr_white_nits,
            peak,
            if self.peak_override.is_some() { " (override)" } else { "" },
            i.max_nits,
            i.max_full_frame_nits,
            i.bits_per_color
        );
        if !i.hdr {
            text.push_str("   ·   Turn on “Use HDR” in Settings > Display to see bars above 80 nits.");
        }
        let tr = Rect::new(r.x + r.w * 0.16, r.y + r.h * 0.86, r.w * 0.82, r.h * 0.1);
        cx.text_wrapped(&text, tr, Font::Ui(12.5), rgba(0.85, 0.85, 0.85, 1.0));
        let rr = Rect::new(r.x + r.w * 0.02, r.y + r.h * 0.83, r.w * 0.3, cx.px(20.0));
        cx.text("SDR white reference ↓", rr, Font::Ui(11.5), rgba(0.75, 0.75, 0.75, 1.0), Align::Left);
    }
    fn info(&self) -> Vec<(String, String)> {
        vec![
            ("HDR".into(), if self.info.hdr { "On".into() } else { "Off".into() }),
            ("SDR white".into(), format!("{:.0} nits", self.info.sdr_white_nits)),
            ("Peak (DXGI)".into(), format!("{:.0} nits", self.info.max_nits)),
        ]
    }
}

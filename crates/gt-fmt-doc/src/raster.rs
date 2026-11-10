//! Pages that are only available as bitmaps (previews embedded in InDesign,
//! Xara or Affinity files), shown in the page viewer: resampled (bilinear) to
//! whatever scale the view asks for.

use crate::pages::{PageSource, Raster};

pub struct RasterPage {
    pub raster: Raster,
    /// Page size in units (points); the bitmap is stretched to it.
    pub size: (f32, f32),
}

pub struct RasterSource {
    pub pages: Vec<RasterPage>,
    pub paper: bool,
    pub dip_per_unit: f32,
}

impl RasterSource {
    /// One bitmap shown at its own pixel size (1 unit = 1 pixel).
    pub fn single(raster: Raster, paper: bool) -> Self {
        let size = (raster.w as f32, raster.h as f32);
        RasterSource { pages: vec![RasterPage { raster, size }], paper, dip_per_unit: 1.0 }
    }
}

fn sample(r: &Raster, sx: f32, sy: f32) -> [f32; 4] {
    let (w, h) = (r.w as i32, r.h as i32);
    let x0 = (sx.floor() as i32).clamp(0, w - 1);
    let y0 = (sy.floor() as i32).clamp(0, h - 1);
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = (sx - sx.floor()).clamp(0.0, 1.0);
    let fy = (sy - sy.floor()).clamp(0.0, 1.0);
    let px = |x: i32, y: i32| {
        let o = ((y * w + x) * 4) as usize;
        [r.data[o] as f32, r.data[o + 1] as f32, r.data[o + 2] as f32, r.data[o + 3] as f32]
    };
    let (a, b, c, d) = (px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1));
    let mut out = [0f32; 4];
    for k in 0..4 {
        let top = a[k] + (b[k] - a[k]) * fx;
        let bot = c[k] + (d[k] - c[k]) * fx;
        out[k] = top + (bot - top) * fy;
    }
    out
}

impl PageSource for RasterSource {
    fn count(&self) -> usize {
        self.pages.len()
    }
    fn size(&self, page: usize) -> (f32, f32) {
        self.pages.get(page).map(|p| p.size).unwrap_or((612.0, 792.0))
    }
    fn dip_per_unit(&self) -> f32 {
        self.dip_per_unit
    }
    fn paper(&self) -> bool {
        self.paper
    }
    fn render(&self, page: usize, scale: f32, x: i32, y: i32, w: u32, h: u32) -> Option<Raster> {
        let p = self.pages.get(page)?;
        let r = &p.raster;
        if r.w == 0 || r.h == 0 {
            return None;
        }
        // Target pixel → bitmap pixel.
        let kx = r.w as f32 / (p.size.0 * scale);
        let ky = r.h as f32 / (p.size.1 * scale);
        // Shrinking a lot: average a few taps instead of aliasing.
        let taps = if kx > 2.0 { 2 } else { 1 };
        let mut data = vec![0u8; (w * h * 4) as usize];
        for ty in 0..h {
            for tx in 0..w {
                let mut acc = [0f32; 4];
                for j in 0..taps {
                    for i in 0..taps {
                        let fx = (x as f32 + tx as f32 + (i as f32 + 0.5) / taps as f32) * kx - 0.5;
                        let fy = (y as f32 + ty as f32 + (j as f32 + 0.5) / taps as f32) * ky - 0.5;
                        let s = sample(r, fx, fy);
                        for k in 0..4 {
                            acc[k] += s[k];
                        }
                    }
                }
                let n = (taps * taps) as f32;
                let o = ((ty * w + tx) * 4) as usize;
                for k in 0..4 {
                    data[o + k] = (acc[k] / n).round().clamp(0.0, 255.0) as u8;
                }
                if self.paper {
                    // Opaque page: composite on white.
                    let a = data[o + 3] as u32;
                    for k in 0..3 {
                        data[o + k] = (data[o + k] as u32 + (255 - a)).min(255) as u8;
                    }
                    data[o + 3] = 255;
                }
            }
        }
        Some(Raster { w, h, data })
    }
}

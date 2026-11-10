//! Window surface made of two DirectComposition layers:
//! * `content` (non-topmost target): FP16 scRGB swapchain for images, video frames and
//!   anything HDR. Allocated lazily — 2880×1800 FP16 is ~40 MB per buffer.
//! * `ui` (topmost target): BGRA8 premultiplied swapchain for chrome and SDR 2D views.
//!   Being topmost, it also draws above child windows (mpv, RichEdit). DWM maps it to
//!   the SDR white level automatically.

use crate::gfx::Gfx;
use windows::core::{Interface, Result};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{ID2D1Bitmap1, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1};
use windows::Win32::Graphics::DirectComposition::{IDCompositionTarget, IDCompositionVisual2};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT,
    DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGISurface, IDXGISwapChain1, IDXGISwapChain3, DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayerKind {
    /// FP16 linear scRGB (1.0 = 80 nits).
    Hdr,
    /// 8-bit sRGB, premultiplied.
    Sdr,
}

pub struct Layer {
    pub kind: LayerKind,
    pub visual: IDCompositionVisual2,
    swap: Option<IDXGISwapChain1>,
    target: Option<ID2D1Bitmap1>,
    size: (u32, u32),
}

impl Layer {
    fn format(&self) -> DXGI_FORMAT {
        match self.kind {
            LayerKind::Hdr => DXGI_FORMAT_R16G16B16A16_FLOAT,
            LayerKind::Sdr => DXGI_FORMAT_B8G8R8A8_UNORM,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn is_allocated(&self) -> bool {
        self.swap.is_some()
    }

    /// Create or resize the swapchain to `w`×`h` physical pixels.
    pub fn ensure(&mut self, gfx: &Gfx, w: u32, h: u32) -> Result<()> {
        let (w, h) = (w.max(1), h.max(1));
        if self.swap.is_some() && self.size == (w, h) {
            return Ok(());
        }
        unsafe {
            self.target = None;
            gfx.dc.SetTarget(None);
            if let Some(swap) = &self.swap {
                swap.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
            } else {
                let desc = DXGI_SWAP_CHAIN_DESC1 {
                    Width: w,
                    Height: h,
                    Format: self.format(),
                    Stereo: false.into(),
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                    BufferCount: 2,
                    Scaling: DXGI_SCALING_STRETCH,
                    SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                    AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                    Flags: 0,
                };
                let swap = gfx.factory.CreateSwapChainForComposition(&gfx.d3d, &desc, None)?;
                if self.kind == LayerKind::Hdr {
                    let s3: IDXGISwapChain3 = swap.cast()?;
                    s3.SetColorSpace1(DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709)?;
                }
                self.visual.SetContent(&swap)?;
                self.swap = Some(swap);
            }
        }
        self.size = (w, h);
        Ok(())
    }

    /// Release the swapchain (hidden window, or a view without HDR content).
    pub fn release(&mut self, gfx: &Gfx) {
        unsafe {
            gfx.dc.SetTarget(None);
            let _ = self.visual.SetContent(None);
        }
        self.target = None;
        self.swap = None;
        self.size = (0, 0);
    }

    /// Bind the back buffer as the D2D target and begin drawing.
    pub fn begin(&mut self, gfx: &Gfx) -> Result<()> {
        let swap = self.swap.as_ref().ok_or_else(windows::core::Error::empty)?;
        unsafe {
            if self.target.is_none() {
                let surface: IDXGISurface = swap.GetBuffer(0)?;
                let props = D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT { format: self.format(), alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                    colorContext: std::mem::ManuallyDrop::new(None),
                };
                self.target = Some(gfx.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?);
            }
            if let Some(t) = &self.target {
                gfx.dc.SetTarget(t);
            }
            gfx.dc.SetDpi(96.0, 96.0);
            gfx.dc.BeginDraw();
        }
        Ok(())
    }

    /// D3D11 render-target view of the current back buffer (drop before resizing).
    pub fn rtv(&mut self, gfx: &Gfx) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView> {
        let swap = self.swap.as_ref().ok_or_else(windows::core::Error::empty)?;
        unsafe {
            let tex: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D = swap.GetBuffer(0)?;
            let mut rtv = None;
            gfx.d3d.CreateRenderTargetView(&tex, None, Some(&mut rtv))?;
            rtv.ok_or_else(windows::core::Error::empty)
        }
    }

    /// Debug: copy the FP16 back buffer to the CPU and return (max, 99th percentile,
    /// mean) of max(R,G,B) inside `rect` (x, y, w, h). scRGB units: 1.0 = 80 nits.
    pub fn readback_stats(&mut self, gfx: &Gfx, rect: (u32, u32, u32, u32)) -> Option<(f32, f32, f32)> {
        use windows::Win32::Graphics::Direct3D11::*;
        if self.kind != LayerKind::Hdr {
            return None;
        }
        let swap = self.swap.as_ref()?;
        unsafe {
            let tex: ID3D11Texture2D = swap.GetBuffer(0).ok()?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            tex.GetDesc(&mut desc);
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            let mut staging = None;
            gfx.d3d.CreateTexture2D(&desc, None, Some(&mut staging)).ok()?;
            let staging = staging?;
            gfx.d3d_ctx.CopyResource(&staging, &tex);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            gfx.d3d_ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m)).ok()?;
            let half = |b: u16| -> f32 {
                let s = if b & 0x8000 != 0 { -1.0 } else { 1.0 };
                let e = ((b >> 10) & 0x1F) as i32;
                let f = (b & 0x3FF) as f32;
                s * if e == 0 { f / 1024.0 * 2f32.powi(-14) } else if e == 31 { f32::INFINITY } else { (1.0 + f / 1024.0) * 2f32.powi(e - 15) }
            };
            let (x0, y0, w, h) = rect;
            let mut vals = Vec::with_capacity((w * h / 4) as usize);
            let mut sum = 0.0f64;
            for y in (y0..(y0 + h).min(desc.Height)).step_by(2) {
                let row = (m.pData as *const u8).add((y * m.RowPitch) as usize) as *const u16;
                for x in (x0..(x0 + w).min(desc.Width)).step_by(2) {
                    let p = row.add((x * 4) as usize);
                    let v = half(*p).max(half(*p.add(1))).max(half(*p.add(2)));
                    sum += v as f64;
                    vals.push(v);
                }
            }
            gfx.d3d_ctx.Unmap(&staging, 0);
            if vals.is_empty() {
                return None;
            }
            let mean = (sum / vals.len() as f64) as f32;
            let k = (vals.len() as f32 * 0.99) as usize;
            let k = k.min(vals.len() - 1);
            vals.select_nth_unstable_by(k, |a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let p99 = vals[k];
            let max = vals.iter().cloned().fold(f32::MIN, f32::max);
            Some((max, p99, mean))
        }
    }

    /// End D2D drawing without presenting (for layers that mix D3D and D2D).
    pub fn end_draw(&mut self, gfx: &Gfx) -> Result<()> {
        unsafe {
            let r = gfx.dc.EndDraw(None, None);
            gfx.dc.SetTarget(None);
            self.target = None;
            r
        }
    }

    pub fn present(&mut self) -> Result<()> {
        match &self.swap {
            Some(swap) => unsafe { swap.Present(1, DXGI_PRESENT(0)).ok() },
            None => Ok(()),
        }
    }

    /// Finish drawing and present. On flip-model swapchains the back buffer is
    /// re-bound each frame, so the D2D target bitmap is dropped after presenting.
    pub fn end(&mut self, gfx: &Gfx) -> Result<()> {
        unsafe {
            let r = gfx.dc.EndDraw(None, None);
            gfx.dc.SetTarget(None);
            self.target = None;
            if let Some(swap) = &self.swap {
                swap.Present(1, DXGI_PRESENT(0)).ok()?;
            }
            r
        }
    }
}

pub struct Surface {
    pub hwnd: HWND,
    _target_content: IDCompositionTarget,
    _target_ui: IDCompositionTarget,
    pub content: Layer,
    pub ui: Layer,
}

impl Surface {
    pub fn new(gfx: &Gfx, hwnd: HWND) -> Result<Self> {
        unsafe {
            let target_content = gfx.dcomp.CreateTargetForHwnd(hwnd, false)?;
            let target_ui = gfx.dcomp.CreateTargetForHwnd(hwnd, true)?;
            let v_content = gfx.dcomp.CreateVisual()?;
            let v_ui = gfx.dcomp.CreateVisual()?;
            target_content.SetRoot(&v_content)?;
            target_ui.SetRoot(&v_ui)?;
            Ok(Self {
                hwnd,
                _target_content: target_content,
                _target_ui: target_ui,
                content: Layer { kind: LayerKind::Hdr, visual: v_content, swap: None, target: None, size: (0, 0) },
                ui: Layer { kind: LayerKind::Sdr, visual: v_ui, swap: None, target: None, size: (0, 0) },
            })
        }
    }

    pub fn commit(&self, gfx: &Gfx) {
        unsafe {
            let _ = gfx.dcomp.Commit();
        }
    }
}

#[allow(dead_code)]
fn _size(_: D2D_SIZE_U) {}

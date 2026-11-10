//! Direct3D 11 objects for the image pipeline: shaders, states, textures, draw.

use gt_image::Pixels;
use std::cell::RefCell;
use std::rc::Rc;
use windows::core::Result;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

static VS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/image_vs.cso"));
static PS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/image_ps.cso"));

/// Mirrors `cbuffer CB` in image.hlsl.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Cb {
    pub dst: [f32; 4],
    pub uvx: [f32; 4],
    pub uvy: [f32; 4],
    pub m0: [f32; 4],
    pub m1: [f32; 4],
    pub m2: [f32; 4],
    pub tparams: [f32; 4],
    pub scales: [f32; 4],
    pub gm_min: [f32; 4],
    pub gm_max: [f32; 4],
    pub gm_gamma: [f32; 4],
    pub gm_off_base: [f32; 4],
    pub gm_off_alt: [f32; 4],
    pub gm_params: [f32; 4],
    pub misc: [f32; 4],
}

pub struct Texture {
    pub tex: ID3D11Texture2D,
    pub srv: ID3D11ShaderResourceView,
    pub w: u32,
    pub h: u32,
    pub mips: bool,
}

pub struct Pipeline {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    linear: ID3D11SamplerState,
    point: ID3D11SamplerState,
    clamp: ID3D11SamplerState,
    blend: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    white: Texture,
}

thread_local! {
    static PIPE: RefCell<Option<Rc<Pipeline>>> = const { RefCell::new(None) };
}

fn sampler(dev: &ID3D11Device, filter: D3D11_FILTER) -> Result<ID3D11SamplerState> {
    let d = D3D11_SAMPLER_DESC {
        Filter: filter,
        AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
        MipLODBias: 0.0,
        MaxAnisotropy: 1,
        ComparisonFunc: D3D11_COMPARISON_NEVER,
        BorderColor: [0.0; 4],
        MinLOD: 0.0,
        MaxLOD: f32::MAX,
    };
    let mut s = None;
    unsafe { dev.CreateSamplerState(&d, Some(&mut s))? };
    s.ok_or_else(windows::core::Error::empty)
}

/// Shared pipeline for the UI thread's device (created once).
pub fn pipeline(dev: &ID3D11Device) -> Result<Rc<Pipeline>> {
    if let Some(p) = PIPE.with(|c| c.borrow().clone()) {
        return Ok(p);
    }
    unsafe {
        let mut vs = None;
        dev.CreateVertexShader(VS, None, Some(&mut vs))?;
        let mut ps = None;
        dev.CreatePixelShader(PS, None, Some(&mut ps))?;
        let bd = D3D11_BUFFER_DESC { ByteWidth: std::mem::size_of::<Cb>() as u32, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, ..Default::default() };
        let mut cb = None;
        dev.CreateBuffer(&bd, None, Some(&mut cb))?;
        let mut blend_desc = D3D11_BLEND_DESC::default();
        blend_desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: true.into(),
            SrcBlend: D3D11_BLEND_ONE,
            DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
            BlendOp: D3D11_BLEND_OP_ADD,
            SrcBlendAlpha: D3D11_BLEND_ONE,
            DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
            BlendOpAlpha: D3D11_BLEND_OP_ADD,
            RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
        };
        let mut blend = None;
        dev.CreateBlendState(&blend_desc, Some(&mut blend))?;
        let rd = D3D11_RASTERIZER_DESC { FillMode: D3D11_FILL_SOLID, CullMode: D3D11_CULL_NONE, ScissorEnable: true.into(), DepthClipEnable: true.into(), ..Default::default() };
        let mut raster = None;
        dev.CreateRasterizerState(&rd, Some(&mut raster))?;
        let white = upload(dev, None, &Pixels::Rgba8(vec![255; 4]), 1, 1, false, false)?;
        let p = Rc::new(Pipeline {
            vs: vs.unwrap(),
            ps: ps.unwrap(),
            cb: cb.unwrap(),
            linear: sampler(dev, D3D11_FILTER_MIN_MAG_MIP_LINEAR)?,
            point: sampler(dev, D3D11_FILTER_MIN_LINEAR_MAG_MIP_POINT)?,
            clamp: sampler(dev, D3D11_FILTER_MIN_MAG_MIP_LINEAR)?,
            blend: blend.unwrap(),
            raster: raster.unwrap(),
            white,
        });
        PIPE.with(|c| *c.borrow_mut() = Some(p.clone()));
        Ok(p)
    }
}

fn format_of(p: &Pixels, srgb: bool) -> DXGI_FORMAT {
    match p {
        Pixels::Rgba8(_) if srgb => DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
        Pixels::Rgba8(_) => DXGI_FORMAT_R8G8B8A8_UNORM,
        Pixels::Rgba16(_) => DXGI_FORMAT_R16G16B16A16_UNORM,
        Pixels::RgbaF16(_) => DXGI_FORMAT_R16G16B16A16_FLOAT,
    }
}

/// Upload pixels; with `mips` the full chain is generated on the GPU (needs `ctx`).
pub fn upload(dev: &ID3D11Device, ctx: Option<&ID3D11DeviceContext>, p: &Pixels, w: u32, h: u32, srgb: bool, mips: bool) -> Result<Texture> {
    let mips = mips && ctx.is_some() && (w > 1 || h > 1);
    let format = format_of(p, srgb);
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: if mips { 0 } else { 1 },
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | if mips { D3D11_BIND_RENDER_TARGET.0 } else { 0 }) as u32,
        CPUAccessFlags: 0,
        MiscFlags: if mips { D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32 } else { 0 },
    };
    let pitch = w * p.bytes_per_pixel() as u32;
    unsafe {
        let mut tex = None;
        if mips {
            dev.CreateTexture2D(&desc, None, Some(&mut tex))?;
        } else {
            let init = D3D11_SUBRESOURCE_DATA { pSysMem: p.as_bytes().as_ptr() as _, SysMemPitch: pitch, SysMemSlicePitch: 0 };
            dev.CreateTexture2D(&desc, Some(&init), Some(&mut tex))?;
        }
        let tex = tex.ok_or_else(windows::core::Error::empty)?;
        let mut srv = None;
        dev.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
        let srv = srv.ok_or_else(windows::core::Error::empty)?;
        if let (true, Some(ctx)) = (mips, ctx) {
            ctx.UpdateSubresource(&tex, 0, None, p.as_bytes().as_ptr() as _, pitch, 0);
            ctx.GenerateMips(&srv);
        }
        Ok(Texture { tex, srv, w, h, mips })
    }
}

/// Replace the top level of an existing texture (animation frames).
pub fn update(ctx: &ID3D11DeviceContext, t: &Texture, p: &Pixels) {
    unsafe {
        ctx.UpdateSubresource(&t.tex, 0, None, p.as_bytes().as_ptr() as _, t.w * p.bytes_per_pixel() as u32, 0);
        if t.mips {
            ctx.GenerateMips(&t.srv);
        }
    }
}

/// 1D transfer lookup table as an N×1 float texture.
pub fn lut_texture(dev: &ID3D11Device, lut: &[[f32; 4]]) -> Result<Texture> {
    let n = lut.len() as u32;
    let desc = D3D11_TEXTURE2D_DESC {
        Width: n,
        Height: 1,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_IMMUTABLE,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let init = D3D11_SUBRESOURCE_DATA { pSysMem: lut.as_ptr() as _, SysMemPitch: n * 16, SysMemSlicePitch: 0 };
    unsafe {
        let mut tex = None;
        dev.CreateTexture2D(&desc, Some(&init), Some(&mut tex))?;
        let tex = tex.ok_or_else(windows::core::Error::empty)?;
        let mut srv = None;
        dev.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
        Ok(Texture { tex, srv: srv.ok_or_else(windows::core::Error::empty)?, w: n, h: 1, mips: false })
    }
}

pub struct DrawTarget<'a> {
    pub rtv: &'a ID3D11RenderTargetView,
    pub width: u32,
    pub height: u32,
    pub scissor: RECT,
}

impl Pipeline {
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&self, ctx: &ID3D11DeviceContext, t: &DrawTarget, cb: &Cb, src: &Texture, gm: Option<&Texture>, lut: Option<&Texture>, nearest: bool) {
        unsafe {
            ctx.UpdateSubresource(&self.cb, 0, None, cb as *const Cb as _, 0, 0);
            ctx.OMSetRenderTargets(Some(&[Some(t.rtv.clone())]), None);
            ctx.OMSetBlendState(&self.blend, None, 0xFFFF_FFFF);
            ctx.RSSetState(&self.raster);
            let vp = D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: t.width as f32, Height: t.height as f32, MinDepth: 0.0, MaxDepth: 1.0 };
            ctx.RSSetViewports(Some(&[vp]));
            ctx.RSSetScissorRects(Some(&[t.scissor]));
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            ctx.IASetInputLayout(None);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            let srvs = [Some(src.srv.clone()), Some(gm.unwrap_or(&self.white).srv.clone()), Some(lut.unwrap_or(&self.white).srv.clone())];
            ctx.PSSetShaderResources(0, Some(&srvs));
            let s0 = if nearest { self.point.clone() } else { self.linear.clone() };
            ctx.PSSetSamplers(0, Some(&[Some(s0), Some(self.clamp.clone())]));
            ctx.Draw(4, 0);
            ctx.PSSetShaderResources(0, Some(&[None, None, None]));
        }
    }
}

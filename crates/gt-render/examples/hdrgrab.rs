//! Measure what the screen actually shows: capture the composed desktop in FP16
//! (scRGB, 1.0 = 80 nits) with DXGI desktop duplication and report the luminance
//! of the HDR test patches inside the video window.
//!
//! Usage: hdrgrab [--class Gregarioustypes.VideoHost] [--video 1920x1080] [--nits 1,10,...]

use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_R16G16B16A16_FLOAT};
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetWindowRect, IsWindowVisible};

fn f16(h: u16) -> f32 {
    let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = ((h >> 10) & 0x1F) as i32;
    let m = (h & 0x3FF) as f32;
    s * match e {
        0 => m * 2f32.powi(-24),
        31 => f32::INFINITY,
        _ => (1.0 + m / 1024.0) * 2f32.powi(e - 15),
    }
}

fn find_visible(class: &str) -> Option<HWND> {
    let wide: Vec<u16> = class.encode_utf16().chain(Some(0)).collect();
    let mut after: Option<HWND> = None;
    // Search top-level windows, then children of the preview window.
    let viewer: Vec<u16> = "Gregarioustypes.Viewer".encode_utf16().chain(Some(0)).collect();
    loop {
        let top = unsafe { FindWindowExW(None, after, PCWSTR(viewer.as_ptr()), PCWSTR::null()) }.ok()?;
        if unsafe { IsWindowVisible(top) }.as_bool() {
            if class == "Gregarioustypes.Viewer" {
                return Some(top);
            }
            if let Ok(c) = unsafe { FindWindowExW(Some(top), None, PCWSTR(wide.as_ptr()), PCWSTR::null()) } {
                if unsafe { IsWindowVisible(c) }.as_bool() {
                    return Some(c);
                }
            }
        }
        after = Some(top);
    }
}

fn main() -> windows::core::Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let class = arg("--class").unwrap_or_else(|| "Gregarioustypes.VideoHost".into());
    let (vw, vh) = arg("--video").and_then(|v| v.split_once('x').map(|(a, b)| (a.parse::<f32>().unwrap_or(1920.0), b.parse::<f32>().unwrap_or(1080.0)))).unwrap_or((1920.0, 1080.0));
    let nits: Vec<f32> = arg("--nits").map(|s| s.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or_else(|| vec![1.0, 10.0, 50.0, 100.0, 203.0, 400.0, 600.0, 1000.0, 1500.0, 2000.0, 4000.0, 10000.0]);
    let Some(hwnd) = find_visible(&class) else {
        eprintln!("no visible {class} window");
        std::process::exit(2);
    };
    let mut wr = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut wr)? };
    println!("window {class}: {}x{} at {},{}", wr.right - wr.left, wr.bottom - wr.top, wr.left, wr.top);

    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        // Find the output containing the window.
        let mut i = 0;
        let (adapter, output) = loop {
            let a = factory.EnumAdapters1(i)?;
            let mut j = 0;
            let mut hit = None;
            while let Ok(o) = a.EnumOutputs(j) {
                let d = o.GetDesc()?;
                let r = d.DesktopCoordinates;
                if wr.left >= r.left && wr.left < r.right && wr.top >= r.top && wr.top < r.bottom {
                    hit = Some(o);
                    break;
                }
                j += 1;
            }
            if let Some(o) = hit {
                break (a, o);
            }
            i += 1;
        };
        let desc = output.GetDesc()?;
        let origin = (desc.DesktopCoordinates.left, desc.DesktopCoordinates.top);
        let mut device = None;
        let mut ctx = None;
        D3D11CreateDevice(&adapter, D3D_DRIVER_TYPE_UNKNOWN, Default::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut ctx))?;
        let device = device.unwrap();
        let ctx = ctx.unwrap();
        let out5: IDXGIOutput5 = output.cast()?;
        let formats: [DXGI_FORMAT; 1] = [DXGI_FORMAT_R16G16B16A16_FLOAT];
        let dup = out5.DuplicateOutput1(&device, 0, &formats)?;
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut staging: Option<ID3D11Texture2D> = None;
        let mut td = D3D11_TEXTURE2D_DESC::default();
        // Copy while the frame is held: after ReleaseFrame its contents are gone.
        // The first frame can be stale; take one that was presented.
        for attempt in 0..8 {
            let mut res = None;
            if dup.AcquireNextFrame(500, &mut info, &mut res).is_err() {
                continue;
            }
            if let Some(tex) = res.and_then(|r| r.cast::<ID3D11Texture2D>().ok()) {
                if staging.is_none() {
                    tex.GetDesc(&mut td);
                    td.Usage = D3D11_USAGE_STAGING;
                    td.BindFlags = 0;
                    td.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                    td.MiscFlags = 0;
                    let mut t = None;
                    device.CreateTexture2D(&td, None, Some(&mut t))?;
                    staging = t;
                }
                ctx.CopyResource(staging.as_ref().unwrap(), &tex);
            }
            let _ = dup.ReleaseFrame();
            if info.LastPresentTime != 0 && staging.is_some() && attempt > 0 {
                break;
            }
        }
        let staging = staging.expect("no frame captured");
        let mut map = D3D11_MAPPED_SUBRESOURCE::default();
        ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut map))?;
        let pitch = map.RowPitch as usize;
        let base = map.pData as *const u8;
        let px = |x: i32, y: i32| -> f32 {
            let (x, y) = ((x - origin.0).clamp(0, td.Width as i32 - 1) as usize, (y - origin.1).clamp(0, td.Height as i32 - 1) as usize);
            let p = base.add(y * pitch + x * 8) as *const u16;
            let (r, g, b) = (f16(*p), f16(*p.add(1)), f16(*p.add(2)));
            r.max(g).max(b)
        };
        let avg = |cx: i32, cy: i32, half: i32| -> f32 {
            let mut s = 0.0;
            let mut n = 0.0;
            for y in (cy - half)..=(cy + half) {
                for x in (cx - half)..=(cx + half) {
                    s += px(x, y);
                    n += 1.0;
                }
            }
            s / n
        };
        // Video fitted inside the window.
        let (ww, wh) = ((wr.right - wr.left) as f32, (wr.bottom - wr.top) as f32);
        let k = (ww / vw).min(wh / vh);
        let (ox, oy) = (wr.left as f32 + (ww - vw * k) / 2.0, wr.top as f32 + (wh - vh * k) / 2.0);
        let patch = 200.0;
        let gap = ((vw - 6.0 * patch) / 7.0).floor();
        println!("{:>8} {:>10} {:>8}", "content", "shown", "scRGB");
        for (n, &target) in nits.iter().enumerate() {
            let (row, col) = (n / 6, n % 6);
            let cx = gap + col as f32 * (patch + gap) + patch / 2.0;
            let cy = if row == 0 { 140.0 } else { 440.0 } + patch / 2.0;
            let (sx, sy) = ((ox + cx * k).round() as i32, (oy + cy * k).round() as i32);
            let half = ((patch * k * 0.25) as i32).max(1);
            let v = avg(sx, sy, half);
            println!("{:>8.0} {:>10.1} {:>8.3}", target, v * 80.0, v);
        }
        // Overall peak inside the window.
        let mut peak = 0f32;
        for y in (wr.top..wr.bottom).step_by(2) {
            for x in (wr.left..wr.right).step_by(2) {
                peak = peak.max(px(x, y));
            }
        }
        println!("window peak: {:.1} nits", peak * 80.0);
        ctx.Unmap(&staging, 0);
    }
    Ok(())
}

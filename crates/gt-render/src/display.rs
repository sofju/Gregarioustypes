//! Per-monitor HDR state: whether Windows HDR is on, the SDR white level and the
//! panel's peak luminance.

use windows::core::Interface;
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig, DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput6};
use windows::Win32::Graphics::Gdi::HMONITOR;

#[derive(Clone, Debug)]
pub struct DisplayInfo {
    pub device: String,
    /// Windows HDR ("Use HDR") is on for this monitor.
    pub hdr: bool,
    /// Brightness of SDR white in nits (scRGB 1.0 = 80 nits).
    pub sdr_white_nits: f32,
    /// Peak luminance reported by the driver/EDID, in nits.
    pub max_nits: f32,
    pub max_full_frame_nits: f32,
    pub min_nits: f32,
    pub bits_per_color: u32,
}

impl Default for DisplayInfo {
    fn default() -> Self {
        Self { device: String::new(), hdr: false, sdr_white_nits: 80.0, max_nits: 80.0, max_full_frame_nits: 80.0, min_nits: 0.0, bits_per_color: 8 }
    }
}

impl DisplayInfo {
    /// Multiplier that maps SDR content (1.0 = SDR white) to scRGB.
    pub fn sdr_scale(&self) -> f32 {
        if self.hdr {
            self.sdr_white_nits / 80.0
        } else {
            1.0
        }
    }

    /// Headroom above SDR white (1.0 when HDR is off).
    pub fn headroom(&self, peak_override: Option<f32>) -> f32 {
        if !self.hdr {
            return 1.0;
        }
        let peak = peak_override.unwrap_or(self.max_nits).max(self.sdr_white_nits);
        peak / self.sdr_white_nits
    }
}

fn wstr(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// SDR white level (nits) for a GDI device name like `\\.\DISPLAY1`.
fn sdr_white_for(device: &str) -> Option<f32> {
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm).is_err() {
            return None;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None).is_err() {
            return None;
        }
        for p in &paths[..np as usize] {
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: p.sourceInfo.adapterId,
                    id: p.sourceInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 || wstr(&src.viewGdiDeviceName) != device {
                continue;
            }
            let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                    size: std::mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32,
                    adapterId: p.targetInfo.adapterId,
                    id: p.targetInfo.id,
                },
                SDRWhiteLevel: 0,
            };
            let rc = DisplayConfigGetDeviceInfo(&mut white.header);
            tracing::debug!("SDR white level for {device}: rc={rc} raw={}", white.SDRWhiteLevel);
            if rc == 0 && white.SDRWhiteLevel > 0 {
                return Some(white.SDRWhiteLevel as f32 / 1000.0 * 80.0);
            }
        }
        None
    }
}

/// Drivers sometimes report nonsense peaks (22,000 nits has been seen on a laptop
/// panel). A value outside 80–10,000 nits (PQ's ceiling) is ignored in favour of
/// the full-frame value when that one is plausible, else a typical HDR panel's
/// 1000 nits. The peak override in the settings always wins over both.
fn plausible_peak(peak: f32, full_frame: f32) -> f32 {
    let ok = |v: f32| v.is_finite() && (80.0..=10_000.0).contains(&v);
    if ok(peak) {
        peak
    } else if ok(full_frame) {
        full_frame
    } else {
        1000.0
    }
}

/// Query the HDR state of `monitor`. Uses a fresh DXGI factory so toggling Windows
/// HDR is picked up immediately.
pub fn query(monitor: HMONITOR) -> DisplayInfo {
    let mut info = DisplayInfo::default();
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return info };
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                o += 1;
                let Ok(o6) = output.cast::<IDXGIOutput6>() else { continue };
                let Ok(d) = o6.GetDesc1() else { continue };
                if d.Monitor != monitor {
                    continue;
                }
                info.device = wstr(&d.DeviceName);
                info.hdr = d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
                info.max_nits = plausible_peak(d.MaxLuminance, d.MaxFullFrameLuminance);
                if info.max_nits != d.MaxLuminance {
                    tracing::warn!("display {}: DXGI peak {:.0} nits is implausible, using {:.0}", info.device, d.MaxLuminance, info.max_nits);
                }
                info.max_full_frame_nits = d.MaxFullFrameLuminance;
                info.min_nits = d.MinLuminance;
                info.bits_per_color = d.BitsPerColor;
                if info.hdr {
                    info.sdr_white_nits = sdr_white_for(&info.device).unwrap_or(80.0);
                }
                return info;
            }
            a += 1;
        }
    }
    info
}

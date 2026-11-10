//! Bindings to the native shim (native/gt_codecs.h).

use std::ffi::{c_char, c_int, c_void};

pub const GT_RGBA8: i32 = 0;
pub const GT_RGBA16: i32 = 1;
pub const GT_RGBAF16: i32 = 2;
pub const GT_RGBAF32: i32 = 3;
pub const GT_CMYK8: i32 = 4;
pub const GT_GM_ISO: i32 = 1;
pub const GT_GM_APPLE: i32 = 2;

#[repr(C)]
pub struct GtImg {
    pub width: i32,
    pub height: i32,
    pub format: i32,
    pub stride: i32,
    pub pixels: *mut u8,
    pub icc: *mut u8,
    pub icc_len: i32,
    pub cicp_primaries: i32,
    pub cicp_transfer: i32,
    pub cicp_matrix: i32,
    pub cicp_full: i32,
    pub has_prim: i32,
    pub prim: [f32; 8],
    pub gamma: f32,
    pub has_alpha: i32,
    pub bit_depth: i32,
    pub orientation: i32,
    pub max_cll: f32,
    pub max_fall: f32,
    pub mastering_max: f32,
    pub exif: *mut u8,
    pub exif_len: i32,
    pub xmp: *mut u8,
    pub xmp_len: i32,
    pub gainmap: *mut GtImg,
    pub gm_kind: i32,
    pub gm_min: [f32; 3],
    pub gm_max: [f32; 3],
    pub gm_gamma: [f32; 3],
    pub gm_base_offset: [f32; 3],
    pub gm_alt_offset: [f32; 3],
    pub gm_base_headroom: f32,
    pub gm_alt_headroom: f32,
    pub gm_use_base_cs: i32,
    pub frames: i32,
    pub delay_ms: i32,
    pub full_width: i32,
    pub full_height: i32,
    pub intensity_target: f32,
    pub error: [c_char; 256],
}

unsafe extern "C" {
    pub fn gt_free(img: *mut GtImg);
    pub fn gt_free_buf(p: *mut c_void);
    pub fn gt_avif_decode(data: *const u8, len: usize, out: *mut GtImg) -> c_int;
    pub fn gt_heif_decode(data: *const u8, len: usize, out: *mut GtImg) -> c_int;
    pub fn gt_jxl_decode(data: *const u8, len: usize, out: *mut GtImg) -> c_int;
    pub fn gt_webp_decode(data: *const u8, len: usize, out: *mut GtImg) -> c_int;
    pub fn gt_webp_anim_open(data: *const u8, len: usize, w: *mut c_int, h: *mut c_int, frames: *mut c_int, loops: *mut c_int) -> *mut c_void;
    pub fn gt_webp_anim_next(h: *mut c_void, rgba_out: *mut u8, ts: *mut c_int) -> c_int;
    pub fn gt_webp_anim_close(h: *mut c_void);
    pub fn gt_jpeg_header(data: *const u8, len: usize, w: *mut c_int, h: *mut c_int, cmyk: *mut c_int) -> c_int;
    pub fn gt_jpeg_decode(data: *const u8, len: usize, scale_denom: c_int, out: *mut GtImg) -> c_int;
    pub fn gt_tiff_decode(path: *const u16, out: *mut GtImg) -> c_int;
    pub fn gt_j2k_decode(data: *const u8, len: usize, max_w: c_int, max_h: c_int, out: *mut GtImg) -> c_int;
    pub fn gt_raw_thumb(data: *const u8, len: usize, bitmap: *mut GtImg, jpeg: *mut *mut u8, jpeg_len: *mut usize, raw_w: *mut c_int, raw_h: *mut c_int, flip: *mut c_int) -> c_int;
    pub fn gt_raw_process(data: *const u8, len: usize, half_size: c_int, out: *mut GtImg) -> c_int;
    pub fn gt_exr_decode(utf8_path: *const c_char, out: *mut GtImg) -> c_int;
}

/// Owned native image, freed on drop.
pub struct Native(pub Box<GtImg>);

impl Native {
    pub fn zeroed() -> Self {
        // SAFETY: GtImg is plain data; all-zero is a valid empty value.
        Native(Box::new(unsafe { std::mem::zeroed() }))
    }
    pub fn ptr(&mut self) -> *mut GtImg {
        &mut *self.0
    }
    pub fn error(&self) -> String {
        let b: Vec<u8> = self.0.error.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
        String::from_utf8_lossy(&b).into_owned()
    }
    pub fn bytes(p: *const u8, n: i32) -> Option<Vec<u8>> {
        (!p.is_null() && n > 0).then(|| unsafe { std::slice::from_raw_parts(p, n as usize) }.to_vec())
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        unsafe { gt_free(self.ptr()) };
    }
}

// ---------------------------------------------------------------- lcms2 (from vcpkg)

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CieXyz {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CieXyY {
    pub x: f64,
    pub y: f64,
    pub yy: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CieXyYTriple {
    pub r: CieXyY,
    pub g: CieXyY,
    pub b: CieXyY,
}

unsafe extern "C" {
    pub fn cmsOpenProfileFromMem(mem: *const c_void, size: u32) -> *mut c_void;
    pub fn cmsCloseProfile(p: *mut c_void) -> c_int;
    pub fn cmsGetColorSpace(p: *mut c_void) -> u32;
    pub fn cmsIsMatrixShaper(p: *mut c_void) -> c_int;
    pub fn cmsReadTag(p: *mut c_void, sig: u32) -> *mut c_void;
    pub fn cmsEvalToneCurveFloat(curve: *const c_void, v: f32) -> f32;
    pub fn cmsGetProfileInfoASCII(p: *mut c_void, info: u32, lang: *const c_char, country: *const c_char, buf: *mut c_char, size: u32) -> u32;
    pub fn cmsCreateRGBProfile(white: *const CieXyY, primaries: *const CieXyYTriple, curves: *const *mut c_void) -> *mut c_void;
    pub fn cmsBuildGamma(ctx: *mut c_void, gamma: f64) -> *mut c_void;
    pub fn cmsFreeToneCurve(c: *mut c_void);
    pub fn cmsCreateTransform(input: *mut c_void, in_fmt: u32, output: *mut c_void, out_fmt: u32, intent: u32, flags: u32) -> *mut c_void;
    pub fn cmsDoTransform(t: *mut c_void, input: *const c_void, output: *mut c_void, n: u32);
    pub fn cmsDeleteTransform(t: *mut c_void);
}

pub const SIG_RGB: u32 = 0x5247_4220;
pub const SIG_GRAY: u32 = 0x4752_4159;
pub const SIG_CMYK: u32 = 0x434D_594B;
pub const TAG_RXYZ: u32 = 0x7258_595A;
pub const TAG_GXYZ: u32 = 0x6758_595A;
pub const TAG_BXYZ: u32 = 0x6258_595A;
pub const TAG_RTRC: u32 = 0x7254_5243;
pub const TAG_GTRC: u32 = 0x6754_5243;
pub const TAG_BTRC: u32 = 0x6254_5243;
pub const TAG_KTRC: u32 = 0x6B54_5243;
pub const TYPE_RGBA_8: u32 = 0x40099;
pub const TYPE_RGBA_16: u32 = 0x4009A;
pub const TYPE_CMYK_8: u32 = 0x60021;
pub const TYPE_CMYK_8_REV: u32 = 0x62021;
pub const TYPE_RGBA_FLT: u32 = 0x44009C;
pub const FLAGS_NOCACHE: u32 = 0x40;
pub const FLAGS_COPY_ALPHA: u32 = 0x0400_0000;

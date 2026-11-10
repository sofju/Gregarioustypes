//! Image decoding into GPU-ready pixels plus a colour description. Decoders keep the
//! source encoding (8/16-bit or half float, any transfer/primaries); the renderer
//! linearises and maps to the display in a shader.

pub mod codecs;
pub mod color;
pub mod exif;
pub mod ffi;
pub mod gainmap;
pub mod util;

pub use codecs::{decode, decode_bytes, detect, DecodeOptions, Kind};
pub use color::{Color, Lum, Transfer};

/// Interleaved RGBA pixels, straight (non-premultiplied) alpha.
#[derive(Clone)]
pub enum Pixels {
    Rgba8(Vec<u8>),
    Rgba16(Vec<u16>),
    /// IEEE half floats as raw bits.
    RgbaF16(Vec<u16>),
}

impl Pixels {
    pub fn bytes_per_pixel(&self) -> usize {
        match self {
            Pixels::Rgba8(_) => 4,
            _ => 8,
        }
    }
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Pixels::Rgba8(v) => v,
            Pixels::Rgba16(v) | Pixels::RgbaF16(v) => unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2) },
        }
    }
    pub fn len_bytes(&self) -> usize {
        self.as_bytes().len()
    }
}

#[derive(Clone, Debug)]
pub enum GainMapKind {
    /// ISO 21496-1 / Ultra HDR. min/max/headrooms are log2; offsets linear.
    Iso { min: [f32; 3], max: [f32; 3], gamma: [f32; 3], base_offset: [f32; 3], alt_offset: [f32; 3], base_headroom: f32, alt_headroom: f32 },
    /// Apple HEIC: hdr = sdr × (1 + (headroom − 1) × gain).
    Apple { headroom: f32 },
}

#[derive(Clone)]
pub struct GainMap {
    pub width: u32,
    pub height: u32,
    pub pixels: Pixels,
    pub kind: GainMapKind,
    /// Encoding of the gain-map samples (Apple uses an sRGB-like curve).
    pub transfer: Transfer,
    pub label: &'static str,
}

impl GainMap {
    /// Largest linear boost (×) this map can produce.
    pub fn max_boost(&self) -> f32 {
        match &self.kind {
            GainMapKind::Iso { max, .. } => 2f32.powf(max.iter().cloned().fold(0.0, f32::max).max(0.0)),
            GainMapKind::Apple { headroom } => *headroom,
        }
    }
}

pub struct Frame {
    pub pixels: Pixels,
    pub delay_ms: u32,
}

pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Pixels,
    pub color: Color,
    pub alpha: bool,
    /// EXIF orientation 1–8 still to be applied at render time.
    pub orientation: u8,
    pub bit_depth: u32,
    pub max_cll: f32,
    pub mastering_max: f32,
    pub gain_map: Option<GainMap>,
    /// Frames after the first (animation), with the first frame's delay.
    pub frames: Vec<Frame>,
    pub first_delay_ms: u32,
    pub frame_count: u32,
    /// Full-resolution size when this decode is reduced.
    pub full_size: Option<(u32, u32)>,
    pub format: &'static str,
    pub facts: Vec<(String, String)>,
    /// Source pixels are floating point scene values (EXR, Radiance, float TIFF...).
    pub hdr_float: bool,
}

impl Image {
    pub fn new(width: u32, height: u32, pixels: Pixels, format: &'static str) -> Self {
        Self {
            width,
            height,
            pixels,
            color: Color::srgb(),
            alpha: false,
            orientation: 1,
            bit_depth: 8,
            max_cll: 0.0,
            mastering_max: 0.0,
            gain_map: None,
            frames: Vec::new(),
            first_delay_ms: 0,
            frame_count: 1,
            full_size: None,
            format,
            facts: Vec::new(),
            hdr_float: false,
        }
    }

    /// Width and height after orientation is applied.
    pub fn display_size(&self) -> (u32, u32) {
        if self.orientation >= 5 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }

    /// Short HDR summary for the title bar / info panel.
    pub fn hdr_label(&self) -> Option<String> {
        if let Some(g) = &self.gain_map {
            return Some(format!("{} gain map", g.label));
        }
        match self.color.lum {
            Lum::Pq => Some("HDR (PQ)".into()),
            Lum::Hlg => Some("HDR (HLG)".into()),
            Lum::ScRgb => Some("HDR (scRGB)".into()),
            Lum::Relative if self.hdr_float => Some("HDR (linear float)".into()),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct DecodeError(pub String);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub type Result<T> = std::result::Result<T, DecodeError>;

pub fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(DecodeError(m.into()))
}

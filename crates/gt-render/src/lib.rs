//! GPU plumbing: D3D11 + D2D + DirectWrite + DirectComposition devices, the
//! two-layer window surface (FP16 scRGB content below a topmost BGRA8 UI layer),
//! and display HDR information.

pub mod display;
pub mod gfx;
pub mod surface;

pub use display::DisplayInfo;
pub use gfx::Gfx;
pub use surface::{Layer, LayerKind, Surface};

pub use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F, D2D_SIZE_F};
pub use windows_numerics::{Matrix3x2, Vector2};

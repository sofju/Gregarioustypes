//! Raster images with colour management and HDR: decoding via `gt-image`, rendering on
//! the FP16 scRGB layer with the image shader.

pub mod gpu;
pub mod provider;
pub mod view;

pub fn register(r: &mut gt_ui::Registry) {
    r.add(provider::ImageProvider);
}

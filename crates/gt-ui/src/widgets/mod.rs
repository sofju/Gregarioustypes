pub mod card;
pub mod doclist;
pub mod hex;
pub mod text;

use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{ID2D1Bitmap1, ID2D1DeviceContext, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

/// Upload premultiplied BGRA pixels as a D2D bitmap.
pub fn bitmap_from_bgra(dc: &ID2D1DeviceContext, width: u32, height: u32, data: &[u8]) -> Option<ID2D1Bitmap1> {
    if width == 0 || height == 0 || data.len() < (width * height * 4) as usize {
        return None;
    }
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
        colorContext: std::mem::ManuallyDrop::new(None),
    };
    unsafe { dc.CreateBitmap(D2D_SIZE_U { width, height }, Some(data.as_ptr() as _), width * 4, &props).ok() }
}

/// Re-upload pixels into an existing bitmap of the same size.
pub fn update_bitmap(bmp: &ID2D1Bitmap1, width: u32, data: &[u8]) -> bool {
    unsafe { bmp.CopyFromMemory(None, data.as_ptr() as _, width * 4).is_ok() }
}

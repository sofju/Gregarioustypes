//! GIF with frame compositing (disposal methods) for animation.

use super::DecodeOptions;
use crate::{Frame, Image, Pixels, Result};

pub fn decode(d: &[u8], opts: &DecodeOptions) -> Result<Image> {
    let mut o = gif::DecodeOptions::new();
    o.set_color_output(gif::ColorOutput::RGBA);
    let mut dec = o.read_info(std::io::Cursor::new(d)).map_err(|e| crate::DecodeError(format!("gif: {e}")))?;
    let (w, h) = (dec.width() as usize, dec.height() as usize);
    let mut canvas = vec![0u8; w * h * 4];
    let mut frames: Vec<(Vec<u8>, u32)> = Vec::new();
    let mut used = 0usize;
    let mut count = 0u32;
    let mut pending: Option<(gif::DisposalMethod, usize, usize, usize, usize, Vec<u8>)> = None;
    while let Some(f) = dec.read_next_frame().map_err(|e| crate::DecodeError(format!("gif: {e}")))? {
        count += 1;
        if let Some((disp, l, t, fw, fh, prev)) = pending.take() {
            match disp {
                gif::DisposalMethod::Background => {
                    for y in t..(t + fh).min(h) {
                        let s = (y * w + l) * 4;
                        let e = (y * w + (l + fw).min(w)) * 4;
                        canvas[s..e].fill(0);
                    }
                }
                gif::DisposalMethod::Previous => canvas = prev,
                _ => {}
            }
        }
        let (l, t, fw, fh) = (f.left as usize, f.top as usize, f.width as usize, f.height as usize);
        let prev = if f.dispose == gif::DisposalMethod::Previous { canvas.clone() } else { Vec::new() };
        for y in 0..fh {
            for x in 0..fw {
                let (cx, cy) = (l + x, t + y);
                if cx >= w || cy >= h {
                    continue;
                }
                let s = &f.buffer[(y * fw + x) * 4..(y * fw + x) * 4 + 4];
                if s[3] != 0 {
                    canvas[(cy * w + cx) * 4..(cy * w + cx) * 4 + 4].copy_from_slice(s);
                }
            }
        }
        let delay = (f.delay as u32 * 10).max(20);
        used += canvas.len();
        if used <= opts.anim_budget || frames.is_empty() {
            frames.push((canvas.clone(), delay));
        } else if count > 1 {
            // Over budget: stop collecting but keep counting.
        }
        pending = Some((f.dispose, l, t, fw, fh, prev));
    }
    if frames.is_empty() {
        return Err(crate::DecodeError("gif: no frames".into()));
    }
    let mut it = frames.into_iter();
    let (first, first_delay) = it.next().unwrap();
    let mut img = Image::new(w as u32, h as u32, Pixels::Rgba8(first), "GIF");
    img.alpha = true;
    img.first_delay_ms = first_delay;
    img.frame_count = count;
    img.frames = it.map(|(p, d)| Frame { pixels: Pixels::Rgba8(p), delay_ms: d }).collect();
    Ok(img)
}

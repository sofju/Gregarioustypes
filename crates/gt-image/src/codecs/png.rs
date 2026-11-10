//! PNG / APNG via the `png` crate, with cICP, iCCP, sRGB, gAMA/cHRM and eXIf.

use super::{apply_color, apply_exif, DecodeOptions};
use crate::{color, err, Frame, Image, Pixels, Result};

fn to_rgba(buf: &[u8], ct: png::ColorType, depth: png::BitDepth, w: usize, h: usize) -> Pixels {
    let ch = match ct {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        _ => 4,
    };
    let n = w * h;
    if depth == png::BitDepth::Sixteen {
        let mut out = vec![0u16; n * 4];
        for i in 0..n {
            let s = |c: usize| u16::from_be_bytes([buf[(i * ch + c) * 2], buf[(i * ch + c) * 2 + 1]]);
            let (r, g, b, a) = match ch {
                1 => (s(0), s(0), s(0), 65535),
                2 => (s(0), s(0), s(0), s(1)),
                3 => (s(0), s(1), s(2), 65535),
                _ => (s(0), s(1), s(2), s(3)),
            };
            out[i * 4..i * 4 + 4].copy_from_slice(&[r, g, b, a]);
        }
        Pixels::Rgba16(out)
    } else {
        let mut out = vec![0u8; n * 4];
        for i in 0..n {
            let s = |c: usize| buf[i * ch + c];
            let (r, g, b, a) = match ch {
                1 => (s(0), s(0), s(0), 255),
                2 => (s(0), s(0), s(0), s(1)),
                3 => (s(0), s(1), s(2), 255),
                _ => (s(0), s(1), s(2), s(3)),
            };
            out[i * 4..i * 4 + 4].copy_from_slice(&[r, g, b, a]);
        }
        Pixels::Rgba8(out)
    }
}

pub fn decode(d: &[u8], opts: &DecodeOptions) -> Result<Image> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(d));
    dec.set_transformations(png::Transformations::EXPAND);
    let mut reader = dec.read_info().map_err(|e| crate::DecodeError(format!("png: {e}")))?;
    let (w, h) = (reader.info().width as usize, reader.info().height as usize);
    let info = reader.info().clone();
    let size = reader.output_buffer_size().ok_or_else(|| crate::DecodeError("png: image too large".into()))?;
    let mut buf = vec![0u8; size];
    let out = reader.next_frame(&mut buf).map_err(|e| crate::DecodeError(format!("png: {e}")))?;
    let (ct, depth) = (out.color_type, out.bit_depth);
    let animated = info.animation_control.map(|a| a.num_frames > 1).unwrap_or(false);
    let first_fc = reader.info().frame_control;
    let mut img = Image::new(w as u32, h as u32, Pixels::Rgba8(Vec::new()), if animated { "APNG" } else { "PNG" });
    img.bit_depth = if depth == png::BitDepth::Sixteen { 16 } else { 8 };
    img.alpha = matches!(info.color_type, png::ColorType::Rgba | png::ColorType::GrayscaleAlpha) || info.trns.is_some();

    if animated && first_fc.map(|f| f.width as usize == w && f.height as usize == h).unwrap_or(false) && depth != png::BitDepth::Sixteen {
        // Composite APNG frames on an RGBA canvas.
        let ac = info.animation_control.unwrap();
        img.frame_count = ac.num_frames;
        let mut canvas = match to_rgba(&buf[..out.buffer_size()], ct, depth, w, h) {
            Pixels::Rgba8(v) => v,
            _ => return err("png: unexpected depth"),
        };
        let delay = |fc: &png::FrameControl| (1000 * fc.delay_num as u32 / if fc.delay_den == 0 { 100 } else { fc.delay_den as u32 }).max(20);
        img.first_delay_ms = first_fc.map(|f| delay(&f)).unwrap_or(100);
        img.pixels = Pixels::Rgba8(canvas.clone());
        let mut used = canvas.len();
        let mut last_fc = first_fc;
        let mut prev: Option<Vec<u8>> = None;
        for _ in 1..ac.num_frames {
            // Dispose of the previous frame.
            if let (Some(fc), Some(p)) = (last_fc, prev.take()) {
                match fc.dispose_op {
                    png::DisposeOp::Background => {
                        for y in fc.y_offset as usize..(fc.y_offset + fc.height) as usize {
                            let s = (y * w + fc.x_offset as usize) * 4;
                            canvas[s..s + fc.width as usize * 4].fill(0);
                        }
                    }
                    png::DisposeOp::Previous => canvas = p,
                    _ => {}
                }
            }
            let Ok(fo) = reader.next_frame(&mut buf) else { break };
            let Some(fc) = reader.info().frame_control else { break };
            prev = Some(canvas.clone());
            let fw = fc.width as usize;
            let fh = fc.height as usize;
            let Pixels::Rgba8(fp) = to_rgba(&buf[..fo.buffer_size()], fo.color_type, fo.bit_depth, fw, fh) else { break };
            for y in 0..fh {
                for x in 0..fw {
                    let (cx, cy) = (fc.x_offset as usize + x, fc.y_offset as usize + y);
                    if cx >= w || cy >= h {
                        continue;
                    }
                    let s = &fp[(y * fw + x) * 4..(y * fw + x) * 4 + 4];
                    let d = &mut canvas[(cy * w + cx) * 4..(cy * w + cx) * 4 + 4];
                    if fc.blend_op == png::BlendOp::Source || s[3] == 255 {
                        d.copy_from_slice(s);
                    } else if s[3] > 0 {
                        let a = s[3] as f32 / 255.0;
                        let da = d[3] as f32 / 255.0;
                        let oa = a + da * (1.0 - a);
                        for c in 0..3 {
                            d[c] = ((s[c] as f32 * a + d[c] as f32 * da * (1.0 - a)) / oa.max(1e-6)) as u8;
                        }
                        d[3] = (oa * 255.0) as u8;
                    }
                }
            }
            used += canvas.len();
            if used > opts.anim_budget {
                break;
            }
            img.frames.push(Frame { pixels: Pixels::Rgba8(canvas.clone()), delay_ms: delay(&fc) });
            last_fc = Some(fc);
        }
    } else {
        img.pixels = to_rgba(&buf[..out.buffer_size()], ct, depth, w, h);
    }

    // Colour: cICP > iCCP > sRGB > gAMA/cHRM.
    if let Some(c) = info.coding_independent_code_points {
        apply_color(&mut img, None, Some((c.color_primaries as i32, c.transfer_function as i32)), None, 0.0);
    } else if let Some(icc) = info.icc_profile.as_deref() {
        apply_color(&mut img, Some(icc), None, None, 0.0);
    } else if info.srgb.is_none() {
        if let Some(g) = info.source_gamma {
            let gamma = g.into_value();
            if gamma > 0.0 && (gamma - 1.0 / 2.2).abs() > 0.01 {
                let prim = info.source_chromaticities.map(|c| {
                    let v = |p: png::ScaledFloat| p.into_value();
                    [v(c.red.0), v(c.red.1), v(c.green.0), v(c.green.1), v(c.blue.0), v(c.blue.1), v(c.white.0), v(c.white.1)]
                });
                let mut col = color::from_cicp(1, 4, 1.0 / gamma, prim);
                col.name = format!("gamma {:.2}", 1.0 / gamma);
                img.color = col;
            }
        }
    }
    apply_exif(&mut img, info.exif_metadata.as_deref(), true);
    Ok(img)
}

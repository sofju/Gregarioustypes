//! Minimal EXIF (TIFF IFD) reader: orientation, camera facts, and the Apple MakerNote
//! values that give the HDR headroom of iPhone gain-map photos.

#[derive(Clone, Debug, Default)]
pub struct Exif {
    pub orientation: Option<u8>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    pub date: Option<String>,
    pub exposure: Option<f64>,
    pub fnumber: Option<f64>,
    pub iso: Option<u32>,
    pub focal: Option<f64>,
    pub focal35: Option<u32>,
    /// Apple MakerNote tags 33 and 48 (HDR headroom inputs).
    pub apple_33: Option<f64>,
    pub apple_48: Option<f64>,
}

struct Tiff<'a> {
    d: &'a [u8],
    le: bool,
}

impl<'a> Tiff<'a> {
    fn u16(&self, o: usize) -> Option<u16> {
        let b = self.d.get(o..o + 2)?;
        Some(if self.le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
    }
    fn u32(&self, o: usize) -> Option<u32> {
        let b = self.d.get(o..o + 4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Some(if self.le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
    }

    /// Entries of the IFD at `off`: (tag, type, count, value offset).
    fn ifd(&self, off: usize) -> Vec<(u16, u16, u32, usize)> {
        let mut v = Vec::new();
        let Some(n) = self.u16(off) else { return v };
        for i in 0..n.min(512) as usize {
            let e = off + 2 + i * 12;
            let (Some(tag), Some(ty), Some(count)) = (self.u16(e), self.u16(e + 2), self.u32(e + 4)) else { break };
            let size = match ty {
                1 | 2 | 6 | 7 => 1,
                3 | 8 => 2,
                4 | 9 | 11 => 4,
                5 | 10 | 12 => 8,
                _ => 1,
            } * count as usize;
            let vo = if size <= 4 { e + 8 } else { self.u32(e + 8).unwrap_or(0) as usize };
            v.push((tag, ty, count, vo));
        }
        v
    }

    fn num(&self, ty: u16, o: usize) -> Option<f64> {
        Some(match ty {
            1 | 7 => *self.d.get(o)? as f64,
            3 => self.u16(o)? as f64,
            4 => self.u32(o)? as f64,
            8 => self.u16(o)? as i16 as f64,
            9 => self.u32(o)? as i32 as f64,
            5 => {
                let (n, d) = (self.u32(o)?, self.u32(o + 4)?);
                if d == 0 { return None } else { n as f64 / d as f64 }
            }
            10 => {
                let (n, d) = (self.u32(o)? as i32, self.u32(o + 4)? as i32);
                if d == 0 { return None } else { n as f64 / d as f64 }
            }
            11 => f32::from_bits(self.u32(o)?) as f64,
            12 => {
                let b = self.d.get(o..o + 8)?;
                let a: [u8; 8] = b.try_into().ok()?;
                if self.le { f64::from_le_bytes(a) } else { f64::from_be_bytes(a) }
            }
            _ => return None,
        })
    }

    fn ascii(&self, o: usize, count: u32) -> Option<String> {
        let b = self.d.get(o..o + count as usize)?;
        let s = String::from_utf8_lossy(b.split(|&c| c == 0).next().unwrap_or(b)).trim().to_string();
        (!s.is_empty()).then_some(s)
    }
}

/// Parse EXIF from a TIFF header, an "Exif\0\0" prefix, or HEIF's 4-byte-offset form.
pub fn parse(data: &[u8]) -> Option<Exif> {
    let mut d = data;
    if d.starts_with(b"Exif\0\0") {
        d = &d[6..];
    } else if d.len() > 4 && !(d.starts_with(b"II") || d.starts_with(b"MM")) {
        let off = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) as usize;
        d = d.get(4 + off..)?;
        if d.starts_with(b"Exif\0\0") {
            d = &d[6..];
        }
    }
    let le = match d.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let t = Tiff { d, le };
    let mut x = Exif::default();
    let ifd0 = t.u32(4)? as usize;
    let mut exif_ifd = None;
    for (tag, ty, count, vo) in t.ifd(ifd0) {
        match tag {
            0x0112 => x.orientation = t.num(ty, vo).map(|v| v as u8).filter(|v| (1..=8).contains(v)),
            0x010F => x.make = t.ascii(vo, count),
            0x0110 => x.model = t.ascii(vo, count),
            0x0132 => x.date = x.date.take().or_else(|| t.ascii(vo, count)),
            0x8769 => exif_ifd = t.u32(vo).map(|v| v as usize),
            _ => {}
        }
    }
    if let Some(e) = exif_ifd {
        for (tag, ty, count, vo) in t.ifd(e) {
            match tag {
                0x829A => x.exposure = t.num(ty, vo),
                0x829D => x.fnumber = t.num(ty, vo),
                0x8827 => x.iso = t.num(ty, vo).map(|v| v as u32),
                0x9003 => x.date = t.ascii(vo, count).or(x.date.take()),
                0x920A => x.focal = t.num(ty, vo),
                0xA405 => x.focal35 = t.num(ty, vo).map(|v| v as u32),
                0xA434 => x.lens = t.ascii(vo, count),
                0x927C => apple_maker_note(d, vo, count as usize, &mut x),
                _ => {}
            }
        }
    }
    Some(x)
}

fn apple_maker_note(d: &[u8], off: usize, len: usize, x: &mut Exif) {
    let Some(mn) = d.get(off..off + len) else { return };
    if !mn.starts_with(b"Apple iOS\0") || mn.len() < 16 {
        return;
    }
    let le = &mn[12..14] == b"II";
    let t = Tiff { d: mn, le };
    for (tag, ty, _count, vo) in t.ifd(14) {
        match tag {
            0x0021 => x.apple_33 = t.num(ty, vo),
            0x0030 => x.apple_48 = t.num(ty, vo),
            _ => {}
        }
    }
}

impl Exif {
    pub fn facts(&self) -> Vec<(String, String)> {
        let mut v = Vec::new();
        let camera = match (&self.make, &self.model) {
            (Some(m), Some(md)) if md.starts_with(m.as_str()) => Some(md.clone()),
            (Some(m), Some(md)) => Some(format!("{m} {md}")),
            (None, Some(md)) => Some(md.clone()),
            (Some(m), None) => Some(m.clone()),
            _ => None,
        };
        if let Some(c) = camera {
            v.push(("Camera".into(), c));
        }
        if let Some(l) = &self.lens {
            v.push(("Lens".into(), l.clone()));
        }
        let mut shot = Vec::new();
        if let Some(f) = self.focal {
            shot.push(match self.focal35 {
                Some(e) if e > 0 => format!("{f:.0} mm ({e} mm eq.)"),
                _ => format!("{f:.0} mm"),
            });
        }
        if let Some(n) = self.fnumber {
            shot.push(format!("ƒ/{n:.1}"));
        }
        if let Some(e) = self.exposure {
            shot.push(if e > 0.0 && e < 1.0 { format!("1/{:.0} s", 1.0 / e) } else { format!("{e:.1} s") });
        }
        if let Some(i) = self.iso {
            shot.push(format!("ISO {i}"));
        }
        if !shot.is_empty() {
            v.push(("Exposure".into(), shot.join(" · ")));
        }
        if let Some(d) = &self.date {
            v.push(("Taken".into(), d.replacen(':', "-", 2)));
        }
        v
    }
}

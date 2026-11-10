//! Gain-map metadata: Adobe/Google `hdrgm` XMP (Ultra HDR), ISO 21496-1 binary, and
//! Apple's MakerNote-derived headroom.

use crate::GainMapKind;

/// Values of an `hdrgm:<name>` property: attribute form or rdf:Seq element form.
fn hdrgm_values(xmp: &str, name: &str) -> Option<Vec<f32>> {
    let attr = format!("hdrgm:{name}=\"");
    if let Some(i) = xmp.find(&attr) {
        let rest = &xmp[i + attr.len()..];
        let end = rest.find('"')?;
        return rest[..end].trim().parse::<f32>().ok().map(|v| vec![v]);
    }
    let open = format!("<hdrgm:{name}>");
    let i = xmp.find(&open)?;
    let rest = &xmp[i + open.len()..];
    let end = rest.find(&format!("</hdrgm:{name}>"))?;
    let body = &rest[..end];
    if !body.contains("<rdf:li") {
        return body.trim().parse::<f32>().ok().map(|v| vec![v]);
    }
    let vals: Vec<f32> = body.split("<rdf:li").skip(1).filter_map(|s| s.split('>').nth(1)?.split('<').next()?.trim().parse::<f32>().ok()).collect();
    (!vals.is_empty()).then_some(vals)
}

fn three(v: Option<Vec<f32>>, default: f32) -> [f32; 3] {
    match v {
        Some(v) if v.len() >= 3 => [v[0], v[1], v[2]],
        Some(v) if !v.is_empty() => [v[0]; 3],
        _ => [default; 3],
    }
}

/// Ultra HDR / Adobe gain map XMP (`http://ns.adobe.com/hdr-gain-map/1.0/`).
pub fn parse_hdrgm_xmp(xmp: &str) -> Option<GainMapKind> {
    if !xmp.contains("hdrgm:") || hdrgm_values(xmp, "GainMapMax").is_none() {
        return None;
    }
    let one = |n: &str, d: f32| hdrgm_values(xmp, n).and_then(|v| v.first().copied()).unwrap_or(d);
    let base_is_hdr = xmp.contains("hdrgm:BaseRenditionIsHDR=\"True\"");
    let (cap_min, cap_max) = (one("HDRCapacityMin", 0.0), one("HDRCapacityMax", 1.0));
    Some(GainMapKind::Iso {
        min: three(hdrgm_values(xmp, "GainMapMin"), 0.0),
        max: three(hdrgm_values(xmp, "GainMapMax"), 1.0),
        gamma: three(hdrgm_values(xmp, "Gamma"), 1.0),
        base_offset: three(hdrgm_values(xmp, "OffsetSDR"), 1.0 / 64.0),
        alt_offset: three(hdrgm_values(xmp, "OffsetHDR"), 1.0 / 64.0),
        base_headroom: if base_is_hdr { cap_max } else { cap_min },
        alt_headroom: if base_is_hdr { cap_min } else { cap_max },
    })
}

/// ISO 21496-1 binary metadata (big-endian), as written by libultrahdr and Apple.
pub fn parse_iso21496(d: &[u8]) -> Option<GainMapKind> {
    let mut p = 0usize;
    let u8_ = |p: &mut usize| -> Option<u8> {
        let v = *d.get(*p)?;
        *p += 1;
        Some(v)
    };
    let u16_ = |p: &mut usize| -> Option<u16> {
        let b = d.get(*p..*p + 2)?;
        *p += 2;
        Some(u16::from_be_bytes([b[0], b[1]]))
    };
    let u32_ = |p: &mut usize| -> Option<u32> {
        let b = d.get(*p..*p + 4)?;
        *p += 4;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let min_version = u16_(&mut p)?;
    let _writer_version = u16_(&mut p)?;
    if min_version != 0 {
        return None;
    }
    let flags = u8_(&mut p)?;
    let channels = if flags & 0x80 != 0 { 3 } else { 1 };
    let common = flags & 0x08 != 0;
    let frac = |n: f64, dd: f64| if dd == 0.0 { 0.0 } else { (n / dd) as f32 };
    let (base_hr, alt_hr);
    let mut min = [0f32; 3];
    let mut max = [0f32; 3];
    let mut gamma = [1f32; 3];
    let mut bo = [0f32; 3];
    let mut ao = [0f32; 3];
    if common {
        let den = u32_(&mut p)? as f64;
        base_hr = frac(u32_(&mut p)? as f64, den);
        alt_hr = frac(u32_(&mut p)? as f64, den);
        for c in 0..channels {
            min[c] = frac(u32_(&mut p)? as i32 as f64, den);
            max[c] = frac(u32_(&mut p)? as i32 as f64, den);
            gamma[c] = frac(u32_(&mut p)? as f64, den);
            bo[c] = frac(u32_(&mut p)? as i32 as f64, den);
            ao[c] = frac(u32_(&mut p)? as i32 as f64, den);
        }
    } else {
        let (n, dd) = (u32_(&mut p)? as f64, u32_(&mut p)? as f64);
        base_hr = frac(n, dd);
        let (n, dd) = (u32_(&mut p)? as f64, u32_(&mut p)? as f64);
        alt_hr = frac(n, dd);
        for c in 0..channels {
            let (n, dd) = (u32_(&mut p)? as i32 as f64, u32_(&mut p)? as f64);
            min[c] = frac(n, dd);
            let (n, dd) = (u32_(&mut p)? as i32 as f64, u32_(&mut p)? as f64);
            max[c] = frac(n, dd);
            let (n, dd) = (u32_(&mut p)? as f64, u32_(&mut p)? as f64);
            gamma[c] = frac(n, dd);
            let (n, dd) = (u32_(&mut p)? as i32 as f64, u32_(&mut p)? as f64);
            bo[c] = frac(n, dd);
            let (n, dd) = (u32_(&mut p)? as i32 as f64, u32_(&mut p)? as f64);
            ao[c] = frac(n, dd);
        }
    }
    if channels == 1 {
        for a in [&mut min, &mut max, &mut gamma, &mut bo, &mut ao] {
            a[1] = a[0];
            a[2] = a[0];
        }
    }
    if gamma.iter().any(|&g| g <= 0.0) {
        return None;
    }
    Some(GainMapKind::Iso { min, max, gamma, base_offset: bo, alt_offset: ao, base_headroom: base_hr, alt_headroom: alt_hr })
}

/// Apple's documented headroom formula from MakerNote tags 33 and 48.
pub fn apple_headroom(m33: f64, m48: f64) -> f32 {
    let stops = if m33 < 1.0 {
        if m48 <= 0.01 { -20.0 * m48 + 1.8 } else { -0.101 * m48 + 1.601 }
    } else if m48 <= 0.01 {
        -70.0 * m48 + 3.0
    } else {
        -0.303 * m48 + 2.303
    };
    2f64.powf(stops.max(0.0)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xmp_attributes() {
        let x = r#"<rdf:Description hdrgm:Version="1.0" hdrgm:GainMapMin="0" hdrgm:GainMapMax="2.5" hdrgm:Gamma="1" hdrgm:OffsetSDR="0.015625" hdrgm:OffsetHDR="0.015625" hdrgm:HDRCapacityMin="0" hdrgm:HDRCapacityMax="2.5"/>"#;
        match parse_hdrgm_xmp(x).unwrap() {
            GainMapKind::Iso { max, alt_headroom, base_headroom, .. } => {
                assert_eq!(max, [2.5; 3]);
                assert_eq!(alt_headroom, 2.5);
                assert_eq!(base_headroom, 0.0);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn xmp_seq() {
        let x = "<hdrgm:GainMapMax><rdf:Seq><rdf:li>1.0</rdf:li><rdf:li>2.0</rdf:li><rdf:li>3.0</rdf:li></rdf:Seq></hdrgm:GainMapMax>";
        match parse_hdrgm_xmp(x).unwrap() {
            GainMapKind::Iso { max, .. } => assert_eq!(max, [1.0, 2.0, 3.0]),
            _ => panic!(),
        }
    }

    #[test]
    fn iso_common_denominator() {
        let mut d = vec![0, 0, 0, 0, 0x08];
        for v in [1000u32, 0, 2000] {
            d.extend_from_slice(&v.to_be_bytes());
        }
        for v in [0i32, 2000, 1000, 15, 15] {
            d.extend_from_slice(&v.to_be_bytes());
        }
        match parse_iso21496(&d).unwrap() {
            GainMapKind::Iso { max, gamma, alt_headroom, .. } => {
                assert_eq!(max, [2.0; 3]);
                assert_eq!(gamma, [1.0; 3]);
                assert_eq!(alt_headroom, 2.0);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn apple() {
        assert!((apple_headroom(1.2, 0.005) - 2f32.powf(2.65)).abs() < 0.01);
    }
}

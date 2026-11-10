//! Media that can't simply be played: CD track stubs (.cda), DRM-protected files
//! (FairPlay .m4p/.m4v, Audible .aa, TiVo, protected Windows Media) and DVD
//! navigation files (.ifo, played through the parent VIDEO_TS).

use std::path::Path;

/// A `.cda` file: Windows' stub for an audio CD track.
#[derive(Clone, Debug)]
pub struct CdaTrack {
    pub track: u16,
    pub serial: u32,
    pub start_sector: u32,
    pub sectors: u32,
}

impl CdaTrack {
    pub fn seconds(&self) -> f64 {
        self.sectors as f64 / 75.0
    }
    pub fn start_seconds(&self) -> f64 {
        self.start_sector as f64 / 75.0
    }
}

pub fn parse_cda(b: &[u8]) -> Option<CdaTrack> {
    if b.len() < 44 || &b[0..4] != b"RIFF" || &b[8..12] != b"CDDA" || &b[12..16] != b"fmt " {
        return None;
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    Some(CdaTrack { track: u16_at(22), serial: u32_at(24), start_sector: u32_at(28), sectors: u32_at(32) })
}

pub fn is_cd_drive(path: &Path) -> bool {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;
    let Some(root) = path.components().next() else { return false };
    let root = format!("{}\\", root.as_os_str().to_string_lossy());
    // DRIVE_CDROM = 5
    unsafe { GetDriveTypeW(&HSTRING::from(root)) == 5 }
}

/// Why a file can't be played, when it is DRM-protected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Drm {
    FairPlay,
    CommonEncryption,
    Audible,
    TiVo,
    WindowsMedia,
}

impl Drm {
    pub fn message(&self) -> &'static str {
        match self {
            Drm::FairPlay => "Protected by Apple FairPlay DRM: only an authorised Apple app can play it.",
            Drm::CommonEncryption => "Encrypted (Common Encryption): it needs the licence from the store it came from.",
            Drm::Audible => "Audible audiobook protected by DRM: play it in the Audible app.",
            Drm::TiVo => "TiVo recording encrypted with the box's Media Access Key.",
            Drm::WindowsMedia => "Protected by Windows Media DRM: it needs its licence to play.",
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Drm::FairPlay => "FairPlay",
            Drm::CommonEncryption => "Common Encryption (CENC)",
            Drm::Audible => "Audible",
            Drm::TiVo => "TiVo",
            Drm::WindowsMedia => "Windows Media DRM",
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

const ASF_HEADER: [u8; 16] = [0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C];
const ASF_CONTENT_ENCRYPTION: [u8; 16] = [0xFB, 0xB3, 0x11, 0x22, 0x23, 0xBD, 0xD2, 0x11, 0xB4, 0xB7, 0x00, 0xA0, 0xC9, 0x55, 0xFC, 0x6E];
const ASF_EXT_CONTENT_ENCRYPTION: [u8; 16] = [0x14, 0xE6, 0x8A, 0x29, 0x22, 0x26, 0x17, 0x4C, 0xB9, 0x35, 0xDA, 0xE0, 0x7E, 0xE9, 0x28, 0x9C];
const ASF_PLAYREADY: [u8; 16] = [0x79, 0xF0, 0x04, 0x9A, 0x40, 0x98, 0x86, 0x42, 0xAB, 0x92, 0xE6, 0x5B, 0xE0, 0x88, 0x5F, 0x95];

/// Detect DRM from the head (and, for MP4 with the index at the end, the tail).
pub fn detect_drm(path: &Path, head: &[u8]) -> Option<Drm> {
    if head.len() >= 8 && u32::from_be_bytes([head[4], head[5], head[6], head[7]]) == 0x5790_7536 {
        return Some(Drm::Audible);
    }
    if head.starts_with(b"TiVo") {
        return Some(Drm::TiVo);
    }
    if head.starts_with(&ASF_HEADER) {
        if find(head, &ASF_CONTENT_ENCRYPTION) || find(head, &ASF_EXT_CONTENT_ENCRYPTION) || find(head, &ASF_PLAYREADY) {
            return Some(Drm::WindowsMedia);
        }
        return None;
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        let check = |b: &[u8]| {
            if !find(b, b"stsd") {
                return None;
            }
            if find(b, b"drms") || find(b, b"drmi") || find(b, b"p608") {
                Some(Drm::FairPlay)
            } else if find(b, b"encv") || find(b, b"enca") {
                Some(Drm::CommonEncryption)
            } else {
                None
            }
        };
        if let Some(d) = check(head) {
            return Some(d);
        }
        if !find(head, b"moov") {
            if let Ok(tail) = gt_core::sniff::read_tail(path, 4 << 20) {
                return check(&tail);
            }
        }
    }
    None
}

/// Title/author/narrator... from an Audible .aa dictionary block.
pub fn audible_tags(path: &Path) -> Vec<(String, String)> {
    let Ok(b) = std::fs::read(path) else { return Vec::new() };
    let be = |i: usize| b.get(i..i + 4).map(|x| u32::from_be_bytes([x[0], x[1], x[2], x[3]]) as usize);
    let mut out = Vec::new();
    let Some(toc) = be(8) else { return out };
    for k in 0..toc.min(64) {
        let base = 16 + k * 12;
        let (Some(kind), Some(off), Some(size)) = (be(base), be(base + 4), be(base + 8)) else { break };
        if kind != 2 || off + size > b.len() {
            continue;
        }
        let Some(count) = be(off) else { continue };
        let mut i = off + 4;
        for _ in 0..count.min(256) {
            let (Some(kl), Some(vl)) = (be(i + 1), be(i + 5)) else { break };
            let k0 = i + 9;
            let (Some(key), Some(val)) = (b.get(k0..k0 + kl), b.get(k0 + kl..k0 + kl + vl)) else { break };
            out.push((String::from_utf8_lossy(key).into_owned(), String::from_utf8_lossy(val).into_owned()));
            i = k0 + kl + vl;
        }
    }
    out
}

/// For a DVD `.ifo`: the folder to hand mpv as `dvd-device`.
pub fn dvd_device(ifo: &Path) -> Option<String> {
    let dir = ifo.parent()?;
    Some(dir.display().to_string())
}

pub fn is_dvd_ifo(head: &[u8]) -> bool {
    head.starts_with(b"DVDVIDEO-VMG") || head.starts_with(b"DVDVIDEO-VTS")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cda() {
        let mut b = b"RIFF\x24\0\0\0CDDAfmt \x18\0\0\0".to_vec();
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&3u16.to_le_bytes());
        b.extend_from_slice(&0xABCDu32.to_le_bytes());
        b.extend_from_slice(&32150u32.to_le_bytes());
        b.extend_from_slice(&(75u32 * 214).to_le_bytes());
        b.extend_from_slice(&[0; 8]);
        let t = parse_cda(&b).unwrap();
        assert_eq!(t.track, 3);
        assert_eq!(t.seconds(), 214.0);
    }

    #[test]
    fn drm_heads() {
        let p = Path::new("x");
        let mut aa = vec![0, 0, 0, 80];
        aa.extend_from_slice(&0x5790_7536u32.to_be_bytes());
        assert_eq!(detect_drm(p, &aa), Some(Drm::Audible));
        assert_eq!(detect_drm(p, b"TiVo\0\x04"), Some(Drm::TiVo));
        let mut mp4 = b"\0\0\0\x18ftypM4P ".to_vec();
        mp4.extend_from_slice(b"....moov....stsd....drms");
        assert_eq!(detect_drm(p, &mp4), Some(Drm::FairPlay));
    }
}

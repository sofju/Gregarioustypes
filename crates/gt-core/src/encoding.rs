//! Text encodings: CP437 (DOS), Windows-1252, UTF-8/16 detection.

use serde::{Deserialize, Serialize};

/// CP437 code point table as displayed by IBM PC hardware: control bytes are glyphs.
pub const CP437: [char; 256] = [
    '\u{0000}', '\u{263A}', '\u{263B}', '\u{2665}', '\u{2666}', '\u{2663}', '\u{2660}', '\u{2022}',
    '\u{25D8}', '\u{25CB}', '\u{25D9}', '\u{2642}', '\u{2640}', '\u{266A}', '\u{266B}', '\u{263C}',
    '\u{25BA}', '\u{25C4}', '\u{2195}', '\u{203C}', '\u{00B6}', '\u{00A7}', '\u{25AC}', '\u{21A8}',
    '\u{2191}', '\u{2193}', '\u{2192}', '\u{2190}', '\u{221F}', '\u{2194}', '\u{25B2}', '\u{25BC}',
    ' ', '!', '"', '#', '$', '%', '&', '\'', '(', ')', '*', '+', ',', '-', '.', '/',
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', ':', ';', '<', '=', '>', '?',
    '@', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O',
    'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '[', '\\', ']', '^', '_',
    '`', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o',
    'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', '{', '|', '}', '~', '\u{2302}',
    '\u{00C7}', '\u{00FC}', '\u{00E9}', '\u{00E2}', '\u{00E4}', '\u{00E0}', '\u{00E5}', '\u{00E7}',
    '\u{00EA}', '\u{00EB}', '\u{00E8}', '\u{00EF}', '\u{00EE}', '\u{00EC}', '\u{00C4}', '\u{00C5}',
    '\u{00C9}', '\u{00E6}', '\u{00C6}', '\u{00F4}', '\u{00F6}', '\u{00F2}', '\u{00FB}', '\u{00F9}',
    '\u{00FF}', '\u{00D6}', '\u{00DC}', '\u{00A2}', '\u{00A3}', '\u{00A5}', '\u{20A7}', '\u{0192}',
    '\u{00E1}', '\u{00ED}', '\u{00F3}', '\u{00FA}', '\u{00F1}', '\u{00D1}', '\u{00AA}', '\u{00BA}',
    '\u{00BF}', '\u{2310}', '\u{00AC}', '\u{00BD}', '\u{00BC}', '\u{00A1}', '\u{00AB}', '\u{00BB}',
    '\u{2591}', '\u{2592}', '\u{2593}', '\u{2502}', '\u{2524}', '\u{2561}', '\u{2562}', '\u{2556}',
    '\u{2555}', '\u{2563}', '\u{2551}', '\u{2557}', '\u{255D}', '\u{255C}', '\u{255B}', '\u{2510}',
    '\u{2514}', '\u{2534}', '\u{252C}', '\u{251C}', '\u{2500}', '\u{253C}', '\u{255E}', '\u{255F}',
    '\u{255A}', '\u{2554}', '\u{2569}', '\u{2566}', '\u{2560}', '\u{2550}', '\u{256C}', '\u{2567}',
    '\u{2568}', '\u{2564}', '\u{2565}', '\u{2559}', '\u{2558}', '\u{2552}', '\u{2553}', '\u{256B}',
    '\u{256A}', '\u{2518}', '\u{250C}', '\u{2588}', '\u{2584}', '\u{258C}', '\u{2590}', '\u{2580}',
    '\u{03B1}', '\u{00DF}', '\u{0393}', '\u{03C0}', '\u{03A3}', '\u{03C3}', '\u{00B5}', '\u{03C4}',
    '\u{03A6}', '\u{0398}', '\u{03A9}', '\u{03B4}', '\u{221E}', '\u{03C6}', '\u{03B5}', '\u{2229}',
    '\u{2261}', '\u{00B1}', '\u{2265}', '\u{2264}', '\u{2320}', '\u{2321}', '\u{00F7}', '\u{2248}',
    '\u{00B0}', '\u{2219}', '\u{00B7}', '\u{221A}', '\u{207F}', '\u{00B2}', '\u{25A0}', '\u{00A0}',
];

/// Reverse lookup Unicode -> CP437 byte (for rendering UTF-8 NFOs with the VGA font).
/// ASCII (controls included) maps to itself so CR/LF/TAB/ESC keep their meaning.
pub fn unicode_to_cp437(c: char) -> Option<u8> {
    if (c as u32) < 0x80 {
        return Some(c as u8);
    }
    CP437.iter().skip(0x80).position(|&x| x == c).map(|p| (p + 0x80) as u8).or_else(|| CP437.iter().take(0x20).position(|&x| x == c).map(|p| p as u8))
}

const W1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

pub fn decode_1252(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9F => W1252_HIGH[(b - 0x80) as usize],
            _ => b as char,
        })
        .collect()
}

/// Decode bytes in a Windows code page (932, 1251...) with the OS tables.
pub fn decode_codepage(bytes: &[u8], cp: u32) -> String {
    use windows::Win32::Globalization::{MultiByteToWideChar, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};
    match cp {
        1252 | 0 => return decode_1252(bytes),
        65001 => return String::from_utf8_lossy(bytes).into_owned(),
        _ => {}
    }
    if bytes.is_empty() {
        return String::new();
    }
    unsafe {
        let n = MultiByteToWideChar(cp, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None);
        if n <= 0 {
            return decode_1252(bytes);
        }
        let mut w = vec![0u16; n as usize];
        MultiByteToWideChar(cp, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, Some(&mut w));
        String::from_utf16_lossy(&w)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Cp437,
    Utf8,
    Utf16Le,
    Utf16Be,
    Windows1252,
}

impl Encoding {
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Cp437 => "CP437 (DOS)",
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Windows1252 => "Windows-1252",
        }
    }

    pub const ALL: [Encoding; 5] = [Encoding::Cp437, Encoding::Utf8, Encoding::Windows1252, Encoding::Utf16Le, Encoding::Utf16Be];
}

pub fn bom(bytes: &[u8]) -> Option<(Encoding, usize)> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Some((Encoding::Utf8, 3))
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        Some((Encoding::Utf16Le, 2))
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Some((Encoding::Utf16Be, 2))
    } else {
        None
    }
}

/// Is a decoded non-ASCII char plausible in a UTF-8 text/NFO? Used to reject
/// CP437 byte pairs that happen to form valid UTF-8 (e.g. `DB B2` = U+06F2).
fn plausible(c: char) -> bool {
    matches!(c as u32,
        0x00A0..=0x024F   // Latin-1 supplement, Latin extended A/B
        | 0x0370..=0x03FF // Greek
        | 0x0400..=0x04FF // Cyrillic
        | 0x2000..=0x206F // general punctuation
        | 0x2070..=0x20CF // super/subscripts, currency
        | 0x2100..=0x23FF // letterlike, arrows, math, technical
        | 0x2500..=0x25FF // box drawing, blocks, geometric shapes
        | 0x2600..=0x27BF // symbols, dingbats
        | 0x3000..=0x30FF // CJK punctuation, kana
        | 0x4E00..=0x9FFF // CJK ideographs
        | 0xAC00..=0xD7AF // Hangul
        | 0xFF00..=0xFFEF // full-width forms
        | 0x1F300..=0x1FAFF // emoji
    )
}

/// Decide the encoding of an NFO/DIZ/ASC file. CP437 unless there is a BOM or the
/// bytes are convincingly UTF-8 text.
pub fn detect_dos_text(bytes: &[u8]) -> Encoding {
    if let Some((e, _)) = bom(bytes) {
        return e;
    }
    if !bytes.iter().any(|&b| b >= 0x80) {
        return Encoding::Cp437;
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        let mut multi = 0usize;
        let mut good = 0usize;
        for c in s.chars().filter(|c| (*c as u32) >= 0x80) {
            multi += 1;
            if plausible(c) {
                good += 1;
            }
        }
        if multi > 0 && good * 10 >= multi * 9 {
            return Encoding::Utf8;
        }
    }
    Encoding::Cp437
}

/// Decode any supported encoding to a String (lossy).
pub fn decode(bytes: &[u8], enc: Encoding) -> String {
    let bytes = match bom(bytes) {
        Some((e, n)) if e == enc => &bytes[n..],
        _ => bytes,
    };
    match enc {
        Encoding::Cp437 => bytes.iter().map(|&b| CP437[b as usize]).collect(),
        Encoding::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        Encoding::Windows1252 => decode_1252(bytes),
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| if enc == Encoding::Utf16Le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
                .collect();
            String::from_utf16_lossy(&units)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    Text(Encoding),
    Binary,
}

/// Generic text-or-binary detection for the fallback text viewer.
/// `complete` is true when `head` holds the whole file (so a cut UTF-8 sequence at the
/// end is an error rather than a truncation artefact).
pub fn detect_text(head: &[u8], complete: bool) -> TextKind {
    if let Some((e, _)) = bom(head) {
        return TextKind::Text(e);
    }
    if head.is_empty() {
        return TextKind::Text(Encoding::Utf8);
    }
    let sample = &head[..head.len().min(64 * 1024)];
    let zeros = sample.iter().filter(|&&b| b == 0).count();
    if zeros > 0 {
        // UTF-16 without BOM: zeros concentrated on one parity.
        let even = sample.iter().step_by(2).filter(|&&b| b == 0).count();
        let odd = zeros - even;
        let half = sample.len() / 2;
        if half > 8 && odd * 10 > half * 6 && even * 50 < half {
            return TextKind::Text(Encoding::Utf16Le);
        }
        if half > 8 && even * 10 > half * 6 && odd * 50 < half {
            return TextKind::Text(Encoding::Utf16Be);
        }
        return TextKind::Binary;
    }
    let ctrl = sample.iter().filter(|&&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C | 0x1B | 0x1A)).count();
    if ctrl * 100 > sample.len() * 2 {
        return TextKind::Binary;
    }
    // A truncated sample may cut a UTF-8 sequence: validate up to the last char boundary.
    match std::str::from_utf8(sample) {
        Ok(_) => TextKind::Text(Encoding::Utf8),
        Err(e) if e.error_len().is_none() && !(complete && sample.len() == head.len()) => TextKind::Text(Encoding::Utf8),
        Err(_) => TextKind::Text(Encoding::Windows1252),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp437_table_is_bijective() {
        for (i, c) in CP437.iter().enumerate().skip(1) {
            assert_eq!(CP437.iter().position(|x| x == c), Some(i), "duplicate at {i:#x}");
        }
    }

    #[test]
    fn block_art_is_not_utf8() {
        // DB B2 is valid UTF-8 (U+06F2) but is CP437 "█▓".
        let art = b"\xDB\xB2\xDB\xB2 RELEASE \xB2\xDB\r\n";
        assert_eq!(detect_dos_text(art), Encoding::Cp437);
        assert_eq!(decode(art, Encoding::Cp437).chars().next(), Some('█'));
    }

    #[test]
    fn real_utf8_nfo() {
        assert_eq!(detect_dos_text("█▓▒░ Café ░▒▓█".as_bytes()), Encoding::Utf8);
        assert_eq!(detect_dos_text(b"\xEF\xBB\xBFplain"), Encoding::Utf8);
    }

    #[test]
    fn ascii_defaults_to_cp437() {
        assert_eq!(detect_dos_text(b"just ascii"), Encoding::Cp437);
    }

    #[test]
    fn text_vs_binary() {
        assert_eq!(detect_text(b"hello\nworld", true), TextKind::Text(Encoding::Utf8));
        assert_eq!(detect_text(&[0u8, 1, 2, 3, 0xFF, 0, 7, 8, 9], true), TextKind::Binary);
        assert_eq!(detect_text(b"caf\xE9", true), TextKind::Text(Encoding::Windows1252));
        assert_eq!(detect_text(&"café".as_bytes()[..4], false), TextKind::Text(Encoding::Utf8));
        let u16: Vec<u8> = "hello world, utf16".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(detect_text(&u16, true), TextKind::Text(Encoding::Utf16Le));
    }
}

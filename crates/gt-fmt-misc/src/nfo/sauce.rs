//! SAUCE metadata record (Standard Architecture for Universal Comment Extensions),
//! appended to ANSI/ASCII art files.

use gt_core::encoding::CP437;

#[derive(Clone, Debug, Default)]
pub struct Sauce {
    pub title: String,
    pub author: String,
    pub group: String,
    pub date: String,
    pub data_type: u8,
    pub file_type: u8,
    pub tinfo1: u16,
    pub tinfo2: u16,
    pub flags: u8,
    pub font: String,
    pub comments: Vec<String>,
}

impl Sauce {
    pub fn ice_colors(&self) -> bool {
        self.flags & 1 != 0
    }
    /// 8 or 9 when specified, else None.
    pub fn letter_spacing(&self) -> Option<u32> {
        match (self.flags >> 1) & 3 {
            1 => Some(8),
            2 => Some(9),
            _ => None,
        }
    }
    /// Legacy (non-square) pixel aspect requested.
    pub fn stretch(&self) -> bool {
        (self.flags >> 3) & 3 == 1
    }
    pub fn is_character(&self) -> bool {
        self.data_type == 1
    }
    pub fn is_ansi(&self) -> bool {
        self.is_character() && matches!(self.file_type, 1 | 2)
    }
    pub fn type_name(&self) -> &'static str {
        match (self.data_type, self.file_type) {
            (1, 0) => "ASCII",
            (1, 1) => "ANSi",
            (1, 2) => "ANSiMation",
            (1, 3) => "RIP script",
            (1, 4) => "PCBoard",
            (1, 5) => "Avatar",
            (1, 6) => "HTML",
            (1, 7) => "Source",
            (1, 8) => "TundraDraw",
            (5, _) => "BinaryText",
            (6, _) => "XBin",
            _ => "Other",
        }
    }
    /// "1996-07-04" from CCYYMMDD.
    pub fn date_pretty(&self) -> String {
        let d = self.date.trim();
        if d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()) {
            format!("{}-{}-{}", &d[0..4], &d[4..6], &d[6..8])
        } else {
            d.to_string()
        }
    }
}

fn field(b: &[u8]) -> String {
    let s: String = b.iter().map(|&c| if c == 0 { ' ' } else { CP437[c as usize] }).collect();
    s.trim_end().to_string()
}

/// Split `data` into (content, SAUCE). Content stops at the DOS EOF marker (0x1A).
pub fn split(data: &[u8]) -> (&[u8], Option<Sauce>) {
    let mut end = data.len();
    let mut sauce = None;
    if data.len() >= 128 && &data[data.len() - 128..data.len() - 123] == b"SAUCE" {
        let r = &data[data.len() - 128..];
        let le = |o: usize| u16::from_le_bytes([r[o], r[o + 1]]);
        let mut s = Sauce {
            title: field(&r[7..42]),
            author: field(&r[42..62]),
            group: field(&r[62..82]),
            date: field(&r[82..90]),
            data_type: r[94],
            file_type: r[95],
            tinfo1: le(96),
            tinfo2: le(98),
            flags: r[105],
            font: {
                let f = &r[106..128];
                let n = f.iter().position(|&c| c == 0).unwrap_or(f.len());
                String::from_utf8_lossy(&f[..n]).trim().to_string()
            },
            comments: Vec::new(),
        };
        end = data.len() - 128;
        let n = r[104] as usize;
        if n > 0 {
            let block = 5 + n * 64;
            if end >= block && &data[end - block..end - block + 5] == b"COMNT" {
                let c = &data[end - block + 5..end];
                s.comments = c.chunks(64).map(field).collect();
                end -= block;
            }
        }
        sauce = Some(s);
    }
    let content = &data[..end];
    let content = match content.iter().position(|&b| b == 0x1A) {
        Some(p) => &content[..p],
        None => content,
    };
    (content, sauce)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_record() {
        let mut data = b"hello\x1a".to_vec();
        data.extend_from_slice(b"COMNT");
        let mut c = [b' '; 64];
        c[..4].copy_from_slice(b"note");
        data.extend_from_slice(&c);
        let mut r = vec![b' '; 128];
        r[..7].copy_from_slice(b"SAUCE00");
        r[7..12].copy_from_slice(b"Title");
        r[82..90].copy_from_slice(b"19960704");
        r[94] = 1;
        r[95] = 1;
        r[96..98].copy_from_slice(&80u16.to_le_bytes());
        r[104] = 1;
        r[105] = 0b0000_0011; // iCE + 8px (bits 1-2 = 01)
        r[106..113].copy_from_slice(b"IBM VGA");
        r[113..128].fill(0);
        data.extend_from_slice(&r);
        let (content, s) = split(&data);
        let s = s.unwrap();
        assert_eq!(content, b"hello");
        assert_eq!(s.title, "Title");
        assert_eq!(s.date_pretty(), "1996-07-04");
        assert!(s.is_ansi() && s.ice_colors());
        assert_eq!(s.letter_spacing(), Some(8));
        assert_eq!(s.font, "IBM VGA");
        assert_eq!(s.comments, vec!["note".to_string()]);
    }
}

//! Character-cell grid built from CP437 bytes, with an ANSI.SYS interpreter for
//! coloured art.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: u8,
    pub fg: u8,
    pub bg: u8,
}

pub const BLANK: Cell = Cell { ch: b' ', fg: 7, bg: 0 };

#[derive(Clone, Debug, Default)]
pub struct Grid {
    pub cols: usize,
    pub rows: Vec<Vec<Cell>>,
    /// Colours other than the default light grey on black appear.
    pub colored: bool,
}

impl Grid {
    pub fn get(&self, row: usize, col: usize) -> Cell {
        self.rows.get(row).and_then(|r| r.get(col)).copied().unwrap_or(BLANK)
    }

    fn put(&mut self, row: usize, col: usize, c: Cell) {
        while self.rows.len() <= row {
            self.rows.push(Vec::new());
        }
        let r = &mut self.rows[row];
        while r.len() <= col {
            r.push(BLANK);
        }
        r[col] = c;
        if c.fg != 7 || c.bg != 0 {
            self.colored = true;
        }
    }

    /// Plain text: CR returns to column 0, LF starts a new line, TAB jumps to the next
    /// multiple of 8, every other byte (controls included) is a glyph.
    pub fn plain(bytes: &[u8]) -> Grid {
        let mut g = Grid::default();
        let (mut row, mut col) = (0usize, 0usize);
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\r' => {
                    col = 0;
                    if bytes.get(i + 1) == Some(&b'\n') {
                        i += 1;
                        row += 1;
                    }
                }
                b'\n' => {
                    row += 1;
                    col = 0;
                }
                b'\t' => col = (col / 8 + 1) * 8,
                b => {
                    g.put(row, col, Cell { ch: b, fg: 7, bg: 0 });
                    col += 1;
                }
            }
            i += 1;
        }
        while g.rows.len() < row + 1 && !bytes.is_empty() && bytes.last() != Some(&b'\n') {
            g.rows.push(Vec::new());
        }
        // Drop trailing empty lines.
        while g.rows.last().is_some_and(|r| r.iter().all(|c| c.ch == b' ')) {
            g.rows.pop();
        }
        g.cols = g.rows.iter().map(|r| r.len()).max().unwrap_or(0).max(1);
        g
    }

    /// ANSI.SYS emulation: SGR colours, cursor movement, save/restore, erase.
    pub fn ansi(bytes: &[u8], width: usize, ice: bool) -> Grid {
        const ANSI_TO_VGA: [u8; 8] = [0, 4, 2, 6, 1, 5, 3, 7];
        let width = if width == 0 { 80 } else { width.min(1000) };
        let mut g = Grid::default();
        let (mut x, mut y) = (0usize, 0usize);
        let (mut fg, mut bg, mut bold, mut blink, mut reverse) = (7u8, 0u8, false, false, false);
        let mut saved = (0usize, 0usize);
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if b == 0x1B && bytes.get(i + 1) == Some(&b'[') {
                let mut j = i + 2;
                while j < bytes.len() && !(0x40..=0x7E).contains(&bytes[j]) {
                    j += 1;
                }
                if j >= bytes.len() {
                    break;
                }
                let params: Vec<usize> = std::str::from_utf8(&bytes[i + 2..j]).unwrap_or("").trim_start_matches('?').split(';').map(|p| p.parse().unwrap_or(0)).collect();
                let n = |k: usize, d: usize| params.get(k).copied().filter(|&v| v != 0).unwrap_or(d);
                match bytes[j] {
                    b'm' => {
                        for &p in if params.is_empty() { &[0][..] } else { &params[..] } {
                            match p {
                                0 => {
                                    fg = 7;
                                    bg = 0;
                                    bold = false;
                                    blink = false;
                                    reverse = false;
                                }
                                1 => bold = true,
                                5 | 6 => blink = true,
                                7 => reverse = true,
                                22 => bold = false,
                                25 => blink = false,
                                27 => reverse = false,
                                30..=37 => fg = ANSI_TO_VGA[p - 30],
                                39 => fg = 7,
                                40..=47 => bg = ANSI_TO_VGA[p - 40],
                                49 => bg = 0,
                                90..=97 => fg = ANSI_TO_VGA[p - 90] + 8,
                                100..=107 => bg = ANSI_TO_VGA[p - 100] + 8,
                                _ => {}
                            }
                        }
                    }
                    b'A' => y = y.saturating_sub(n(0, 1)),
                    b'B' => y += n(0, 1),
                    b'C' => x = (x + n(0, 1)).min(width - 1),
                    b'D' => x = x.saturating_sub(n(0, 1)),
                    b'H' | b'f' => {
                        y = n(0, 1) - 1;
                        x = (n(1, 1) - 1).min(width - 1);
                    }
                    b'J' => {
                        if params.first() == Some(&2) {
                            g.rows.clear();
                            x = 0;
                            y = 0;
                        }
                    }
                    b'K' => {
                        if let Some(r) = g.rows.get_mut(y) {
                            r.truncate(x);
                        }
                    }
                    b's' => saved = (x, y),
                    b'u' => (x, y) = saved,
                    _ => {}
                }
                i = j + 1;
                continue;
            }
            match b {
                b'\r' => x = 0,
                b'\n' => {
                    y += 1;
                    x = 0;
                }
                0x1A => break,
                b'\t' => x = ((x / 8 + 1) * 8).min(width - 1),
                _ => {
                    let mut f = fg + if bold { 8 } else { 0 };
                    let mut k = bg + if blink && ice { 8 } else { 0 };
                    if reverse {
                        std::mem::swap(&mut f, &mut k);
                    }
                    g.put(y, x, Cell { ch: b, fg: f.min(15), bg: k.min(15) });
                    x += 1;
                    if x >= width {
                        x = 0;
                        y += 1;
                    }
                }
            }
            i += 1;
        }
        while g.rows.last().is_some_and(|r| r.iter().all(|c| c.ch == b' ' && c.bg == 0)) {
            g.rows.pop();
        }
        g.cols = width.max(g.rows.iter().map(|r| r.len()).max().unwrap_or(0));
        g
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_lines_and_tabs() {
        let g = Grid::plain(b"ab\r\n\tc\n");
        assert_eq!(g.rows.len(), 2);
        assert_eq!(g.get(1, 8).ch, b'c');
        assert_eq!(g.cols, 9);
    }

    #[test]
    fn ansi_colours() {
        let g = Grid::ansi(b"\x1b[1;31mX\x1b[0m\x1b[5;44mY", 80, true);
        assert_eq!(g.get(0, 0), Cell { ch: b'X', fg: 12, bg: 0 });
        assert_eq!(g.get(0, 1), Cell { ch: b'Y', fg: 7, bg: 9 });
        assert!(g.colored);
    }

    #[test]
    fn ansi_cursor() {
        let g = Grid::ansi(b"\x1b[2;5HZ", 80, false);
        assert_eq!(g.get(1, 4).ch, b'Z');
    }
}

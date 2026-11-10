//! Pixel-exact VGA text-mode rasteriser. Renders a region of the cell grid at one
//! VGA pixel per output pixel; the view scales the result by an integer factor with
//! nearest-neighbour sampling.

use super::grid::Grid;

pub struct VgaFont {
    pub name: &'static str,
    pub height: usize,
    pub data: &'static [u8],
}

pub static VGA_8X16: VgaFont = VgaFont { name: "IBM VGA 8×16", height: 16, data: include_bytes!("../../assets/vga8x16.bin") };
pub static VGA_8X14: VgaFont = VgaFont { name: "IBM EGA 8×14", height: 14, data: include_bytes!("../../assets/vga8x14.bin") };
pub static BIOS_8X8: VgaFont = VgaFont { name: "IBM VGA50 8×8", height: 8, data: include_bytes!("../../assets/bios8x8.bin") };

/// Pick the font named by SAUCE (`IBM VGA`, `IBM VGA50`, `IBM EGA`, `IBM EGA43`).
pub fn font_for(sauce_font: Option<&str>) -> &'static VgaFont {
    match sauce_font.map(|s| s.to_ascii_uppercase()) {
        Some(f) if f.contains("VGA50") || f.contains("EGA43") || f.contains("8X8") => &BIOS_8X8,
        Some(f) if f.contains("EGA") || f.contains("8X14") => &VGA_8X14,
        _ => &VGA_8X16,
    }
}

/// The 16-colour VGA palette, BGRA.
pub const VGA_PALETTE: [[u8; 4]; 16] = [
    [0x00, 0x00, 0x00, 0xFF],
    [0xAA, 0x00, 0x00, 0xFF],
    [0x00, 0xAA, 0x00, 0xFF],
    [0xAA, 0xAA, 0x00, 0xFF],
    [0x00, 0x00, 0xAA, 0xFF],
    [0xAA, 0x00, 0xAA, 0xFF],
    [0x00, 0x55, 0xAA, 0xFF],
    [0xAA, 0xAA, 0xAA, 0xFF],
    [0x55, 0x55, 0x55, 0xFF],
    [0xFF, 0x55, 0x55, 0xFF],
    [0x55, 0xFF, 0x55, 0xFF],
    [0xFF, 0xFF, 0x55, 0xFF],
    [0x55, 0x55, 0xFF, 0xFF],
    [0xFF, 0x55, 0xFF, 0xFF],
    [0x55, 0xFF, 0xFF, 0xFF],
    [0xFF, 0xFF, 0xFF, 0xFF],
];

/// Is pixel (gx, gy) of glyph `ch` set? With 9-pixel cells, column 8 repeats column 7
/// for the line-drawing range 0xC0–0xDF exactly like VGA hardware, so horizontal
/// lines join up; other glyphs get a blank 9th column.
#[inline]
pub fn pixel(font: &VgaFont, ch: u8, gx: usize, gy: usize) -> bool {
    let row = font.data[ch as usize * font.height + gy];
    let gx = if gx == 8 {
        if (0xC0..=0xDF).contains(&ch) {
            7
        } else {
            return false;
        }
    } else {
        gx
    };
    row & (0x80 >> gx) != 0
}

pub struct Region {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

/// Stream selection between two (row, col) cells, inclusive start, exclusive end.
pub type Selection = ((usize, usize), (usize, usize));

fn selected(sel: Option<Selection>, row: usize, col: usize) -> bool {
    match sel {
        Some((a, b)) => (row, col) >= a && (row, col) < b,
        None => false,
    }
}

/// Render `region` (in VGA pixels) into `out` as opaque BGRA.
pub fn render(grid: &Grid, font: &VgaFont, cell_w: usize, palette: &[[u8; 4]; 16], region: &Region, sel: Option<Selection>, out: &mut Vec<u8>) {
    out.clear();
    out.resize(region.w * region.h * 4, 0);
    let fh = font.height;
    for py in 0..region.h {
        let y = region.y + py;
        let (row, gy) = (y / fh, y % fh);
        let line = grid.rows.get(row);
        let dst = &mut out[py * region.w * 4..(py + 1) * region.w * 4];
        let mut px = 0;
        while px < region.w {
            let x = region.x + px;
            let (col, gx0) = (x / cell_w, x % cell_w);
            let cell = line.and_then(|l| l.get(col)).copied().unwrap_or(super::grid::BLANK);
            let inv = selected(sel, row, col);
            let (mut fg, mut bg) = (palette[cell.fg as usize & 15], palette[cell.bg as usize & 15]);
            if inv {
                std::mem::swap(&mut fg, &mut bg);
            }
            // Emit the rest of this cell's pixels on this scanline.
            let mut gx = gx0;
            while gx < cell_w && px < region.w {
                let c = if pixel(font, cell.ch, gx, gy) { fg } else { bg };
                dst[px * 4..px * 4 + 4].copy_from_slice(&c);
                gx += 1;
                px += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::grid::Grid;
    use super::*;

    /// Full blocks and box-drawing lines must tile without gaps (rows and columns).
    #[test]
    fn blocks_and_lines_are_gapless() {
        for cell_w in [8usize, 9] {
            let g = Grid::plain(b"\xDB\xDB\xDB\xDB\r\n\xDB\xDB\xDB\xDB\r\n\xC4\xC4\xC4\xC4\r\n");
            let font = &VGA_8X16;
            let region = Region { x: 0, y: 0, w: 4 * cell_w, h: 3 * 16 };
            let mut out = Vec::new();
            render(&g, font, cell_w, &VGA_PALETTE, &region, None, &mut out);
            let fg = VGA_PALETTE[7];
            // Rows 0..32 are the two block lines: every pixel lit.
            for y in 0..32 {
                for x in 0..region.w {
                    let p = &out[(y * region.w + x) * 4..(y * region.w + x) * 4 + 4];
                    if cell_w == 8 || x % 9 != 8 {
                        assert_eq!(p, fg, "gap at ({x},{y}) cell_w={cell_w}");
                    }
                }
            }
            // The horizontal line (0xC4) row 7 of the third text row is continuous,
            // including the 9th column in 9-pixel mode.
            let y = 32 + 7;
            for x in 0..region.w {
                let p = &out[(y * region.w + x) * 4..(y * region.w + x) * 4 + 4];
                assert_eq!(p, fg, "line break at x={x} cell_w={cell_w}");
            }
        }
    }

    #[test]
    fn nine_pixel_rule() {
        // 0xDB is in 0xC0..=0xDF: 9th column duplicated. 'A' is not.
        assert!(pixel(&VGA_8X16, 0xDB, 8, 5));
        assert!(!pixel(&VGA_8X16, b'A', 8, 5));
        // ░ (0xB0) is outside the range: blank 9th column, like real VGA.
        assert!(!pixel(&VGA_8X16, 0xB0, 8, 0));
    }
}

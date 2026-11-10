"""Convert Windows bitmap .FON fonts (NE container, FNT v2/v3 resources) from
The Ultimate Oldschool PC Font Pack into raw 1-bpp glyph tables.

Output: 256 glyphs x height bytes; byte = one row, MSB = leftmost pixel (8-wide fonts).
Run with:  python -I scripts/gen-vga-font.py <input.FON> <output.bin>
"""
import struct
import sys


def read_fon(path):
    data = open(path, "rb").read()
    if data[:2] != b"MZ":
        raise ValueError("not an MZ file")
    ne = struct.unpack_from("<I", data, 0x3C)[0]
    if data[ne:ne + 2] != b"NE":
        raise ValueError("not an NE file")
    rsrc = ne + struct.unpack_from("<H", data, ne + 0x24)[0]
    shift = struct.unpack_from("<H", data, rsrc)[0]
    pos = rsrc + 2
    fonts = []
    while True:
        type_id = struct.unpack_from("<H", data, pos)[0]
        if type_id == 0:
            break
        count = struct.unpack_from("<H", data, pos + 2)[0]
        pos += 8
        for _ in range(count):
            off, length = struct.unpack_from("<HH", data, pos)
            if type_id == 0x8008:  # RT_FONT
                fonts.append(data[off << shift:(off << shift) + (length << shift)])
            pos += 12
    if not fonts:
        raise ValueError("no FONT resource")
    return fonts[0]


def fnt_to_table(fnt):
    version = struct.unpack_from("<H", fnt, 0)[0]
    pix_height = struct.unpack_from("<H", fnt, 88)[0]
    first = fnt[95]
    last = fnt[96]
    if version == 0x200:
        table, entry = 118, 4
    elif version == 0x300:
        table, entry = 148, 6
    else:
        raise ValueError("unsupported FNT version %x" % version)
    out = bytearray(256 * pix_height)
    for ch in range(first, last + 1):
        e = table + (ch - first) * entry
        width = struct.unpack_from("<H", fnt, e)[0]
        if entry == 4:
            off = struct.unpack_from("<H", fnt, e + 2)[0]
        else:
            off = struct.unpack_from("<I", fnt, e + 2)[0]
        if width > 8:
            raise ValueError("glyph wider than 8 pixels: %d" % width)
        # Column-major: first byte-column holds pixels 0..7 for every row.
        for row in range(pix_height):
            out[ch * pix_height + row] = fnt[off + row]
    return pix_height, bytes(out)


def main():
    src, dst = sys.argv[1], sys.argv[2]
    height, table = fnt_to_table(read_fon(src))
    with open(dst, "wb") as f:
        f.write(table)
    print("%s: 8x%d, %d bytes" % (dst, height, len(table)))


if __name__ == "__main__":
    main()

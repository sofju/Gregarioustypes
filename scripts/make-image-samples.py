"""Image samples that need hand-built containers (no third-party Python packages):
ICO, ICNS, PNGs with a Display-P3 ICC profile / LUT-curve profile / gAMA, and a
CMYK TIFF carrying Windows' RSWOP profile.
Run:  python -I scripts/make-image-samples.py
"""
import os
import struct
import zlib

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "samples", "images", "generated")
os.makedirs(ROOT, exist_ok=True)


def out(name, data):
    with open(os.path.join(ROOT, name), "wb") as f:
        f.write(data)
    print("wrote", name, len(data))


def png_chunk(t, d):
    return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)


def png(w, h, rows_rgba8, extra=b""):
    raw = b"".join(b"\0" + r for r in rows_rgba8)
    return (b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) + extra
            + png_chunk(b"IDAT", zlib.compress(raw, 9)) + png_chunk(b"IEND", b""))


def swatches(w, h):
    """Pure primaries + a grey ramp: wide-gamut profiles make the primaries more saturated."""
    rows = []
    cols = [(255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 255, 0), (0, 255, 255), (255, 0, 255)]
    for y in range(h):
        r = bytearray()
        for x in range(w):
            if y < h // 2:
                c = cols[x * len(cols) // w]
                r += bytes((*c, 255))
            else:
                v = x * 255 // (w - 1)
                r += bytes((v, v, v, 255))
        rows.append(bytes(r))
    return rows


# ---------------------------------------------------------------- ICC builder

def s15(v):
    return struct.pack(">i", int(round(v * 65536)))


def xyz_tag(x, y, z):
    return b"XYZ \0\0\0\0" + s15(x) + s15(y) + s15(z)


def curv_gamma(g):
    return b"curv\0\0\0\0" + struct.pack(">I", 1) + struct.pack(">H", int(round(g * 256)))


def curv_table(n=256):
    # An S-shaped curve no gamma fits: forces the LUT path.
    vals = []
    for i in range(n):
        x = i / (n - 1)
        y = x ** 2.2 if x < 0.5 else 0.5 ** 2.2 + (x - 0.5) * (1 - 0.5 ** 2.2) / 0.5
        vals.append(int(round(max(0, min(1, y)) * 65535)))
    return b"curv\0\0\0\0" + struct.pack(">I", n) + b"".join(struct.pack(">H", v) for v in vals)


def para_srgb():
    # parametric type 3: sRGB
    p = [2.4, 1 / 1.055, 0.055 / 1.055, 1 / 12.92, 0.04045]
    return b"para\0\0\0\0" + struct.pack(">H", 3) + b"\0\0" + b"".join(s15(v) for v in p)


def desc(text):
    t = text.encode("ascii") + b"\0"
    return b"desc\0\0\0\0" + struct.pack(">I", len(t)) + t + b"\0" * 12 + b"\0" * 67


def icc_profile(name, rgb_xyz, trc):
    """ICC v2 display matrix/TRC profile; rgb_xyz are D50-adapted colorants."""
    tags = [
        (b"desc", desc(name)),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
        (b"rXYZ", xyz_tag(*rgb_xyz[0])),
        (b"gXYZ", xyz_tag(*rgb_xyz[1])),
        (b"bXYZ", xyz_tag(*rgb_xyz[2])),
        (b"rTRC", trc),
        (b"gTRC", trc),
        (b"bTRC", trc),
        (b"cprt", b"text\0\0\0\0Public domain\0"),
    ]
    off = 128 + 4 + 12 * len(tags)
    table = b""
    body = b""
    for sig, data in tags:
        while (off + len(body)) % 4:
            body += b"\0"
        table += sig + struct.pack(">II", off + len(body), len(data))
        body += data
    size = off + len(body)
    hdr = struct.pack(">I", size) + b"none" + struct.pack(">I", 0x02100000) + b"mntrRGB XYZ " + b"\0" * 12 + b"acsp" + b"MSFT" + b"\0" * 4
    hdr += b"\0" * 4 + b"\0" * 4 + b"\0" * 8 + struct.pack(">I", 0) + s15(0.9642) + s15(1.0) + s15(0.8249) + b"none"
    hdr = hdr.ljust(128, b"\0")
    return hdr + struct.pack(">I", len(tags)) + table + body


# Display P3 colorants, chromatically adapted to D50 (from Apple's Display P3 profile).
P3 = [(0.515102, 0.241182, -0.001049), (0.291965, 0.692236, 0.041882), (0.157153, 0.066582, 0.784378)]
SRGB = [(0.436066, 0.222488, 0.013916), (0.385147, 0.716873, 0.097076), (0.143066, 0.060608, 0.714096)]


def main():
    w, h = 600, 300
    rows = swatches(w, h)
    p3 = icc_profile("Display P3 (test)", P3, para_srgb())
    out("display-p3-icc.png", png(w, h, rows, png_chunk(b"iCCP", b"Display P3\0\0" + zlib.compress(p3))))
    out("srgb-plain.png", png(w, h, rows))
    lut = icc_profile("S-curve LUT (test)", SRGB, curv_table())
    out("lut-curve-icc.png", png(w, h, rows, png_chunk(b"iCCP", b"LUT\0\0" + zlib.compress(lut))))
    out("gamma-1.0.png", png(w, h, rows, png_chunk(b"gAMA", struct.pack(">I", 100000))))

    # The Netflix reference PNG holds PQ data but no cICP chunk: write a tagged copy.
    src = os.path.join(ROOT, "..", "avif-netflix", "hdr_cosmos01000_pq_cicp.png")
    if os.path.exists(src):
        d = open(src, "rb").read()
        ihdr_end = 8 + 12 + 13
        cicp = png_chunk(b"cICP", bytes((9, 16, 0, 1)))
        out("pq-bt2020-cicp-tagged.png", d[:ihdr_end] + cicp + d[ihdr_end:])

    # ICO with a 256px PNG entry and a 32px BMP entry.
    icon_png = png(256, 256, [b"".join(bytes(((x ^ y) & 255, x, y, 255)) for x in range(256)) for y in range(256)])
    bmp32 = bytearray(struct.pack("<IiiHHIIiiII", 40, 32, 64, 1, 32, 0, 0, 0, 0, 0, 0))
    for y in range(32):
        for x in range(32):
            bmp32 += bytes((255, 0, 0, 255))
    bmp32 += b"\0" * (4 * 32)
    entries = [(256, icon_png), (32, bytes(bmp32))]
    hdr = struct.pack("<HHH", 0, 1, len(entries))
    off = 6 + 16 * len(entries)
    dirs = b""
    blob = b""
    for size, data in entries:
        dirs += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(data), off + len(blob))
        blob += data
    out("icon.ico", hdr + dirs + blob)

    # ICNS with a 256px PNG (ic08) and 512px PNG (ic09).
    big = png(512, 512, [b"".join(bytes((x // 2, y // 2, 128, 255)) for x in range(512)) for y in range(512)])
    elems = [(b"ic08", icon_png), (b"ic09", big)]
    body = b"".join(t + struct.pack(">I", 8 + len(d)) + d for t, d in elems)
    out("icon.icns", b"icns" + struct.pack(">I", 8 + len(body)) + body)

    # CMYK TIFF (uncompressed, contiguous) with the RSWOP profile Windows ships.
    rswop = r"C:\Windows\System32\spool\drivers\color\RSWOP.icm"
    if os.path.exists(rswop):
        icc = open(rswop, "rb").read()
        cw, ch = 400, 200
        pix = bytearray()
        for y in range(ch):
            for x in range(cw):
                band = x * 5 // cw
                c = [(255, 0, 0, 0), (0, 255, 0, 0), (0, 0, 255, 0), (0, 0, 0, 255), (60, 40, 40, 20)][band]
                pix += bytes(c)
        entries = []
        data_off = 8
        ifd_entries = 11
        ifd_size = 2 + ifd_entries * 12 + 4
        pix_off = data_off + ifd_size
        icc_off = pix_off + len(pix)
        def e(tag, typ, count, value):
            return struct.pack("<HHII", tag, typ, count, value)
        ifd = struct.pack("<H", ifd_entries)
        ifd += e(256, 4, 1, cw) + e(257, 4, 1, ch) + e(258, 3, 1, 8) + e(259, 3, 1, 1) + e(262, 3, 1, 5)
        ifd += e(273, 4, 1, pix_off) + e(277, 3, 1, 4) + e(278, 4, 1, ch) + e(279, 4, 1, len(pix)) + e(284, 3, 1, 1)
        ifd += e(34675, 7, len(icc), icc_off)
        ifd += struct.pack("<I", 0)
        out("cmyk-rswop.tif", b"II*\0" + struct.pack("<I", data_off) + ifd + bytes(pix) + icc)


if __name__ == "__main__":
    main()

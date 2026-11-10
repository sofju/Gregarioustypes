"""Design-format samples into samples/design/:
- psd-tools/: real Photoshop files from the psd-tools test corpus (8/16/32-bit,
  PSB, grayscale, CMYK with spot channels, multichannel, transparency, layers);
- generated/: PSDs in the colour modes the corpus lacks (Indexed, Bitmap, Lab,
  Duotone, 16-bit grayscale), written directly.
Xara, Affinity and InDesign files come from installed apps or the user's own
documents (not copied here).
Run: python -I scripts/make-design-samples.py
"""
import math
import os
import struct
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CORPUS = os.path.join(ROOT, "samples", "design", "psd-tools")
GEN = os.path.join(ROOT, "samples", "design", "generated")
os.makedirs(CORPUS, exist_ok=True)
os.makedirs(GEN, exist_ok=True)

FILES = """1layer.psd 2layers.psd 16bit5x5.psd 32bit.psd 32bit.psb 16bit5x5.psb gray0.psd gray1.psb cmyk-gray-ramp.psd
cmyk-spot.psd multichannel-pattern-fill.psd transparentbg.psd transparentbg-gimp.psd semi-transparent-layers.psd
hidden-groups.psd 300dpi.psb layer_effects.psd advanced-blending.psd masks.psd text.psd metadata.psd artboard.psd
background-red-opacity-80.psd 0layers.psd""".split()
for f in FILES:
    dst = os.path.join(CORPUS, f)
    if not os.path.exists(dst):
        url = "https://raw.githubusercontent.com/psd-tools/psd-tools/main/tests/psd_files/" + f
        with urllib.request.urlopen(url) as r, open(dst, "wb") as o:
            o.write(r.read())


def psd(name, mode, depth, chans, w, h, planes, cmdata=b""):
    hdr = b"8BPS" + struct.pack(">H6xHIIHH", 1, chans, h, w, depth, mode)
    data = hdr + struct.pack(">I", len(cmdata)) + cmdata + struct.pack(">I", 0) + struct.pack(">I", 0)
    data += struct.pack(">H", 0) + b"".join(planes)
    with open(os.path.join(GEN, name), "wb") as f:
        f.write(data)


w, h = 256, 128
pal = bytearray(768)
for i in range(256):
    a = i / 256 * 2 * math.pi
    pal[i] = int(127 + 127 * math.cos(a))
    pal[256 + i] = int(127 + 127 * math.cos(a - 2.09))
    pal[512 + i] = int(127 + 127 * math.cos(a + 2.09))
psd("indexed.psd", 2, 8, 1, w, h, [bytes(x for y in range(h) for x in range(w))], bytes(pal))
row = (w + 7) // 8
bits = bytearray(row * h)
for y in range(h):
    for x in range(w):
        if ((x // 16 + y // 16) % 2 == 0) ^ ((x - 128) ** 2 + (y - 64) ** 2 < 50 ** 2):
            bits[y * row + x // 8] |= 0x80 >> (x % 8)
psd("bitmap.psd", 0, 1, 1, w, h, [bytes(bits)])
L = bytes(int(x / (w - 1) * 255) for y in range(h) for x in range(w))
A = bytes(int(y / (h - 1) * 255) for y in range(h) for x in range(w))
B = bytes(255 - int(y / (h - 1) * 255) for y in range(h) for x in range(w))
psd("lab.psd", 9, 8, 3, w, h, [L, A, B])
psd("duotone.psd", 8, 8, 1, w, h, [bytes((x + y) % 256 for y in range(h) for x in range(w))], b"\0" * 32)
psd("gray16.psd", 1, 16, 1, w, h, [b"".join(struct.pack(">H", int(x / (w - 1) * 65535)) for y in range(h) for x in range(w))])
print("ok", len(os.listdir(CORPUS)), "corpus files,", len(os.listdir(GEN)), "generated")

"""Generate document samples into samples/docs/generated/ (PDF, PS, EPS, AI, SVG,
HTML, Markdown, NZB). Uses the bundled Ghostscript and the system ffmpeg.
Run: python -I scripts/make-doc-samples.py
"""
import base64
import gzip
import os
import struct
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "samples", "docs", "generated")
GS = os.path.join(ROOT, "deps", "ghostscript", "bin", "gswin64c.exe")
os.makedirs(OUT, exist_ok=True)


def p(name):
    return os.path.join(OUT, name)


def write(name, data):
    mode = "wb" if isinstance(data, bytes) else "w"
    kw = {} if isinstance(data, bytes) else {"encoding": "utf-8", "newline": "\n"}
    with open(p(name), mode, **kw) as f:
        f.write(data)


def gs(*args):
    subprocess.run([GS, "-q", "-dNOPAUSE", "-dBATCH", "-dSAFER", *args], check=True)


# --- a photo for documents ---------------------------------------------------------
if not os.path.exists(p("photo.jpg")):
    subprocess.run(["ffmpeg", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=960x640:rate=1", "-frames:v", "1", "-q:v", "3", p("photo.jpg")], check=True)

# --- PostScript sources --------------------------------------------------------------
PROLOG = """%!PS-Adobe-3.0
%%Title: Gregarioustypes PostScript sample
%%Creator: make-doc-samples.py
%%Pages: {pages}
%%DocumentMedia: A4 595 842 0 () ()
%%EndComments
/Helvetica findfont 12 scalefont setfont
"""


def page(n, total, heavy=False):
    s = [f"%%Page: {n} {n}", "gsave"]
    s.append("0.95 0.95 1 setrgbcolor 0 0 595 842 rectfill")
    s.append(f"0.1 0.3 0.6 setrgbcolor /Helvetica-Bold findfont 28 scalefont setfont 60 760 moveto (Page {n} of {total}) show")
    s.append("0 setgray /Times-Roman findfont 11 scalefont setfont")
    y = 720
    for line in range(40):
        s.append(f"60 {y} moveto (Line {line + 1}: The quick brown fox jumps over the lazy dog. 0123456789 \\(page {n}\\)) show")
        y -= 15
    # Some vector art.
    s.append("gsave 420 120 translate")
    for k in range(36):
        s.append(f"{k / 36.0:.3f} 0.5 0.9 sethsbcolor 0 0 moveto 120 0 lineto 0.5 setlinewidth stroke 10 rotate")
    s.append("grestore")
    if heavy:
        # Hair-thin lines that only resolve when zoomed in (detail tiles).
        s.append("0.1 setlinewidth 0 0 0 setrgbcolor")
        for k in range(0, 400):
            s.append(f"{60 + k * 1.2:.1f} 40 moveto 0 30 rlineto stroke")
        s.append("/Helvetica findfont 0.8 scalefont setfont 60 30 moveto (tiny text only readable at 1600 percent zoom) show")
    s.append("grestore showpage")
    return "\n".join(s) + "\n"


def ps_doc(pages, heavy=False):
    return PROLOG.format(pages=pages) + "".join(page(i + 1, pages, heavy) for i in range(pages)) + "%%EOF\n"


write("document.ps", ps_doc(6))
write("_long.ps", ps_doc(300))
gs("-sDEVICE=pdfwrite", "-o", p("long-300-pages.pdf"), p("_long.ps"))
os.remove(p("_long.ps"))
gs("-sDEVICE=pdfwrite", "-o", p("document.pdf"), p("document.ps"))
# Encrypted with a user password.
gs("-sDEVICE=pdfwrite", "-sOwnerPassword=owner", "-sUserPassword=secret", "-o", p("password.pdf"), p("document.ps"))
# Owner password only (opens without a password, shows as encrypted).
gs("-sDEVICE=pdfwrite", "-sOwnerPassword=owner", "-dEncryptionR=3", "-dKeyLength=128", "-o", p("restricted.pdf"), p("document.ps"))

# A0 poster with hairlines: detail tiles when zoomed in.
poster = """%!PS-Adobe-3.0
%%BoundingBox: 0 0 2384 3370
<< /PageSize [2384 3370] >> setpagedevice
/Helvetica-Bold findfont 160 scalefont setfont 0.8 0.2 0.1 setrgbcolor 150 3000 moveto (A0 POSTER) show
0.05 setlinewidth 0 setgray
0 4 2384 { dup 0 moveto 3370 lineto stroke } for
/Helvetica findfont 2 scalefont setfont
200 200 moveto (microtext microtext microtext microtext) show
showpage
"""
write("_poster.ps", poster)
gs("-sDEVICE=pdfwrite", "-o", p("poster-a0.pdf"), p("_poster.ps"))
os.remove(p("_poster.ps"))

# Mixed page sizes and a rotated page.
mixed = """%!PS-Adobe-3.0
<< /PageSize [595 842] >> setpagedevice /Helvetica findfont 40 scalefont setfont 50 700 moveto (Portrait A4) show showpage
<< /PageSize [842 595] >> setpagedevice /Helvetica findfont 40 scalefont setfont 50 500 moveto (Landscape A4) show showpage
<< /PageSize [612 792] >> setpagedevice /Helvetica findfont 40 scalefont setfont 50 700 moveto (US Letter) show showpage
"""
write("_mixed.ps", mixed)
gs("-sDEVICE=pdfwrite", "-dAutoRotatePages=/None", "-o", p("mixed-sizes.pdf"), p("_mixed.ps"))
os.remove(p("_mixed.ps"))

# --- EPS ---------------------------------------------------------------------------------
eps = """%!PS-Adobe-3.0 EPSF-3.0
%%BoundingBox: 0 0 300 200
%%Title: Sunset badge
%%Creator: make-doc-samples.py
%%CreationDate: 2024-05-01
%%EndComments
0.98 0.6 0.2 setrgbcolor 0 0 300 200 rectfill
1 0.85 0.3 setrgbcolor 150 60 50 0 180 arc fill
0.2 0.1 0.3 setrgbcolor 0 0 300 60 rectfill
1 setgray /Helvetica-Bold findfont 26 scalefont setfont 64 150 moveto (SUNSET EPS) show
showpage
%%EOF
"""
write("badge.eps", eps)
# DOS EPS: binary header + PostScript + TIFF preview.
gs("-sDEVICE=tiff24nc", "-r72", "-dEPSCrop", "-o", p("_prev.tif"), p("badge.eps"))
with open(p("_prev.tif"), "rb") as f:
    tif = f.read()
os.remove(p("_prev.tif"))
ps = eps.encode()
hdr_len = 30
header = struct.pack("<IIIIIIIH", 0xC6D3D0C5, hdr_len, len(ps), 0, 0, hdr_len + len(ps), len(tif), 0xFFFF)
write("badge-tiff-preview.eps", header + ps + tif)

# --- Illustrator ---------------------------------------------------------------------------
# PDF-compatible AI: a PDF with Illustrator's creator string.
art = """%!PS-Adobe-3.0
<< /PageSize [400 300] >> setpagedevice
[ /Creator (Adobe Illustrator 28.0 \\(Windows\\)) /Title (Logo) /DOCINFO pdfmark
0.1 0.5 0.9 setrgbcolor 200 150 100 0 360 arc fill
1 setgray /Helvetica-Bold findfont 48 scalefont setfont 145 135 moveto (AI) show
showpage
"""
write("_art.ps", art)
gs("-sDEVICE=pdfwrite", "-o", p("logo-pdf-compatible.ai"), p("_art.ps"))
os.remove(p("_art.ps"))
# AI saved without PDF content: placeholder page + XMP thumbnail.
subprocess.run(["ffmpeg", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "mandelbrot=size=256x192", "-frames:v", "1", p("_thumb.jpg")], check=True)
with open(p("_thumb.jpg"), "rb") as f:
    thumb = base64.b64encode(f.read()).decode()
os.remove(p("_thumb.jpg"))
thumb = "&#xA;".join(thumb[i:i + 76] for i in range(0, len(thumb), 76))
placeholder = """%!PS-Adobe-3.0
<< /PageSize [612 792] >> setpagedevice
/Helvetica findfont 12 scalefont setfont 40 700 moveto (This is an Adobe\\256 Illustrator\\256 File that was) show
40 685 moveto (saved without PDF Content.) show
showpage
"""
write("_ph.ps", placeholder)
gs("-sDEVICE=pdfwrite", "-o", p("_ph.pdf"), p("_ph.ps"))
os.remove(p("_ph.ps"))
with open(p("_ph.pdf"), "rb") as f:
    pdf = f.read()
os.remove(p("_ph.pdf"))
xmp = ('<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">'
       '<rdf:Description xmlns:xmpGImg="http://ns.adobe.com/xap/1.0/g/img/"><xmpGImg:image>' + thumb +
       '</xmpGImg:image></rdf:Description></rdf:RDF></x:xmpmeta>')
# Appended as a comment after %%EOF: PDFium ignores it, our scanner finds it.
write("native-only.ai", pdf + b"\n%" + xmp.encode() + b"\n")
# PostScript-era Illustrator (AI 3 style header; self-contained PostScript).
write("legacy-ps.ai", "%!PS-Adobe-2.0 EPSF-1.2\n%%Creator: Adobe Illustrator(TM) 3.2\n%%BoundingBox: 0 0 200 200\n%%EndComments\n"
      "0 0.6 0.3 setrgbcolor 20 20 160 160 rectfill 1 setgray /Helvetica findfont 30 scalefont setfont 40 90 moveto (AI 3) show\nshowpage\n%%EOF\n")

# --- SVG ---------------------------------------------------------------------------------
svg_text = """<svg xmlns="http://www.w3.org/2000/svg" width="640" height="420" viewBox="0 0 640 420">
  <title>Gradient card with text</title>
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#4cc2ff"/><stop offset="1" stop-color="#7a33ff"/></linearGradient>
    <filter id="blur"><feGaussianBlur stdDeviation="6"/></filter>
  </defs>
  <rect x="20" y="20" width="600" height="380" rx="28" fill="url(#g)"/>
  <circle cx="520" cy="110" r="60" fill="#fff" opacity="0.35" filter="url(#blur)"/>
  <text x="60" y="140" font-family="Segoe UI, sans-serif" font-size="48" font-weight="700" fill="#fff">Gregarioustypes</text>
  <text x="60" y="200" font-family="Georgia, serif" font-size="24" fill="#fff">Vector text stays sharp at any zoom — café ✓</text>
  <path d="M60 300 Q 200 220 340 300 T 600 300" fill="none" stroke="#fff" stroke-width="4" stroke-dasharray="12 6"/>
</svg>
"""
write("card.svg", svg_text)
with gzip.open(p("card.svgz"), "wb") as f:
    f.write(svg_text.encode())
icon = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M12 2 2 22h20z" fill="#222"/><path d="M11 9h2v6h-2zm0 8h2v2h-2z" fill="#fc0"/></svg>\n'
write("icon-transparent.svg", icon)
with open(os.path.join(OUT, "photo.jpg"), "rb") as f:
    photo = base64.b64encode(f.read()).decode()
write("embedded-image.svg", f'<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="960" height="640">'
      f'<image width="960" height="640" xlink:href="data:image/jpeg;base64,{photo}"/>'
      f'<rect x="40" y="40" width="300" height="80" fill="#000" opacity="0.6"/><text x="60" y="95" font-size="40" fill="#fff">Embedded JPEG</text></svg>\n')
write("broken.svg", "<svg xmlns='http://www.w3.org/2000/svg'><rect width='10' height='10'</svg>")

# --- HTML ----------------------------------------------------------------------------------
os.makedirs(p("site"), exist_ok=True)
write(os.path.join("site", "style.css"), "body{font-family:Georgia,serif;max-width:46em;margin:2em auto;line-height:1.5;color:#222}h1{color:#0a6}.box{border:2px solid #0a6;padding:1em;border-radius:8px}\n")
with open(p("photo.jpg"), "rb") as src, open(p(os.path.join("site", "photo.jpg")), "wb") as dst:
    dst.write(src.read())
write(os.path.join("site", "index.html"), """<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Sample page</title><link rel="stylesheet" href="style.css"></head>
<body>
<h1>Local HTML page</h1>
<p>Stylesheet and image load from the same folder. <a href="https://example.com/">An external link</a> opens in the browser.</p>
<img src="photo.jpg" alt="test pattern" width="480">
<div class="box" id="js">JavaScript is <b>disabled</b> in previews.</div>
<script>document.getElementById('js').textContent = 'JavaScript RAN (it should not)';</script>
<p><img src="https://example.com/tracker.png" alt="(remote image blocked)"></p>
<table border="1" cellpadding="6"><tr><th>Format</th><th>Engine</th></tr><tr><td>HTML</td><td>WebView2</td></tr><tr><td>Markdown</td><td>pulldown-cmark</td></tr></table>
</body></html>
""")
write("legacy-cp1252.htm", "<html><head><title>Legacy page</title></head><body><h2>Caf\xe9 cr\xe8me</h2><p>Windows-1252 bytes without a charset.</p></body></html>".encode("cp1252"))

# --- Markdown --------------------------------------------------------------------------------
write("README.md", """# Gregarioustypes

Space-bar previews for Windows. Press <kbd>Space</kbd> in Explorer.

> [!NOTE]
> GitHub-style alerts, tables, task lists and footnotes are supported.

## Features

- [x] Images, HDR, video
- [x] Archives and disc images
- [ ] 3D models

| Format | Engine | Notes |
|:-------|:------:|------:|
| PDF    | PDFium | vector zoom |
| SVG    | resvg  | sharp |

```rust
fn main() {
    println!("Hello, world!");
}
```

Inline `code`, ~~strikethrough~~, a footnote[^1] and an image:

![test pattern](photo.jpg)

[^1]: Footnotes render at the bottom.
""")

# --- NZB ---------------------------------------------------------------------------------------
import random
random.seed(7)


def segs(n, size=768000, missing=()):
    out = []
    for i in range(1, n + 1):
        if i in missing:
            continue
        b = size if i < n else size // 3
        mid = "%x%x@news.example.com" % (random.getrandbits(48), i)
        out.append(f'      <segment bytes="{b}" number="{i}">{mid}</segment>')
    return "\n".join(out)


def nzb_file(subject, n, date=1715000000, poster="poster@example.com (Uploader)", groups=("alt.binaries.example",), missing=()):
    g = "\n".join(f"      <group>{x}</group>" for x in groups)
    return f"""  <file poster="{poster}" date="{date}" subject="{subject}">
    <groups>
{g}
    </groups>
    <segments>
{segs(n, missing=missing)}
    </segments>
  </file>
"""


name = "Some.Linux.Distro.2024.x64"
files = []
for k in range(1, 9):
    files.append(nzb_file(f'[{k}/14] - &quot;{name}.part{k:02d}.rar&quot; yEnc (1/{136 if k < 8 else 41})', 136 if k < 8 else 41, missing=(17, 18) if k == 3 else ()))
files.append(nzb_file(f'[9/14] - &quot;{name}.nfo&quot; yEnc (1/1)', 1))
files.append(nzb_file(f'[10/14] - &quot;{name}.sfv&quot; yEnc (1/1)', 1))
files.append(nzb_file(f'[11/14] - &quot;{name}.par2&quot; yEnc (1/1)', 1))
files.append(nzb_file(f'[12/14] - &quot;{name}.vol00+01.par2&quot; yEnc (1/2)', 2))
files.append(nzb_file(f'[13/14] - &quot;{name}.vol01+02.par2&quot; yEnc (1/3)', 3))
files.append(nzb_file(f'[14/14] - &quot;{name}.vol03+04.par2&quot; yEnc (1/6)', 6))
write("release.nzb", f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <head>
    <meta type="title">{name}</meta>
    <meta type="password">s3cret</meta>
    <meta type="category">Software &gt; Linux</meta>
    <meta type="tag">x64</meta>
    <meta type="tag">iso</meta>
  </head>
{''.join(files)}</nzb>
""")
# Minimal, old-style subjects without [n/m] and no head.
write("simple.nzb", """<?xml version="1.0" encoding="iso-8859-1" ?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
""" + nzb_file('holiday_photos.zip (1/12)', 12, groups=("alt.binaries.pictures", "alt.binaries.misc")) + nzb_file('Caf\xe9 notes.txt (1/1)', 1) + "</nzb>\n")
print("ok", sorted(os.listdir(OUT)))

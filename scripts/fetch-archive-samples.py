"""Download archive/disc samples we can't generate (M4), plus the tsMuxeR tool used
to author a Blu-ray ISO. Each file goes into its own folder.

Usage: python -I scripts/fetch-archive-samples.py
"""

import binascii
import os
import sys
import urllib.request
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SAMPLES = os.path.join(ROOT, "samples", "archives", "downloaded")
TOOLS = os.path.join(ROOT, "deps", "tools")
LA = "https://raw.githubusercontent.com/libarchive/libarchive/v3.8.1/libarchive/test/"

# (folder, url, file name); ".uu" files are uudecoded after download.
FILES = [
    ("libarchive", LA + "test_read_format_rar.rar.uu", "rar4.rar"),
    ("libarchive", LA + "test_read_format_rar5_multiple_files.rar.uu", "rar5-multiple.rar"),
    ("libarchive", LA + "test_read_format_cpio_svr4_gzip_rpm.rpm.uu", "cpio-gzip.rpm"),
    ("libarchive", LA + "test_read_format_cab_1.cab.uu", "sample.cab"),
    ("libarchive", LA + "test_read_format_7zip_lzma2.7z.uu", "lzma2.7z"),
    ("dmg", "https://github.com/rxhanson/Rectangle/releases/download/v2.0.4/Rectangle2.0.4.dmg", "Rectangle2.0.4.dmg"),
    ("pkg", "https://github.com/macadmins/outset/releases/download/v4.3.0.22031/Outset-4.3.0.22031.pkg", "Outset-4.3.0.22031.pkg"),
]

TSMUXER = "https://github.com/justdan96/tsMuxer/releases/download/2.7.0/tsMuxer-2.7.0-win64.zip"


def uudecode(text):
    out = bytearray()
    started = False
    for line in text.splitlines():
        if line.startswith("begin "):
            started = True
            continue
        if not started:
            continue
        if line.strip() == "end":
            break
        if not line:
            continue
        try:
            out += binascii.a2b_uu(line)
        except binascii.Error:
            # Some encoders pad short lines oddly: retry with the declared length.
            n = (ord(line[0]) - 32) & 63
            out += binascii.a2b_uu(line[: 1 + (n + 2) // 3 * 4])
    return bytes(out)


def get(url):
    req = urllib.request.Request(url, headers={"User-Agent": "Gregarioustypes-samples"})
    with urllib.request.urlopen(req, timeout=300) as r:
        return r.read()


for folder, url, name in FILES:
    d = os.path.join(SAMPLES, folder)
    os.makedirs(d, exist_ok=True)
    dst = os.path.join(d, name)
    if os.path.exists(dst):
        print("have", dst)
        continue
    try:
        data = get(url)
        if url.endswith(".uu"):
            data = uudecode(data.decode("latin-1"))
        open(dst, "wb").write(data)
        print("got ", dst, len(data))
    except Exception as e:
        print("FAIL", url, e, file=sys.stderr)

tsm_dir = os.path.join(TOOLS, "tsmuxer")
if not os.path.exists(os.path.join(tsm_dir, "tsMuxeR.exe")):
    os.makedirs(tsm_dir, exist_ok=True)
    z = os.path.join(tsm_dir, "tsMuxer.zip")
    open(z, "wb").write(get(TSMUXER))
    with zipfile.ZipFile(z) as zf:
        zf.extractall(tsm_dir)
    os.remove(z)
    print("got  tsMuxeR", os.listdir(tsm_dir))
else:
    print("have tsMuxeR")

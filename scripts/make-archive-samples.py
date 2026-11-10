"""Generate the archive and disc-image sample corpus (M4).

Uses the bundled 7-Zip (deps/7zip/7z.exe), ffmpeg, tsMuxeR (deps/tools/tsmuxer,
from fetch-archive-samples.py) and pycdlib. Output: samples/archives/generated.
"""

import bz2
import gzip
import io
import lzma
import os
import shutil
import struct
import subprocess
import sys
import tarfile
import zlib

import pycdlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "samples", "archives", "generated")
SRC = os.path.join(OUT, "_src")
SEVENZIP = os.path.join(ROOT, "deps", "7zip", "7z.exe")
TSMUXER = os.path.join(ROOT, "deps", "tools", "tsmuxer", "tsMuxeR.exe")
MEDIA = os.path.join(ROOT, "samples", "media", "generated")
os.makedirs(OUT, exist_ok=True)


def out(n):
    return os.path.join(OUT, n)


def ok(n):
    print("ok  ", n, os.path.getsize(out(n)))


def run(cmd, **kw):
    r = subprocess.run(cmd, capture_output=True, text=True, **kw)
    if r.returncode != 0:
        print("FAIL", cmd[:3], r.stderr[-400:] or r.stdout[-400:])
    return r.returncode == 0


# ---------------------------------------------------------------- source tree
if os.path.exists(SRC):
    shutil.rmtree(SRC)
os.makedirs(os.path.join(SRC, "docs", "nested deeper"), exist_ok=True)
os.makedirs(os.path.join(SRC, "media"), exist_ok=True)
os.makedirs(os.path.join(SRC, "empty folder"), exist_ok=True)
open(os.path.join(SRC, "readme.txt"), "w", encoding="utf-8").write("Sample archive for Gregarioustypes.\n" * 20)
open(os.path.join(SRC, "docs", "notes.md"), "w", encoding="utf-8").write("# Notes\n\n- one\n- two\n")
open(os.path.join(SRC, "docs", "nested deeper", "ünïcödé — 名前.txt"), "w", encoding="utf-8").write("unicode name\n")
open(os.path.join(SRC, "docs", "data.bin"), "wb").write(os.urandom(200_000))
for f in ("tagged-cover.mp3", "h264-aac.mp4"):
    if os.path.exists(os.path.join(MEDIA, f)):
        shutil.copy(os.path.join(MEDIA, f), os.path.join(SRC, "media", f))
shutil.copy(os.path.join(ROOT, "samples", "media", "generated", "cover.jpg"), os.path.join(SRC, "media", "cover.jpg"))


def sz(name, *args):
    if os.path.exists(out(name)):
        os.remove(out(name))
    if run([SEVENZIP, "a", *args, out(name), "*"], cwd=SRC):
        ok(name)


sz("sample.zip", "-tzip")
sz("encrypted-aes.zip", "-tzip", "-pgregarious", "-mem=AES256")
sz("solid.7z", "-t7z", "-mx=7", "-ms=on")
sz("encrypted-headers.7z", "-t7z", "-pgregarious", "-mhe=on")
sz("sample.wim", "-twim")
# 10k entries for listing speed.
big = os.path.join(OUT, "_big")
if not os.path.exists(big):
    for i in range(100):
        d = os.path.join(big, f"dir{i:03d}")
        os.makedirs(d, exist_ok=True)
        for j in range(100):
            open(os.path.join(d, f"file{j:03d}.txt"), "w").write(f"{i}-{j}\n")
if os.path.exists(out("10k-entries.zip")):
    os.remove(out("10k-entries.zip"))
run([SEVENZIP, "a", "-tzip", "-mx=1", out("10k-entries.zip"), "*"], cwd=big) and ok("10k-entries.zip")

# tar family (Python's tarfile).
def tar_bytes():
    b = io.BytesIO()
    with tarfile.open(fileobj=b, mode="w", format=tarfile.PAX_FORMAT) as t:
        t.add(SRC, arcname="project")
    return b.getvalue()


tb = tar_bytes()
open(out("sample.tar"), "wb").write(tb); ok("sample.tar")
open(out("sample.tar.gz"), "wb").write(gzip.compress(tb)); ok("sample.tar.gz")
shutil.copy(out("sample.tar.gz"), out("sample.tgz")); ok("sample.tgz")
open(out("sample.tar.bz2"), "wb").write(bz2.compress(tb)); ok("sample.tar.bz2")
shutil.copy(out("sample.tar.bz2"), out("sample.tbz2")); ok("sample.tbz2")
open(out("sample.tar.xz"), "wb").write(lzma.compress(tb)); ok("sample.tar.xz")
open(out("readme.txt.gz"), "wb").write(gzip.compress(open(os.path.join(SRC, "readme.txt"), "rb").read())); ok("readme.txt.gz")
shutil.copy(out("readme.txt.gz"), out("readme.gzip")); ok("readme.gzip")


# cpio (SVR4 "newc") and its compressed forms.
def cpio_newc(root):
    o = io.BytesIO()
    ino = 1
    for dirpath, dirs, files in os.walk(root):
        dirs.sort()
        for n in sorted(dirs) + sorted(files):
            p = os.path.join(dirpath, n)
            rel = os.path.relpath(p, root).replace("\\", "/")
            isdir = os.path.isdir(p)
            data = b"" if isdir else open(p, "rb").read()
            name = rel.encode("utf-8") + b"\0"
            mode = 0o040755 if isdir else 0o100644
            hdr = "070701" + "".join(f"{v:08X}" for v in (ino, mode, 0, 0, 1, int(os.path.getmtime(p)), len(data), 0, 0, 0, 0, len(name), 0))
            o.write(hdr.encode() + name)
            o.write(b"\0" * ((4 - (110 + len(name)) % 4) % 4))
            o.write(data)
            o.write(b"\0" * ((4 - len(data) % 4) % 4))
            ino += 1
    name = b"TRAILER!!!\0"
    o.write(("070701" + "".join(f"{v:08X}" for v in (0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, len(name), 0))).encode() + name)
    o.write(b"\0" * ((4 - (110 + len(name)) % 4) % 4))
    return o.getvalue()


cp = cpio_newc(SRC)
open(out("sample.cpio"), "wb").write(cp); ok("sample.cpio")
open(out("sample.cpgz"), "wb").write(gzip.compress(cp)); ok("sample.cpgz")
open(out("sample.cpbz2"), "wb").write(bz2.compress(cp)); ok("sample.cpbz2")


# Debian package: ar archive with debian-binary, control.tar.gz, data.tar.xz.
def ar(members):
    o = io.BytesIO()
    o.write(b"!<arch>\n")
    for name, data in members:
        o.write(f"{name:<16}{0:<12}{0:<6}{0:<6}{'100644':<8}{len(data):<10}`\n".encode())
        o.write(data)
        if len(data) % 2:
            o.write(b"\n")
    return o.getvalue()


def tar_of(files, compress):
    b = io.BytesIO()
    with tarfile.open(fileobj=b, mode="w:" + compress) as t:
        for name, data in files:
            ti = tarfile.TarInfo(name)
            ti.size = len(data)
            ti.mode = 0o644
            t.addfile(ti, io.BytesIO(data))
    return b.getvalue()


control = (b"Package: gregarious-sample\nVersion: 1.2.3-1\nArchitecture: amd64\nMaintainer: Sample Maintainer <sample@example.invalid>\n"
           b"Installed-Size: 512\nDepends: libc6 (>= 2.34), zlib1g\nSection: utils\nPriority: optional\nHomepage: https://example.invalid/\n"
           b"Description: Sample package for previews\n A longer description of the sample package,\n spanning two lines.\n")
deb = ar([("debian-binary", b"2.0\n"),
          ("control.tar.gz", tar_of([("./control", control), ("./md5sums", b"0123 usr/bin/sample\n")], "gz")),
          ("data.tar.xz", tar_of([("./usr/bin/sample", b"#!/bin/sh\necho hi\n"), ("./usr/share/doc/gregarious-sample/copyright", b"MIT\n")], "xz"))])
open(out("gregarious-sample_1.2.3-1_amd64.deb"), "wb").write(deb); ok("gregarious-sample_1.2.3-1_amd64.deb")

# ---------------------------------------------------------------- ISO / UDF images
def make_iso(path, udf=False, label="SAMPLE_DISC"):
    iso = pycdlib.PyCdlib()
    if udf:
        iso.new(interchange_level=3, joliet=3, udf="2.60", vol_ident=label)
    else:
        iso.new(interchange_level=3, joliet=3, rock_ridge="1.09", vol_ident=label)
    files = [("readme.txt", os.path.join(SRC, "readme.txt")), ("docs/notes.md", os.path.join(SRC, "docs", "notes.md")),
             ("docs/data.bin", os.path.join(SRC, "docs", "data.bin")), ("media/cover.jpg", os.path.join(SRC, "media", "cover.jpg"))]
    made = set()
    for rel, src in files:
        parts = rel.split("/")
        for k in range(1, len(parts)):
            d = "/".join(parts[:k])
            if d not in made:
                made.add(d)
                kw = {"iso_path": "/" + d.upper(), "joliet_path": "/" + d}
                if udf:
                    kw["udf_path"] = "/" + d
                else:
                    kw["rr_name"] = parts[k - 1]
                iso.add_directory(**kw)
        isoname = "/" + "/".join(p.upper() for p in parts[:-1] + [parts[-1].upper().replace("-", "_").split(".")[0][:8] + "." + parts[-1].split(".")[-1].upper()[:3] + ";1"])
        kw = {"iso_path": isoname, "joliet_path": "/" + rel}
        if udf:
            kw["udf_path"] = "/" + rel
        else:
            kw["rr_name"] = parts[-1]
        iso.add_file(src, **kw)
    iso.write(path)
    iso.close()


make_iso(out("data.iso")); ok("data.iso")
make_iso(out("udf.iso"), udf=True, label="UDF_DISC"); ok("udf.iso")
iso = open(out("data.iso"), "rb").read()
assert len(iso) % 2048 == 0


def msf(lba):
    lba += 150
    bcd = lambda v: ((v // 10) << 4) | (v % 10)
    return bytes([bcd(lba // 4500), bcd((lba // 75) % 60), bcd(lba % 75)])


SYNC = b"\x00" + b"\xff" * 10 + b"\x00"


def raw_mode1(data, sub=0):
    """2048-byte sectors -> 2352-byte Mode 1 raw sectors (EDC/ECC left zero)."""
    o = bytearray()
    for i in range(len(data) // 2048):
        o += SYNC + msf(i) + b"\x01" + data[i * 2048:(i + 1) * 2048] + b"\0" * 288
        if sub:
            o += b"\0" * sub
    return bytes(o)


raw = raw_mode1(iso)
open(out("disc.bin"), "wb").write(raw); ok("disc.bin")
open(out("disc.cue"), "w").write('FILE "disc.bin" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n'); ok("disc.cue")
# Mixed-mode: a data track followed by an audio track.
audio = b"\0" * (2352 * 75 * 3)
open(out("mixed.bin"), "wb").write(raw + audio)
open(out("mixed.cue"), "w").write('FILE "mixed.bin" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    PREGAP 00:02:00\n'
                                  f'    INDEX 01 {len(raw) // 2352 // 4500:02d}:{(len(raw) // 2352 // 75) % 60:02d}:{len(raw) // 2352 % 75:02d}\n'); ok("mixed.cue")
open(out("alcohol.mdf"), "wb").write(raw); ok("alcohol.mdf")
open(out("alcohol.mds"), "wb").write(b"MEDIA DESCRIPTOR" + b"\x01\x05" + b"\0" * 70); ok("alcohol.mds")
open(out("clonecd.img"), "wb").write(raw); ok("clonecd.img")
open(out("clonecd.ccd"), "w").write("[CloneCD]\nVersion=3\n[Disc]\nTocEntries=4\nSessions=1\n[TRACK 1]\nMODE=1\nINDEX 1=0\n"); ok("clonecd.ccd")
open(out("blindwrite.b5i"), "wb").write(raw); ok("blindwrite.b5i")
open(out("blindwrite.b6i"), "wb").write(raw_mode1(iso, sub=96)); ok("blindwrite.b6i")
open(out("instant.pdi"), "wb").write(b"PDI-HEADER" + b"\0" * (4096 - 10) + iso); ok("instant.pdi")
open(out("discjuggler.cdi"), "wb").write(raw + b"\0" * 1024 + struct.pack("<I", len(raw))); ok("discjuggler.cdi")


# Nero: 2048-byte track data, then chunks and a NER5 footer.
def nrg(data):
    chunks = bytearray()
    # DAOX: size, UPC(13), pad, toc type, first, last track, then one track record.
    track = b"\0" * 12 + struct.pack(">HHI", 2048, 0x0000, 0) + struct.pack(">QQQ", 0, 0, len(data))
    daox_body = struct.pack(">I", 0) + b"\0" * 13 + b"\0" + struct.pack(">H", 0) + bytes([1, 1]) + track
    chunks += b"DAOX" + struct.pack(">I", len(daox_body)) + daox_body
    chunks += b"SINF" + struct.pack(">I", 4) + struct.pack(">I", 1)
    chunks += b"MTYP" + struct.pack(">I", 4) + struct.pack(">I", 1)
    chunks += b"END!" + struct.pack(">I", 0)
    return data + bytes(chunks) + b"NER5" + struct.pack(">Q", len(data))


open(out("nero.nrg"), "wb").write(nrg(iso)); ok("nero.nrg")


# UltraISO compressed image (ISZ v1, zlib chunks).
def isz(data, chunk=16 * 2048):
    blocks = [data[i:i + chunk] for i in range(0, len(data), chunk)]
    comp = []
    table = bytearray()
    for b in blocks:
        if b.count(0) == len(b):
            comp.append(b"")
            v = (0 << 22) | len(b)
        else:
            c = zlib.compress(b, 9)
            if len(c) < len(b):
                comp.append(c)
                v = (2 << 22) | len(c)
            else:
                comp.append(b)
                v = (1 << 22) | len(b)
        table += struct.pack("<I", v)[:3]
    key = b"IsZ!"
    enc = bytes(table[i] ^ (~key[i & 3] & 0xFF) for i in range(len(table)))
    header_size = 64
    ptr_offs = header_size
    data_offs = ptr_offs + len(enc)
    hdr = struct.pack("<4sBbIHIbqIIBbIIIb", b"IsZ!", header_size, 1, 0x12345678, 2048, len(data) // 2048, 0, 0,
                      len(blocks), chunk, 3, 0, ptr_offs, 0, data_offs, 0)
    hdr += b"\0" * (header_size - len(hdr))
    return hdr + enc + b"".join(comp)


open(out("ultraiso.isz"), "wb").write(isz(iso)); ok("ultraiso.isz")

# ---------------------------------------------------------------- Blu-ray (tsMuxeR)
bd_dir = os.path.join(OUT, "_bd")
os.makedirs(bd_dir, exist_ok=True)
hevc = os.path.join(bd_dir, "video.hevc")
ac3 = os.path.join(bd_dir, "audio.ac3")
ff = shutil.which("ffmpeg") or "ffmpeg"
if not os.path.exists(hevc):
    run([ff, "-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=24000/1001:duration=12", "-vf", "format=yuv420p10le",
         "-c:v", "libx265", "-preset", "ultrafast", "-x265-params",
         "log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:hdr10=1:repeat-headers=1:aud=1:hrd=1:"
         "master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,300", "-bsf:v", "hevc_mp4toannexb", hevc])
if not os.path.exists(ac3):
    run([ff, "-y", "-loglevel", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=12:sample_rate=48000", "-ac", "6", "-c:a", "ac3", "-b:a", "448k", ac3])
meta = os.path.join(bd_dir, "bd.meta")
open(meta, "w").write("MUXOPT --no-pcr-on-video-pid --new-audio-pes --blu-ray-v3 --vbr --custom-chapters=00:00:00.000;00:00:04.000;00:00:08.000 --vbv-len=500\n"
                      f'V_MPEGH/ISO/HEVC, "{hevc}", fps=23.976, insertSEI, contSPS\n'
                      f'A_AC3, "{ac3}", lang=eng\n')
if os.path.exists(TSMUXER):
    for target in (out("bluray-uhd.iso"), os.path.join(OUT, "bluray-folder")):
        if os.path.isdir(target):
            shutil.rmtree(target)
        elif os.path.exists(target):
            os.remove(target)
        if run([TSMUXER, meta, target]):
            print("ok  ", os.path.basename(target))
else:
    print("SKIP Blu-ray: run scripts/fetch-archive-samples.py first")

shutil.rmtree(SRC, ignore_errors=True)
print("done")

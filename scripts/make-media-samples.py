"""Generate the media sample corpus (M3) with ffmpeg and a few hand-built files.

Usage: python scripts/make-media-samples.py [out_dir]
Default out_dir: samples/media/generated
"""

import math
import os
import shutil
import struct
import subprocess
import sys
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "samples", "media", "generated"))
os.makedirs(OUT, exist_ok=True)

FFMPEG = shutil.which("ffmpeg") or "ffmpeg"
DUR = 6
failures = []


def out(name):
    return os.path.join(OUT, name)


def ff(name, *args, inputs=None, overwrite=True):
    """Run ffmpeg; `inputs` is a list of argument lists placed before the outputs."""
    cmd = [FFMPEG, "-hide_banner", "-loglevel", "error"]
    if overwrite:
        cmd.append("-y")
    for i in inputs or []:
        cmd += i
    cmd += list(args)
    cmd.append(out(name))
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0 or not os.path.exists(out(name)):
        failures.append((name, r.stderr.strip()[-400:]))
        print("FAIL", name, r.stderr.strip()[-300:])
        return False
    print("ok  ", name, os.path.getsize(out(name)))
    return True


def copy(src, dst):
    if os.path.exists(out(src)):
        shutil.copyfile(out(src), out(dst))
        print("ok  ", dst, "(copy of", src + ")")


VIDEO = ["-f", "lavfi", "-i", f"testsrc2=size=1280x720:rate=25:duration={DUR}"]
VIDEO_SD = ["-f", "lavfi", "-i", f"testsrc2=size=720x576:rate=25:duration={DUR}"]
VIDEO_SMALL = ["-f", "lavfi", "-i", f"testsrc2=size=352x288:rate=25:duration={DUR}"]
# A little tune instead of a single beep: stepping pitch, stereo.
TUNE = r"0.25*sin(2*PI*(330+110*floor(mod(t*2\,4)))*t)"
AUDIO = ["-f", "lavfi", "-i", f"aevalsrc={TUNE}|{TUNE.replace('330', '440')}:s=48000:d={DUR}"]
AUDIO44 = ["-f", "lavfi", "-i", f"aevalsrc={TUNE}|{TUNE.replace('330', '440')}:s=44100:d=10"]
AUDIO51 = ["-f", "lavfi", "-i", "aevalsrc=" + "|".join(f"0.2*sin(2*PI*{f}*t)" for f in (330, 440, 550, 60, 660, 770)) + f":s=48000:d={DUR}:c=5.1"]
TAGS = ["-metadata", "title=Sample Tune", "-metadata", "artist=Gregarioustypes", "-metadata", "album=Test Corpus",
        "-metadata", "track=3/12", "-metadata", "date=2026", "-metadata", "genre=Electronic"]

# ---------------------------------------------------------------- cover art
ff("cover.jpg", "-frames:v", "1", "-q:v", "3", inputs=[["-f", "lavfi", "-i", "mandelbrot=size=600x600:end_pts=1"]])
COVER = ["-i", out("cover.jpg")]

# ---------------------------------------------------------------- video
H264 = ["-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p"]
ff("h264-aac.mp4", "-map", "0:v", "-map", "1:a", *H264, "-c:a", "aac", "-b:a", "128k", "-movflags", "+faststart",
   "-metadata", "title=H.264 sample", inputs=[VIDEO, AUDIO])
ff("h264-aac.m4v", "-map", "0:v", "-map", "1:a", *H264, "-c:a", "aac", "-f", "mp4", inputs=[VIDEO, AUDIO])
ff("h264-aac.mov", "-map", "0:v", "-map", "1:a", *H264, "-c:a", "aac", "-f", "mov", inputs=[VIDEO, AUDIO])
copy("h264-aac.mov", "h264-aac.qt")
ff("prores-proxy.mov", "-map", "0:v", "-map", "1:a", "-c:v", "prores_ks", "-profile:v", "0", "-c:a", "pcm_s16le", "-t", "3",
   inputs=[VIDEO, AUDIO])
# Phone-style portrait video (rotation in the track matrix).
if ff("_landscape.mp4", "-map", "0:v", "-map", "1:a", *H264, "-c:a", "aac", inputs=[VIDEO, AUDIO]):
    ff("portrait-rotated.mp4", "-c", "copy", inputs=[["-display_rotation", "90", "-i", out("_landscape.mp4")]])
    os.remove(out("_landscape.mp4"))

# Matroska with two audio tracks, subtitles and chapters.
srt = out("_subs.srt")
with open(srt, "w", encoding="utf-8") as f:
    f.write("1\n00:00:00,500 --> 00:00:02,500\nHello from the subtitle track\n\n2\n00:00:03,000 --> 00:00:05,500\nÇa marche — 字幕\n")
meta = out("_chapters.txt")
with open(meta, "w", encoding="utf-8") as f:
    f.write(";FFMETADATA1\ntitle=Multi-track sample\n")
    for i, (s, e) in enumerate([(0, 2000), (2000, 4000), (4000, 6000)]):
        f.write(f"[CHAPTER]\nTIMEBASE=1/1000\nSTART={s}\nEND={e}\ntitle=Chapter {i + 1}\n")
ff("multitrack.mkv", "-map", "0:v", "-map", "1:a", "-map", "2:a", "-map", "3:s", "-map_metadata", "4", "-map_chapters", "4",
   *H264, "-c:a:0", "aac", "-c:a:1", "libopus", "-c:s", "srt",
   "-metadata:s:a:0", "language=eng", "-metadata:s:a:0", "title=Stereo AAC",
   "-metadata:s:a:1", "language=fra", "-metadata:s:a:1", "title=Opus",
   "-metadata:s:s:0", "language=eng",
   inputs=[VIDEO, AUDIO, AUDIO, ["-i", srt], ["-i", meta]])
os.remove(srt)

ff("av1-opus.mkv", "-map", "0:v", "-map", "1:a", "-c:v", "libsvtav1", "-preset", "10", "-crf", "40", "-pix_fmt", "yuv420p10le",
   "-c:a", "libopus", inputs=[VIDEO, AUDIO])
ff("vp9.webm", "-map", "0:v", "-map", "1:a", "-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-b:v", "1M",
   "-c:a", "libopus", inputs=[VIDEO, AUDIO])

# AVI family.
ff("mpeg4-mp3.avi", "-map", "0:v", "-map", "1:a", "-c:v", "mpeg4", "-q:v", "4", "-c:a", "libmp3lame", "-b:a", "128k",
   inputs=[VIDEO, AUDIO])
ff("divx.divx", "-map", "0:v", "-map", "1:a", "-c:v", "mpeg4", "-vtag", "DIVX", "-q:v", "4", "-c:a", "libmp3lame",
   "-f", "avi", inputs=[VIDEO, AUDIO])
ff("xvid.xvid", "-map", "0:v", "-map", "1:a", "-c:v", "libxvid", "-q:v", "4", "-c:a", "libmp3lame", "-f", "avi",
   inputs=[VIDEO, AUDIO])

# Flash video.
ff("flv1-mp3.flv", "-map", "0:v", "-map", "1:a", "-c:v", "flv", "-q:v", "4", "-c:a", "libmp3lame", "-ar", "44100",
   inputs=[VIDEO_SMALL, AUDIO])
ff("h264.flv", "-map", "0:v", "-map", "1:a", *H264, "-c:a", "aac", inputs=[VIDEO, AUDIO])

# Windows Media.
ff("wmv2.wmv", "-map", "0:v", "-map", "1:a", "-c:v", "wmv2", "-b:v", "1500k", "-c:a", "wmav2", "-b:a", "128k", "-f", "asf",
   inputs=[VIDEO, AUDIO])
copy("wmv2.wmv", "recording.dvr-ms")

# MPEG program/elementary streams.
MPEG2 = ["-c:v", "mpeg2video", "-b:v", "4M", "-maxrate", "8M", "-bufsize", "1835k"]
ff("mpeg2-ps.mpg", "-map", "0:v", "-map", "1:a", *MPEG2, "-c:a", "mp2", "-b:a", "192k", "-f", "mpeg", inputs=[VIDEO, AUDIO])
copy("mpeg2-ps.mpg", "mpeg2-ps.mpeg")
copy("mpeg2-ps.mpg", "mpeg2-ps.mpe")
ff("mpeg1.m1v", "-c:v", "mpeg1video", "-b:v", "2M", "-f", "mpeg1video", inputs=[VIDEO_SMALL])
copy("mpeg1.m1v", "mpeg1.mpv")
ff("mpeg2.m2v", *MPEG2, "-f", "mpeg2video", inputs=[VIDEO])
ff("dvd.vob", "-map", "0:v", "-map", "1:a", *MPEG2, "-aspect", "16:9", "-r", "25", "-c:a", "ac3", "-b:a", "448k", "-f", "vob",
   inputs=[VIDEO_SD, AUDIO51])
if not ff("hddvd.evo", "-map", "0:v", "-map", "1:a", *H264, "-c:a", "eac3", "-f", "vob", inputs=[VIDEO, AUDIO51]):
    copy("dvd.vob", "hddvd.evo")

# Transport streams.
ff("h264-aac.ts", "-map", "0:v", "-map", "1:a", *H264, "-c:a", "aac", "-f", "mpegts", inputs=[VIDEO, AUDIO])
copy("h264-aac.ts", "h264-aac.tp")
copy("h264-aac.ts", "camera.jts")
ff("mpeg2-ac3.m2t", "-map", "0:v", "-map", "1:a", *MPEG2, "-c:a", "ac3", "-f", "mpegts", inputs=[VIDEO, AUDIO])
ff("bluray.m2ts", "-map", "0:v", "-map", "1:a", *H264, "-c:a", "ac3", "-f", "mpegts", "-mpegts_m2ts_mode", "1",
   inputs=[VIDEO, AUDIO51])

# Ogg Media, RealMedia.
ff("theora-vorbis.ogm", "-map", "0:v", "-map", "1:a", "-c:v", "libtheora", "-q:v", "6", "-c:a", "libvorbis", "-f", "ogg",
   inputs=[VIDEO, AUDIO])
if ff("realvideo.rmvb", "-map", "0:v", "-map", "1:a", "-c:v", "rv20", "-b:v", "800k", "-c:a", "ac3", "-f", "rm",
      inputs=[VIDEO_SMALL, AUDIO]):
    copy("realvideo.rmvb", "realvideo.rv")
ff("realaudio.ra", "-map", "0:a", "-c:a", "ac3", "-b:a", "96k", "-f", "rm", inputs=[AUDIO])

# 4K HEVC Main10 for decode performance.
ff("hevc-4k-60.mp4", "-map", "0:v", "-map", "1:a", "-c:v", "libx265", "-preset", "ultrafast", "-crf", "28", "-pix_fmt",
   "yuv420p10le", "-tag:v", "hvc1", "-c:a", "aac", "-t", "3", "-x265-params", "log-level=error",
   inputs=[["-f", "lavfi", "-i", "testsrc2=size=3840x2160:rate=60:duration=3"], AUDIO])


# ---------------------------------------------------------------- HDR test patterns
def png16(path, w, h, rows):
    """Write a 16-bit RGB PNG; `rows(y)` returns the packed big-endian row bytes."""
    raw = bytearray()
    for y in range(h):
        raw += b"\x00" + rows(y)
    def chunk(t, d):
        c = t + d
        return struct.pack(">I", len(d)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 16, 2, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(bytes(raw), 6)))
        f.write(chunk(b"IEND", b""))


def pq(nits):
    m1, m2, c1, c2, c3 = 2610 / 16384, 2523 / 4096 * 128, 3424 / 4096, 2413 / 4096 * 32, 2392 / 4096 * 32
    y = max(nits, 0) / 10000.0
    p = y ** m1
    return ((c1 + c2 * p) / (1 + c3 * p)) ** m2


def hlg(nits, peak=1000.0):
    # Display light -> scene light (inverse OOTF, gamma 1.2 at 1000 nits) -> HLG OETF.
    ys = (min(nits, peak) / peak) ** (1 / 1.2)
    a, b, c = 0.17883277, 0.28466892, 0.55991073
    return math.sqrt(3 * ys) if ys <= 1 / 12 else a * math.log(12 * ys - b) + c


HDR_NITS = [1, 10, 50, 100, 203, 400, 600, 1000, 1500, 2000, 4000, 10000]
W, H = 1920, 1080
PATCH = 200


def patches(encode, nits_list):
    """Two rows of six square patches on black, then a full-width ramp up to the
    brightest patch (so the frame's real peak matches the list)."""
    top = encode(max(nits_list))
    cols = 6
    gap = (W - cols * PATCH) // (cols + 1)
    def row(y):
        line = bytearray(W * 6)
        band = None
        if 140 <= y < 140 + PATCH:
            band = 0
        elif 440 <= y < 440 + PATCH:
            band = 1
        if band is not None:
            for i in range(cols):
                k = band * cols + i
                if k >= len(nits_list):
                    continue
                v = int(round(encode(nits_list[k]) * 65535))
                px = struct.pack(">HHH", v, v, v)
                x0 = gap + i * (PATCH + gap)
                line[x0 * 6:(x0 + PATCH) * 6] = px * PATCH
        elif 800 <= y < 1000:
            for x in range(W):
                v = int(round(x / (W - 1) * top * 65535))
                line[x * 6:x * 6 + 6] = struct.pack(">HHH", v, v, v)
        return bytes(line)
    return row


pq_png = out("_pq.png")
png16(pq_png, W, H, patches(pq, HDR_NITS))
X265_PQ = ("colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:range=limited:hdr10=1:repeat-headers=1:log-level=error:"
           "master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=10000,400")
TO_YUV = "scale=out_color_matrix=bt2020:out_range=tv:flags=accurate_rnd+full_chroma_int,format=yuv420p10le"
for name, extra in (("hdr10-pq-patches.mkv", []), ("hdr10-pq-patches.mp4", ["-tag:v", "hvc1"])):
    ff(name, "-t", "5", "-vf", TO_YUV, "-c:v", "libx265", "-preset", "fast", "-crf", "12", "-x265-params", X265_PQ,
       "-color_primaries", "bt2020", "-color_trc", "smpte2084", "-colorspace", "bt2020nc", "-color_range", "tv", *extra,
       inputs=[["-loop", "1", "-framerate", "25", "-i", pq_png]])
os.remove(pq_png)

# A typical 1000-nit master: nothing above the panel's peak, so it should show 1:1.
pq1k_png = out("_pq1k.png")
png16(pq1k_png, W, H, patches(pq, [1, 10, 50, 100, 203, 400, 600, 800, 1000]))
ff("hdr10-1000nit-master.mkv", "-t", "5", "-vf", TO_YUV, "-c:v", "libx265", "-preset", "fast", "-crf", "12", "-x265-params",
   X265_PQ.replace("max-cll=10000,400", "max-cll=1000,300"),
   "-color_primaries", "bt2020", "-color_trc", "smpte2084", "-colorspace", "bt2020nc", "-color_range", "tv",
   inputs=[["-loop", "1", "-framerate", "25", "-i", pq1k_png]])
os.remove(pq1k_png)

hlg_png = out("_hlg.png")
png16(hlg_png, W, H, patches(hlg, [1, 10, 50, 100, 203, 400, 600, 800, 1000]))
ff("hlg-patches.mkv", "-t", "5", "-vf", TO_YUV, "-c:v", "libx265", "-preset", "fast", "-crf", "12",
   "-x265-params", "colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc:range=limited:repeat-headers=1:log-level=error",
   "-color_primaries", "bt2020", "-color_trc", "arib-std-b67", "-colorspace", "bt2020nc", "-color_range", "tv",
   inputs=[["-loop", "1", "-framerate", "25", "-i", hlg_png]])
os.remove(hlg_png)

# SDR reference: Rec.709 patches (fractions of SDR white) for brightness comparisons.
sdr_png = out("_sdr.png")
png16(sdr_png, W, H, patches(lambda f: f, [0.05, 0.1, 0.2, 0.3, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0]))
ff("sdr-patches.mp4", "-t", "5", "-vf", "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p", *H264[:4],
   "-crf", "12", "-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709",
   inputs=[["-loop", "1", "-framerate", "25", "-i", sdr_png]])
os.remove(sdr_png)

# ---------------------------------------------------------------- audio
ff("tagged-cover.mp3", "-map", "0:a", "-map", "1:v", "-c:a", "libmp3lame", "-b:a", "192k", "-c:v", "copy", "-id3v2_version", "3",
   "-metadata:s:v", "title=Album cover", "-metadata:s:v", "comment=Cover (front)", *TAGS, inputs=[AUDIO44, COVER])
copy("tagged-cover.mp3", "tagged-cover.mpa")
copy("tagged-cover.mp3", "tagged-cover.mpga")
ff("layer2.mp2", "-c:a", "mp2", "-b:a", "192k", *TAGS, inputs=[AUDIO44])
ff("aac-cover.m4a", "-map", "0:a", "-map", "1:v", "-c:a", "aac", "-b:a", "160k", "-c:v", "copy", "-disposition:v",
   "attached_pic", *TAGS, inputs=[AUDIO44, COVER])
ff("alac.m4a", "-map", "0:a", "-c:a", "alac", *TAGS, inputs=[AUDIO44])
book = out("_book.txt")
with open(book, "w", encoding="utf-8") as f:
    f.write(";FFMETADATA1\ntitle=Sample Audiobook\nartist=Narrator Name\ngenre=Audiobook\n")
    for i in range(3):
        f.write(f"[CHAPTER]\nTIMEBASE=1/1000\nSTART={i * 3333}\nEND={(i + 1) * 3333}\ntitle=Chapter {i + 1}\n")
ff("audiobook.m4b", "-map", "0:a", "-map", "1:v", "-map_metadata", "2", "-map_chapters", "2", "-c:a", "aac", "-b:a", "64k",
   "-c:v", "copy", "-disposition:v", "attached_pic", "-f", "ipod", inputs=[AUDIO44, COVER, ["-i", book]])
os.remove(book)
ff("adts.aac", "-c:a", "aac", "-b:a", "128k", "-f", "adts", inputs=[AUDIO44])
ff("surround.ac3", "-c:a", "ac3", "-b:a", "448k", inputs=[AUDIO51])
ff("surround.dts", "-c:a", "dca", "-strict", "-2", "-b:a", "768k", inputs=[AUDIO51])
ff("hires-cover.flac", "-map", "0:a", "-map", "1:v", "-c:a", "flac", "-sample_fmt", "s32", "-ar", "96000", "-c:v", "copy",
   "-disposition:v", "attached_pic", *TAGS, inputs=[AUDIO44, COVER])
ff("opus-vorbis.mka", "-map", "0:a", "-map", "1:a", "-c:a:0", "libopus", "-c:a:1", "libvorbis", *TAGS, inputs=[AUDIO44, AUDIO])
ff("vorbis.ogg", "-c:a", "libvorbis", "-q:a", "5", *TAGS, inputs=[AUDIO44])
ff("pcm16.wav", "-c:a", "pcm_s16le", *TAGS, inputs=[AUDIO44])
ff("pcm24-51.wav", "-c:a", "pcm_s24le", inputs=[AUDIO51])
ff("pcm16.aiff", "-c:a", "pcm_s16be", *TAGS, inputs=[AUDIO44])
copy("pcm16.aiff", "pcm16.aif")
ff("float.aifc", "-c:a", "pcm_f32be", "-f", "aiff", inputs=[AUDIO44])
ff("sun.au", "-c:a", "pcm_s16be", "-f", "au", inputs=[AUDIO44])
ff("mulaw.snd", "-c:a", "pcm_mulaw", "-ar", "8000", "-ac", "1", "-f", "au", inputs=[AUDIO44])
ff("wavpack.wv", "-c:a", "wavpack", *TAGS, inputs=[AUDIO44])
ff("wma2.wma", "-c:a", "wmav2", "-b:a", "128k", "-f", "asf", *TAGS, inputs=[AUDIO44])
ff("pcm-s16le-44k-stereo.raw", "-c:a", "pcm_s16le", "-f", "s16le", inputs=[AUDIO44])

# ---------------------------------------------------------------- DRM look-alikes
# .m4p: an AAC m4a whose sample entry is 'drms' (FairPlay), as iTunes Store files were.
if os.path.exists(out("aac-cover.m4a")):
    data = bytearray(open(out("aac-cover.m4a"), "rb").read())
    i = data.find(b"stsd")
    j = data.find(b"mp4a", i)
    if i > 0 and j > 0:
        data[j:j + 4] = b"drms"
        open(out("protected.m4p"), "wb").write(data)
        print("ok   protected.m4p")
# .aa (Audible): size + magic 0x57907536 + a minimal TOC.
aa = bytearray(struct.pack(">II", 0, 0x57907536))
aa += struct.pack(">II", 1, 0) + b"\x00" * 64
aa[0:4] = struct.pack(">I", len(aa))
open(out("audible.aa"), "wb").write(aa)
print("ok   audible.aa")
# .tivo: TiVo container header in front of an MPEG-PS payload.
if os.path.exists(out("mpeg2-ps.mpg")):
    ps = open(out("mpeg2-ps.mpg"), "rb").read()
    hdr = b"TiVo" + struct.pack(">HHHIH", 4, 0x000D, 0, 0x1000, 0)
    hdr += b"\x00" * (0x1000 - len(hdr))
    open(out("recording.tivo"), "wb").write(hdr + ps)
    print("ok   recording.tivo")

# ---------------------------------------------------------------- CD audio track file
def cda(track, start_lba, length_sectors):
    def msf(lba):
        lba += 150
        return bytes([lba % 75, (lba // 75) % 60, lba // 4500, 0])
    fmt = struct.pack("<HHIII", 1, track, 0x1234ABCD, start_lba, length_sectors) + msf(start_lba) + msf(length_sectors - 150)
    body = b"CDDA" + b"fmt " + struct.pack("<I", len(fmt)) + fmt
    return b"RIFF" + struct.pack("<I", len(body)) + body


open(out("Track03.cda"), "wb").write(cda(3, 32150, 75 * 214))
print("ok   Track03.cda")


# ---------------------------------------------------------------- MIDI
def vlq(n):
    b = [n & 0x7F]
    n >>= 7
    while n:
        b.append((n & 0x7F) | 0x80)
        n >>= 7
    return bytes(reversed(b))


def track(events):
    data = b"".join(vlq(dt) + ev for dt, ev in events) + b"\x00\xFF\x2F\x00"
    return b"MTrk" + struct.pack(">I", len(data)) + data


TPQ = 480
tempo = track([(0, b"\xFF\x03" + bytes([len(b"Sample Song")]) + b"Sample Song"),
               (0, b"\xFF\x51\x03" + (500000).to_bytes(3, "big")),
               (0, b"\xFF\x58\x04\x04\x02\x18\x08")])
melody_notes = [60, 62, 64, 65, 67, 69, 71, 72, 71, 69, 67, 65, 64, 62, 60, 67]
mel = [(0, b"\xFF\x03\x05Piano"), (0, b"\xC0\x00")]
for n in melody_notes:
    mel += [(0, bytes([0x90, n, 96])), (TPQ, bytes([0x80, n, 0]))]
strings = [(0, b"\xFF\x03\x07Strings"), (0, b"\xC1\x30")]
for chord in ([48, 52, 55], [53, 57, 60], [55, 59, 62], [48, 52, 55]):
    strings += [(0, bytes([0x91, n, 70])) for n in chord]
    strings += [(TPQ * 4 if k == 0 else 0, bytes([0x81, n, 0])) for k, n in enumerate(chord)]
drums = [(0, b"\xFF\x03\x05Drums")]
for beat in range(16):
    drums += [(0, bytes([0x99, 36 if beat % 2 == 0 else 38, 100])), (0, bytes([0x99, 42, 70])),
              (TPQ, bytes([0x89, 36 if beat % 2 == 0 else 38, 0])), (0, bytes([0x89, 42, 0]))]
smf = b"MThd" + struct.pack(">IHHH", 6, 1, 4, TPQ) + tempo + track(mel) + track(strings) + track(drums)
open(out("song.mid"), "wb").write(smf)
open(out("song.midi"), "wb").write(smf)
rmid = b"RMID" + b"data" + struct.pack("<I", len(smf)) + smf
open(out("song.rmi"), "wb").write(b"RIFF" + struct.pack("<I", len(rmid)) + rmid)
shutil.copyfile(out("song.rmi"), out("song.rmid"))
shutil.copyfile(out("song.rmi"), out("song.rim"))
print("ok   song.mid / .midi / .rmi / .rmid / .rim")

# ---------------------------------------------------------------- playlists
audio_list = [("tagged-cover.mp3", "Gregarioustypes - Sample Tune", 10), ("hires-cover.flac", "Hi-res FLAC", 10),
              ("vorbis.ogg", "Ogg Vorbis", 10), ("missing-file.mp3", "A file that does not exist", 200)]
video_list = [("h264-aac.mp4", "H.264 sample", 6), ("multitrack.mkv", "Multi-track Matroska", 6), ("hdr10-pq-patches.mkv", "HDR10 patches", 5)]
with open(out("playlist.m3u"), "w", encoding="utf-8") as f:
    f.write("#EXTM3U\n")
    for p, t, d in audio_list + video_list:
        f.write(f"#EXTINF:{d},{t}\n{p}\n")
with open(out("unicode-playlist.m3u8"), "w", encoding="utf-8") as f:
    f.write("#EXTM3U\n#PLAYLIST:Écoute — 再生リスト\n#EXTINF:10,Bjørk - Jóga (sample)\ntagged-cover.mp3\nhttps://example.com/stream.mp3\n")
with open(out("playlist.pls"), "w", encoding="utf-8") as f:
    f.write("[playlist]\n")
    for i, (p, t, d) in enumerate(audio_list, 1):
        f.write(f"File{i}={p}\nTitle{i}={t}\nLength{i}={d}\n")
    f.write(f"NumberOfEntries={len(audio_list)}\nVersion=2\n")


def asx(entries):
    s = '<asx version="3.0">\n  <title>Sample ASX</title>\n'
    for p, t, _ in entries:
        s += f'  <entry>\n    <title>{t}</title>\n    <ref href="{p}"/>\n  </entry>\n'
    return s + "</asx>\n"


open(out("playlist.wvx"), "w", encoding="utf-8").write(asx(video_list))
open(out("playlist.wax"), "w", encoding="utf-8").write(asx(audio_list))
with open(out("playlist.ram"), "w", encoding="utf-8") as f:
    f.write("file:///" + out("realaudio.ra").replace("\\", "/") + "\n")
    f.write("rtsp://media.example.com/live/stream.rm\n")
with open(out("playlist.xpl"), "w", encoding="utf-8") as f:
    f.write('<?xml version="1.0" encoding="UTF-8"?>\n<playlist>\n')
    for p, t, _ in audio_list:
        f.write(f'  <item src="{p}" title="{t}"/>\n')
    f.write("</playlist>\n")
with open(out("playlist.jmx"), "w", encoding="utf-8") as f:
    for p, _, _ in audio_list:
        f.write(p + "\n")
with open(out("playlist.xspf"), "w", encoding="utf-8") as f:
    f.write('<?xml version="1.0" encoding="UTF-8"?>\n<playlist version="1" xmlns="http://xspf.org/ns/0/">\n<trackList>\n')
    for p, t, d in audio_list:
        f.write(f"<track><location>{p}</location><title>{t}</title><duration>{d * 1000}</duration></track>\n")
    f.write("</trackList>\n</playlist>\n")

for name in ("cover.jpg",):
    pass
print()
print(f"{len(failures)} failures")
for n, e in failures:
    print(" ", n, e)

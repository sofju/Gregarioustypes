"""Download real-world media samples that ffmpeg cannot generate (M3).

Each file goes into its own folder under samples/media/downloaded/<name>/.
Usage: python -I scripts/fetch-media-samples.py
"""

import os
import sys
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BASE = os.path.join(ROOT, "samples", "media", "downloaded")
S = "https://samples.ffmpeg.org/"

# (folder, url, file name, max bytes or None)
FILES = [
    ("ape", S + "A-codecs/lossless/luckynight.ape", "luckynight.ape", None),
    ("musepack", S + "A-codecs/musepack/sv8/sv8-tags_and_chaps.mpc", "sv8-tags_and_chaps.mpc", None),
    ("musepack-sv7", S + "A-codecs/musepack/03-Take%20A%20Chance%20On%20Me.mpc", "take-a-chance-sv7.mpc", None),
    ("realaudio-cook", S + "real/AC-cook/FUN_RM_64.rm", "FUN_RM_64.rm", None),
    ("realaudio8", S + "real/AC-cook/mc_sich_ra8_44.rm", "mc_sich_ra8_44.ra", None),
    ("wma-cover", S + "asf-wmv/wma_with_cover_art/wma_with_WmPicture.wma", "wma_with_WmPicture.wma", None),
    # Dolby Vision test streams from dovi_tool's test assets.
    ("dovi", "https://github.com/quietvoid/dovi_tool/raw/main/assets/hevc_tests/regular.mkv", "dovi-regular.mkv", None),
    ("dovi", "https://github.com/quietvoid/dovi_tool/raw/main/assets/hevc_tests/regular.hevc", "dovi-regular.hevc", None),
    # A real TiVo recording is 427 MB; its header and first chunks are enough.
    ("tivo", S + "TiVo/Twitches%20-%20%2020121016.TiVo", "Twitches-head.TiVo", 4 << 20),
]


def fetch(folder, url, name, limit):
    d = os.path.join(BASE, folder)
    os.makedirs(d, exist_ok=True)
    dst = os.path.join(d, name)
    if os.path.exists(dst) and os.path.getsize(dst) > 0:
        print("have", dst)
        return
    req = urllib.request.Request(url, headers={"User-Agent": "Gregarioustypes-samples"})
    if limit:
        req.add_header("Range", f"bytes=0-{limit - 1}")
    with urllib.request.urlopen(req, timeout=300) as r, open(dst + ".part", "wb") as f:
        n = 0
        while True:
            b = r.read(1 << 16)
            if not b:
                break
            f.write(b)
            n += len(b)
            if limit and n >= limit:
                break
    os.replace(dst + ".part", dst)
    print("got ", dst, os.path.getsize(dst))


for f in FILES:
    try:
        fetch(*f)
    except Exception as e:  # keep going; report at the end
        print("FAIL", f[1], e, file=sys.stderr)

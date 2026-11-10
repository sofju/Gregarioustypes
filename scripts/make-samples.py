"""Generate synthetic sample files for formats/edge cases that are awkward to download.
Run:  python -I scripts/make-samples.py
Writes into samples/<category>/.
"""
import hashlib
import os
import struct

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "samples")


def out(cat, name, data):
    d = os.path.join(ROOT, cat)
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, name), "wb") as f:
        f.write(data)
    print("wrote", os.path.join(cat, name), len(data))


# ---------------------------------------------------------------- bencode

def benc(v):
    if isinstance(v, int):
        return b"i%de" % v
    if isinstance(v, str):
        v = v.encode("utf-8")
    if isinstance(v, bytes):
        return b"%d:%s" % (len(v), v)
    if isinstance(v, list):
        return b"l" + b"".join(benc(x) for x in v) + b"e"
    if isinstance(v, dict):
        items = sorted((k.encode() if isinstance(k, str) else k, x) for k, x in v.items())
        return b"d" + b"".join(benc(k) + benc(x) for k, x in items) + b"e"
    raise TypeError(type(v))


def torrents():
    piece = 256 * 1024
    files = [
        (["Season 01", "Episode 01 - Pilot.mkv"], 734003200),
        (["Season 01", "Episode 02 - Café Nights.mkv"], 701038592),
        (["Season 01", "Subs", "Episode 01.fr.srt"], 48211),
        (["Season 01", "Subs", "Episode 02.fr.srt"], 51200),
        (["Extras", "Behind the scenes.mp4"], 210763776),
        ([".pad", "123"], 123),
        (["README.nfo"], 4096),
        (["日本語のファイル.txt"], 1024),
    ]
    total = sum(s for _, s in files)
    npieces = (total + piece - 1) // piece
    info = {
        "name": "Demo Show (2026) [Gregarioustypes sample]",
        "piece length": piece,
        "pieces": b"\x01" * (20 * npieces),
        "files": [
            dict({"length": s, "path": p}, **({"attr": "p"} if p[0] == ".pad" else {})) for p, s in files
        ],
        "private": 1,
        "source": "GT",
    }
    t = {
        "announce": "https://tracker.example.org/announce/abcdef0123456789",
        "announce-list": [
            ["https://tracker.example.org/announce/abcdef0123456789"],
            ["udp://tracker.opentrackr.example:1337/announce", "udp://open.example.net:6969/announce"],
            ["wss://tracker.webtorrent.example"],
        ],
        "url-list": ["https://seed.example.org/files/", "https://mirror.example.com/demo/"],
        "comment": "Synthetic multi-file torrent with tiers, web seeds and a padding file.",
        "created by": "Gregarioustypes make-samples.py",
        "creation date": 1793750400,
    }
    t["info"] = info
    out("torrent", "multi-file-private.torrent", benc(t))

    # v2-only (BEP 52) with a file tree and DHT nodes.
    tree = {
        "docs": {"manual.pdf": {"": {"length": 3145728, "pieces root": b"\x02" * 32}}},
        "setup.exe": {"": {"length": 52428800, "pieces root": b"\x03" * 32}},
    }
    info2 = {"name": "Installer v2", "piece length": 1 << 20, "meta version": 2, "file tree": tree}
    t2 = {"info": info2, "nodes": [["router.bittorrent.example", 6881], ["10.0.0.5", 51413]], "creation date": 1767225600}
    out("torrent", "v2-only.torrent", benc(t2))


# ---------------------------------------------------------------- NFO / ANSI

def sauce(title, author, group, date, dtype, ftype, w, h, flags, font, comments=()):
    r = bytearray(b" " * 128)
    r[0:7] = b"SAUCE00"
    r[7:7 + len(title)] = title.encode("cp437")
    r[42:42 + len(author)] = author.encode("cp437")
    r[62:62 + len(group)] = group.encode("cp437")
    r[82:90] = date.encode()
    r[90:94] = struct.pack("<I", 0)
    r[94] = dtype
    r[95] = ftype
    r[96:98] = struct.pack("<H", w)
    r[98:100] = struct.pack("<H", h)
    r[100:104] = b"\0\0\0\0"
    r[104] = len(comments)
    r[105] = flags
    f = font.encode().ljust(22, b"\0")
    r[106:128] = f
    block = b""
    if comments:
        block = b"COMNT" + b"".join(c.encode("cp437").ljust(64, b" ") for c in comments)
    return b"\x1a" + block + bytes(r)


def nfo():
    lines = [
        "\xda" + "\xc4" * 76 + "\xbf",
        "\xb3" + " " * 76 + "\xb3",
        "\xb3   \xdb\xdb\xdb\xdb\xdb\xdb\xdb  \xdb\xdb\xdb\xdb\xdb\xdb\xdb   \xb0\xb1\xb2\xdb GREGARIOUSTYPES \xdb\xb2\xb1\xb0   \xdc\xdc\xdc\xdc\xdc  \xdf\xdf\xdf\xdf\xdf  \xdd\xde      \xb3",
        "\xb3   \xdb\xdb       \xdb\xdb   \xdb\xdb   \xb2\xb2\xb2\xb2\xb2\xb2\xb2\xb2\xb2\xb2\xb2\xb2\xb2\xb2\xb2\xb2\xb2\xb2\xb2\xb2\xb2\xb2\xb2  \xdb\xdb\xdb\xdb\xdb  \xdb\xdb\xdb\xdb\xdb  \xdd\xde      \xb3",
        "\xb3   \xdb\xdb  \xdb\xdb\xdb\xdb   \xdb\xdb   \xb1\xb1\xb1\xb1\xb1\xb1\xb1\xb1\xb1\xb1\xb1\xb1\xb1\xb1\xb1\xb1\xb1\xb1\xb1\xb1\xb1\xb1\xb1                     \xb3",
        "\xb3   \xdb\xdb\xdb\xdb\xdb\xdb\xdb   \xdb\xdb   \xb0\xb0\xb0\xb0\xb0\xb0\xb0\xb0\xb0\xb0\xb0\xb0\xb0\xb0\xb0\xb0\xb0\xb0\xb0\xb0\xb0\xb0\xb0                     \xb3",
        "\xc3" + "\xc4" * 76 + "\xb4",
        "\xb3  Release ...: Demo.Show.2026.1080p.WEB                                    \xb3",
        "\xb3  Size ......: 1.56 GB      Genre ...: Documentaire  \x03 \x04 \x05 \x06 \x0e \x10 \x11    \xb3",
        "\xb3  Notes .....: Caf\x82, na\x8bve, \x9c12 \xab \xac \xf8 \xe0\xe1\xe2\xe3 \xfb\xfd                            \xb3",
        "\xc8" + "\xcd" * 30 + "\xcb" + "\xcd" * 14 + "\xcb" + "\xcd" * 30 + "\xbc",
        "                               \xba  9px-cells   \xba",
        "                               \xc8\xcd\xcd\xcd\xcd\xcd\xcd\xcd\xcd\xcd\xcd\xcd\xcd\xcd\xcd\xbc",
    ]
    data = "\r\n".join(lines).encode("latin-1") + b"\r\n"
    out("nfo", "cp437-art.nfo", data)
    out("nfo", "cp437-art-sauce.nfo", data + sauce("Gregarioustypes demo", "Claude", "GT", "20261108", 1, 0, 80, len(lines), 0b0101, "IBM VGA", ["Synthetic NFO with SAUCE (9px, iCE)"]))

    # ANSI with colours, cursor movement, SAUCE iCE colours and 80 columns.
    esc = "\x1b["
    ans = ""
    ans += esc + "2J" + esc + "1;1H"
    for i, c in enumerate([31, 33, 32, 36, 34, 35]):
        ans += esc + "0;%d;%dm" % (c, 40 + (i % 8)) + "\xdb\xdb\xb2\xb2\xb1\xb1\xb0\xb0" + esc + "1;%dm" % c + "\xdb\xdb\xb2\xb2\xb1\xb1\xb0\xb0"
    ans += esc + "0m\r\n"
    ans += esc + "1;37;44m" + "  GREGARIOUSTYPES ANSI TEST  " + esc + "0m" + esc + "5;41m" + " iCE " + esc + "0m\r\n"
    ans += esc + "3;10H" + esc + "1;33m" + "\xc9" + "\xcd" * 20 + "\xbb" + esc + "4;10H" + "\xba" + esc + "0;36m" + "  cursor moved here " + esc + "1;33m\xba" + esc + "5;10H" + "\xc8" + "\xcd" * 20 + "\xbc" + esc + "0m\r\n"
    out("nfo", "colors.ans", ans.encode("latin-1") + sauce("ANSI test", "Claude", "GT", "20261108", 1, 1, 80, 5, 0b0011, "IBM VGA"))

    # UTF-8 NFO with box drawing (should be detected as UTF-8).
    u = "╔══════════════════╗\r\n║  UTF-8 NFO ░▒▓█  ║\r\n║  Café — naïve    ║\r\n╚══════════════════╝\r\n"
    out("nfo", "utf8-box.nfo", u.encode("utf-8"))
    # FILE_ID.DIZ
    out("nfo", "FILE_ID.DIZ", "\xfe GREGARIOUSTYPES v0.1 \xfe\r\n Space to preview\r\n \xb0\xb1\xb2\xdb\xdb\xb2\xb1\xb0\r\n".encode("latin-1"))


def text():
    body = "Gregarioustypes text sample.\r\nCafé, naïve, déjà vu — “quotes” and emoji 🎉.\r\n\tTabbed line\r\n" + "A very long line " * 40 + "\r\n"
    out("text", "utf8.txt", body.encode("utf-8"))
    out("text", "utf8-bom.txt", b"\xef\xbb\xbf" + body.encode("utf-8"))
    out("text", "utf16le-bom.txt", b"\xff\xfe" + body.encode("utf-16-le"))
    out("text", "windows1252.txt", "Café, naïve, déjà vu — “quotes” €\r\n".encode("cp1252"))
    code = "fn main() {\n    // Rust source\n    let x = 42;\n    println!(\"{x}\");\n}\n" * 30
    out("text", "code.rs", code.encode())
    out("text", "typescript.ts", b"export const answer: number = 42;\nconsole.log(answer);\n")
    out("text", "big.log", ("".join("2026-11-08 12:00:%02d INFO line %d of a big log file\n" % (i % 60, i) for i in range(200000))).encode())
    out("text", "binary.bin", bytes(range(256)) * 64)


if __name__ == "__main__":
    torrents()
    nfo()
    text()

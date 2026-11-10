"""Collect the licence texts the installer ships in {app}\\licenses:

  LICENSE.txt                 Gregarioustypes (GNU GPL v3 or later; also libmpv's licence)
  THIRD-PARTY-NOTICES.md      what is bundled, under which licence, and where its source is
  components\\                 the programs and libraries in deps\\ (7-Zip, Ghostscript...)
  native\\                     C/C++ libraries linked into Gregarioustypes.exe (vcpkg's copyright files)
  rust\\                       every Rust crate compiled into the app
  syntaxes.md                 the syntax definitions and themes used for code colouring

Usage:  python scripts/collect-licenses.py      (writes target/licenses; run by package.ps1)
"""

import json
import pathlib
import re
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
DEPS = ROOT / "deps"
OUT = ROOT / "target" / "licenses"
VCPKG = ROOT / "vcpkg_installed" / "x64-windows-static"

# deps\ file -> name in components\ (files that the installer doesn't already
# place next to the component, or that sit in a folder it leaves out).
COMPONENTS = [
    ("7zip/License.txt", "7-Zip.txt"),
    ("7zip-lgpl/License.txt", "7-Zip-library.txt"),
    ("7zip-lgpl/README.txt", "7-Zip-library-build.txt"),
    ("mediainfo/Developers/License.html", "MediaInfo.html"),
    ("pdfium/LICENSE", "PDFium.txt"),
    ("ghostscript/doc/COPYING", "Ghostscript-AGPL-3.0.txt"),
    ("libreoffice/license.txt", "LibreOffice.txt"),
    ("blender/license/license.md", "Blender.md"),
    ("three/LICENSE", "three.js.txt"),
    ("ruffle/LICENSE_MIT", "Ruffle-MIT.txt"),
    ("ruffle/LICENSE_APACHE", "Ruffle-Apache-2.0.txt"),
    ("soundfont/LICENSE.txt", "GeneralUser-GS.txt"),
    ("fonts/LICENSE.TXT", "Oldschool-PC-Fonts-CC-BY-SA-4.0.txt"),
]

MPV_NOTICE = """libmpv (deps\\mpv\\libmpv-2.dll)

mpv and the FFmpeg build inside it (configured with --enable-gpl --enable-version3)
are distributed under the GNU General Public License, version 3 or later: see
LICENSE.txt in this folder. Some parts are under the LGPL or more permissive licences.

Build: https://github.com/shinchiro/mpv-winbuild-cmake/releases/tag/20261107
mpv source (commit 9028187538): https://github.com/mpv-player/mpv
Build scripts, with every library and its version: https://github.com/shinchiro/mpv-winbuild-cmake
"""

LICENSE_FILE = re.compile(r"^(licen[cs]e|copying|notice|unlicense|copyright|authors)([-_.].*)?$", re.I)


def copy_components(missing):
    d = OUT / "components"
    d.mkdir(parents=True)
    for src, name in COMPONENTS:
        p = DEPS / src
        if p.is_file():
            shutil.copyfile(p, d / name)
        else:
            missing.append(str(p))
    (d / "mpv.txt").write_text(MPV_NOTICE, encoding="utf-8")


def copy_native(missing):
    d = OUT / "native"
    d.mkdir(parents=True)
    share = VCPKG / "share"
    if not share.is_dir():
        missing.append(str(share))
        return
    for port in sorted(share.iterdir()):
        c = port / "copyright"
        if c.is_file():
            shutil.copyfile(c, d / f"{port.name}.txt")


def rust_crates():
    """Packages compiled into the app and the dialog hook (normal dependencies)."""
    meta = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", "x86_64-pc-windows-msvc"],
            cwd=ROOT,
        )
    )
    pkgs = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    roots = [i for i in meta["workspace_members"] if pkgs[i]["name"] in ("gt-app", "gt-dialoghook")]
    seen, stack = set(), list(roots)
    while stack:
        i = stack.pop()
        if i in seen:
            continue
        seen.add(i)
        for dep in nodes[i]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    ws = set(meta["workspace_members"])
    return sorted((pkgs[i] for i in seen if i not in ws), key=lambda p: (p["name"], p["version"]))


def copy_rust():
    d = OUT / "rust"
    d.mkdir(parents=True)
    crates = rust_crates()
    for p in crates:
        root = pathlib.Path(p["manifest_path"]).parent
        texts = []
        for f in sorted(root.iterdir()):
            if f.is_file() and LICENSE_FILE.match(f.name):
                texts.append(f"----- {f.name} -----\n\n" + f.read_text(encoding="utf-8", errors="replace"))
        head = f"{p['name']} {p['version']}\nLicence: {p.get('license') or 'see below'}\n"
        if p.get("repository"):
            head += f"Source: {p['repository']}\n"
        if not texts:
            head += "\n(The published crate has no licence file; its licence is the one named above.)\n"
        (d / f"{p['name']}-{p['version']}.txt").write_text(head + "\n" + "\n\n".join(texts), encoding="utf-8")
    return len(crates)


def syntaxes():
    md = subprocess.check_output(["cargo", "run", "-q", "--locked", "-p", "gt-fmt-misc", "--example", "syntax-licenses"], cwd=ROOT)
    (OUT / "syntaxes.md").write_bytes(md)


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)
    shutil.copyfile(ROOT / "LICENSE", OUT / "LICENSE.txt")
    shutil.copyfile(ROOT / "THIRD-PARTY-NOTICES.md", OUT / "THIRD-PARTY-NOTICES.md")
    missing = []
    copy_components(missing)
    copy_native(missing)
    n = copy_rust()
    syntaxes()
    if missing:
        sys.exit("missing licence files (run scripts\\fetch-deps.ps1?):\n  " + "\n  ".join(missing))
    print(f"licenses: {len(COMPONENTS) + 1} component files, {len(list((OUT / 'native').iterdir()))} native libraries, {n} Rust crates -> {OUT}")


if __name__ == "__main__":
    main()

# Third-party notices

Gregarioustypes is free software under the GNU General Public License, version 3
or later ([LICENSE](LICENSE)). It stands on the shoulders of the projects below,
each under its own licence. The installer places all of their licence texts in
the `licenses` folder of the installation (gathered by
[`scripts/collect-licenses.py`](scripts/collect-licenses.py)).

None of these components is in this repository: [`scripts/fetch-deps.ps1`](scripts/fetch-deps.ps1)
downloads them from the links below, checks them against the SHA-256 hashes in
[`scripts/deps.lock`](scripts/deps.lock), and builds the 7-Zip library from source.

## Bundled programs and libraries

| Component | Version | Licence | How it is used | Source code |
|---|---|---|---|---|
| mpv and FFmpeg (`libmpv-2.dll`, shinchiro's build) | 2026-11-07, mpv commit 9028187538 | GPL-3.0-or-later (FFmpeg built with `--enable-gpl --enable-version3`) | Library loaded by the app: video and audio | [build](https://github.com/shinchiro/mpv-winbuild-cmake/releases/tag/20261107), [build scripts](https://github.com/shinchiro/mpv-winbuild-cmake), [mpv](https://github.com/mpv-player/mpv) |
| 7-Zip library (`deps\7zip-lgpl\7z.dll`) | 26.06 | LGPL-2.1-or-later, with BSD-3-Clause and BSD-2-Clause parts | Library loaded by the app: archives and disc images | [7z2606-src.7z](https://github.com/ip7z/7zip/releases/tag/26.06). Built by `fetch-deps.ps1` **without the RAR decoders** (`CPP/7zip/Compress/Rar*`), the only 7-Zip files under the unRAR licence restriction |
| 7-Zip program (`deps\7zip\7z.exe` and its `7z.dll`) | 26.06 | LGPL-2.1-or-later; its RAR decoders carry the unRAR licence restriction | Separate program: extracts files from RAR archives | [7-Zip 26.06](https://github.com/ip7z/7zip/releases/tag/26.06) |
| MediaInfo library | 26.10 | BSD-2-Clause | Library loaded by the app: media reports | [MediaInfoLib](https://github.com/MediaArea/MediaInfoLib) |
| PDFium (bblanchon's build) | Chromium 8138 | BSD-3-Clause and Apache-2.0 | Library loaded by the app: PDF and Illustrator files | [pdfium-binaries](https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium%2F8138), [PDFium](https://pdfium.googlesource.com/pdfium/) |
| Ghostscript (optional component) | 10.08.0 | AGPL-3.0-or-later | Separate program: PostScript and EPS | [ghostpdl-downloads gs10080](https://github.com/ArtifexSoftware/ghostpdl-downloads/releases/tag/gs10080) |
| LibreOffice (optional component) | 26.8.2 | MPL-2.0 (some parts under other free licences) | Separate program: Word, PowerPoint and OpenDocument files | [LibreOffice 26.8.2 source](https://download.documentfoundation.org/libreoffice/src/26.8.2/) |
| Blender (optional component) | 5.2.3 LTS | GPL-3.0-or-later | Separate program: `.blend` files and some USD files | [Blender source](https://download.blender.org/source/) |
| three.js | r187 | MIT | In the app's web view: 3D models | [three.js](https://github.com/mrdoob/three.js) |
| Ruffle (web build) | nightly 2026-11-08 | MIT or Apache-2.0 | In the app's web view: Flash files | [Ruffle](https://github.com/ruffle-rs/ruffle/releases/tag/nightly-2026-11-08) |
| GeneralUser GS SoundFont | 2.0.3 | GeneralUser GS License v2.0 | Instrument sounds for MIDI files | [GeneralUser GS](https://github.com/mrbumpy409/GeneralUser-GS) |
| The Ultimate Oldschool PC Font Pack, by VileR | 2.2 | CC BY-SA 4.0 | IBM VGA and BIOS glyphs for NFO and DOS art | [int10h.org](https://int10h.org/oldschool-pc-fonts/) |

This product uses MediaInfo library, Copyright (c) 2002-2025 [MediaArea.net SARL](https://mediaarea.net/).

The VGA and BIOS glyph tables in `crates/gt-fmt-misc/assets/*.bin` are adapted from
The Ultimate Oldschool PC Font Pack by VileR ([int10h.org](https://int10h.org/oldschool-pc-fonts/)):
they were converted from its bitmap fonts by [`scripts/gen-vga-font.py`](scripts/gen-vga-font.py).
They are shared under the same licence, [CC BY-SA 4.0](crates/gt-fmt-misc/assets/OLDSCHOOL-FONTS-LICENSE.txt).

The installer contains these components unchanged, except the 7-Zip library
described above. If one of the source links stops working, open an issue on
[the repository](https://github.com/sofju/Gregarioustypes/issues) and the
corresponding source will be provided.

Microsoft Edge WebView2, used for web pages, Flash, 3D models and the settings
window, is part of Windows and isn't bundled.

## Libraries compiled into Gregarioustypes.exe

Built with [vcpkg](https://github.com/microsoft/vcpkg) (baseline `6fdd23fa0a`,
triplet `x64-windows-static`) and linked statically.

| Library | Version | Licence |
|---|---|---|
| Alembic | 1.8.12 | BSD-3-Clause |
| Brotli | 1.2.1 | MIT |
| dav1d | 1.5.4 | BSD-2-Clause |
| Highway | 1.4.0 | Apache-2.0 or BSD-3-Clause |
| Imath | 3.2.3 | BSD-3-Clause |
| JasPer | 4.2.9 | JasPer License 2.0 |
| Little CMS | 2.19.1 | MIT |
| libavif | 1.4.2 | BSD-2-Clause |
| libde265 | 1.1.3 | LGPL-3.0-or-later |
| libdeflate | 1.26 | MIT |
| libheif | 1.23.7 | LGPL-3.0-or-later |
| libjpeg-turbo | 3.2.1 | IJG, BSD-3-Clause and Zlib |
| libjxl | 0.12.0 | BSD-3-Clause |
| liblzma (XZ Utils) | 5.8.4 | 0BSD |
| LibRaw | 0.22.3 | LGPL-2.1 or CDDL-1.0 |
| libspng | 0.7.4 | BSD-2-Clause |
| libwebp | 1.6.0 | BSD-3-Clause |
| libyuv | 1916 | BSD-3-Clause |
| OpenEXR | 3.5.3 | BSD-3-Clause |
| OpenJPEG | 2.5.4 | BSD-2-Clause |
| OpenJPH | 0.32.0 | BSD-2-Clause |
| LibTIFF | 4.7.2 | libtiff licence |
| zlib | 1.3.2 | Zlib |
| Zstandard | 1.5.7 | BSD-3-Clause or GPL-2.0 |

libheif, libde265 and LibRaw are under the LGPL and linked statically. The
complete source of Gregarioustypes and its build scripts are in this repository,
so the app can be rebuilt against modified versions of them, as the LGPL requires.

## Rust crates

About 200 crates from [crates.io](https://crates.io), almost all under MIT,
Apache-2.0, BSD, Zlib or Unlicense terms. [bdinfo-rs](https://github.com/agentjp/bdinfo-rs),
which writes the Blu-ray reports, is under LGPL-2.1-or-later. The installer's
`licenses\rust` folder holds the licence of every crate compiled into the app.

## Syntax definitions and themes

Code colouring uses the syntax definitions and themes collected by
[bat](https://github.com/sharkdp/bat) (through [two-face](https://github.com/CosmicHorrorDev/two-face)
and [syntect](https://github.com/trishume/syntect)), each under its own licence,
listed in the installer's `licenses\syntaxes.md`.

---

Quick Look and macOS are trademarks of Apple Inc. Gregarioustypes is an
independent project, not affiliated with or endorsed by Apple.
